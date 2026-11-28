// SPDX-License-Identifier: GPL-2.0
/* FTTR logical bridge ports -> lower-device flow block. No private PPE API. */
#include <linux/etherdevice.h>
#include <linux/if_vlan.h>
#include <linux/if_bridge.h>
#include <linux/in6.h>
#include <linux/rtnetlink.h>
#include <linux/xarray.h>
#include <net/flow_offload.h>
#include <net/pkt_cls.h>
#include "fmcs.h"

static bool fmcs_bridge_debug;
module_param_named(bridge_offload_debug, fmcs_bridge_debug, bool, 0644);
MODULE_PARM_DESC(bridge_offload_debug, "Rate-limited bridge TC adapter diagnostics");

static inline bool fmcs_ipv6_addr_all_ones(const struct in6_addr *addr)
{
	return (addr->s6_addr32[0] == U32_MAX &&
		addr->s6_addr32[1] == U32_MAX &&
		addr->s6_addr32[2] == U32_MAX &&
		addr->s6_addr32[3] == U32_MAX);
}

struct fmcs_bridge_offload {
	struct fmcs_priv *priv;
	struct mutex lock;
	struct flow_block lower_block;
	struct flow_block_cb *lower_cb;
	struct net_device *lower;
	struct list_head driver_blocks;
	/* Only cookies successfully submitted by this adapter may be destroyed. */
	struct xarray cookies;
};

struct fmcs_bridge_key {
	struct flow_dissector_key_meta meta;
	struct flow_dissector_key_control control;
	struct flow_dissector_key_basic basic;
	struct flow_dissector_key_ipv4_addrs ipv4;
	struct flow_dissector_key_ipv6_addrs ipv6;
	struct flow_dissector_key_ip ip;
	struct flow_dissector_key_tcp tcp;
	struct flow_dissector_key_ports ports;
	struct flow_dissector_key_vlan vlan;
};

static int fmcs_bridge_port(struct fmcs_priv *priv, int ifindex)
{
	int i;

	for (i = 0; i < FMCS_MAX_FTTR_PORTS; i++)
		if (priv->fttr_devs[i] && priv->fttr_devs[i]->ifindex == ifindex)
			return i + 1;
	return 0;
}

/* Rebuild, rather than mutate, the caller's match and action storage. */
static int fmcs_bridge_replace(struct fmcs_bridge_offload *b,
			       struct flow_cls_offload *f)
{
	struct flow_rule *rule = f->rule, *copy;
	struct flow_dissector dissector = {};
	struct fmcs_bridge_key key = {}, mask = {};
	struct flow_cls_offload cls = *f;
	struct flow_match_meta meta;
	struct net_device *lower, *out = NULL, *in, *master;
	struct flow_action_entry *act;
	unsigned long allowed;
	bool is_ipv4, is_ipv6;
	int i, src, dst, ret;

	allowed = BIT_ULL(FLOW_DISSECTOR_KEY_META) |
		  BIT_ULL(FLOW_DISSECTOR_KEY_CONTROL) |
		  BIT_ULL(FLOW_DISSECTOR_KEY_BASIC) |
		  BIT_ULL(FLOW_DISSECTOR_KEY_IPV4_ADDRS) |
		  BIT_ULL(FLOW_DISSECTOR_KEY_IPV6_ADDRS) |
		  BIT_ULL(FLOW_DISSECTOR_KEY_PORTS) |
		  BIT_ULL(FLOW_DISSECTOR_KEY_IP) |
		  BIT_ULL(FLOW_DISSECTOR_KEY_TCP);
	is_ipv4 = flow_rule_match_key(rule, FLOW_DISSECTOR_KEY_IPV4_ADDRS);
	is_ipv6 = flow_rule_match_key(rule, FLOW_DISSECTOR_KEY_IPV6_ADDRS);
	if (rule->match.dissector->used_keys & ~allowed ||
	    !flow_rule_match_key(rule, FLOW_DISSECTOR_KEY_META) ||
	    !flow_rule_match_key(rule, FLOW_DISSECTOR_KEY_CONTROL) ||
	    !flow_rule_match_key(rule, FLOW_DISSECTOR_KEY_BASIC) ||
	    !(is_ipv4 ^ is_ipv6) ||
	    !flow_rule_match_key(rule, FLOW_DISSECTOR_KEY_PORTS))
		return -EOPNOTSUPP;
	flow_rule_match_meta(rule, &meta);
	if (meta.mask->ingress_ifindex != U32_MAX)
		return -EOPNOTSUPP;
	src = fmcs_bridge_port(b->priv, meta.key->ingress_ifindex);
	if (!src || !fmcs_bridge_port_enabled(b->priv, src))
		return -EOPNOTSUPP;
	flow_action_for_each(i, act, &rule->action) {
		switch (act->id) {
		case FLOW_ACTION_REDIRECT:
			if (out)
				return -EOPNOTSUPP;
			out = act->dev;
			break;
		case FLOW_ACTION_MANGLE:
			if (act->mangle.htype != FLOW_ACT_MANGLE_HDR_TYPE_ETH)
				return -EOPNOTSUPP;
			break;
		case FLOW_ACTION_CSUM:
			break;
		default:
			return -EOPNOTSUPP;
		}
	}
	if (!out)
		return -EOPNOTSUPP;
	dst = fmcs_bridge_port(b->priv, out->ifindex);
	if (!dst || dst == src || !fmcs_bridge_pair_enabled(b->priv, src, dst))
		return -EOPNOTSUPP;
	in = b->priv->fttr_devs[src - 1];
	rcu_read_lock();
	master = netdev_master_upper_dev_get_rcu(in);
	ret = !master || !netif_is_bridge_master(master) ||
	      master != netdev_master_upper_dev_get_rcu(out) || br_vlan_enabled(master);
	lower = rcu_dereference(b->priv->fttr_lower);
	rcu_read_unlock();
	if (ret || !lower || lower != b->lower || !b->lower_cb || !netif_running(lower) ||
	    !netif_carrier_ok(lower) || !tc_can_offload(lower))
		return -EOPNOTSUPP;
	if (xa_load(&b->cookies, f->cookie))
		return 0;

#define FMCS_COPY_KEY(ID, name, type) do { \
	if (flow_rule_match_key(rule, FLOW_DISSECTOR_KEY_##ID)) { \
		struct flow_match_##type match; \
		flow_rule_match_##type(rule, &match); \
		key.name = *match.key; \
		mask.name = *match.mask; \
		dissector.used_keys |= BIT_ULL(FLOW_DISSECTOR_KEY_##ID); \
		dissector.offset[FLOW_DISSECTOR_KEY_##ID] = \
			offsetof(struct fmcs_bridge_key, name); \
	} \
} while (0)
	FMCS_COPY_KEY(META, meta, meta);
	FMCS_COPY_KEY(CONTROL, control, control);
	FMCS_COPY_KEY(BASIC, basic, basic);
	FMCS_COPY_KEY(IPV4_ADDRS, ipv4, ipv4_addrs);
	FMCS_COPY_KEY(IPV6_ADDRS, ipv6, ipv6_addrs);
	FMCS_COPY_KEY(IP, ip, ip);
	FMCS_COPY_KEY(TCP, tcp, tcp);
	FMCS_COPY_KEY(PORTS, ports, ports);
#undef FMCS_COPY_KEY
	if (mask.control.addr_type != U16_MAX ||
	    mask.basic.n_proto != htons(0xffff) || mask.basic.ip_proto != U8_MAX ||
	    mask.ports.src != htons(0xffff) || mask.ports.dst != htons(0xffff) ||
	    (key.basic.ip_proto != IPPROTO_TCP && key.basic.ip_proto != IPPROTO_UDP))
		return -EOPNOTSUPP;
	if (is_ipv4) {
		if (key.control.addr_type != FLOW_DISSECTOR_KEY_IPV4_ADDRS ||
		    key.basic.n_proto != htons(ETH_P_IP) ||
		    mask.ipv4.src != htonl(U32_MAX) || mask.ipv4.dst != htonl(U32_MAX))
			return -EOPNOTSUPP;
	} else {
		if (key.control.addr_type != FLOW_DISSECTOR_KEY_IPV6_ADDRS ||
		    key.basic.n_proto != htons(ETH_P_IPV6) ||
		    !fmcs_ipv6_addr_all_ones(&mask.ipv6.src) ||
		    !fmcs_ipv6_addr_all_ones(&mask.ipv6.dst))
			return -EOPNOTSUPP;
	}
	key.meta.ingress_ifindex = lower->ifindex;
	key.vlan.vlan_id = FMCS_PORT_TO_FPGA_VID(src);
	key.vlan.vlan_tpid = htons(ETH_P_8021Q);
	mask.vlan.vlan_id = VLAN_VID_MASK;
	mask.vlan.vlan_priority = 7;
	mask.vlan.vlan_dei = 1;
	mask.vlan.vlan_tpid = htons(0xffff);
	dissector.used_keys |= BIT_ULL(FLOW_DISSECTOR_KEY_VLAN);
	dissector.offset[FLOW_DISSECTOR_KEY_VLAN] =
		offsetof(struct fmcs_bridge_key, vlan);
	copy = flow_rule_alloc(rule->action.num_entries + 2);
	if (!copy)
		return -ENOMEM;
	copy->match.dissector = &dissector;
	copy->match.key = &key;
	copy->match.mask = &mask;
	copy->action.entries[0].id = FLOW_ACTION_VLAN_POP;
	copy->action.entries[1].id = FLOW_ACTION_VLAN_PUSH;
	copy->action.entries[1].vlan.vid = FMCS_PORT_TO_FPGA_VID(dst);
	copy->action.entries[1].vlan.proto = htons(ETH_P_8021Q);
	memcpy(&copy->action.entries[2], rule->action.entries,
	       sizeof(*act) * rule->action.num_entries);
	flow_action_for_each(i, act, &copy->action)
		if (act->id == FLOW_ACTION_REDIRECT)
			act->dev = lower;
	/* Reserve ownership before the lower creates a hardware rule. */
	ret = xa_err(xa_store(&b->cookies, f->cookie, xa_mk_value(1), GFP_KERNEL));
	if (!ret) {
		cls.rule = copy;
		ret = b->lower_cb->cb(TC_SETUP_CLSFLOWER, &cls, b->lower_cb->cb_priv);
		if (ret)
			xa_erase(&b->cookies, f->cookie);
	}
	kfree(copy);
	return ret;
}

static int fmcs_bridge_cb(enum tc_setup_type type, void *data, void *cb_priv)
{
	struct fmcs_bridge_offload *b = cb_priv;
	struct flow_cls_offload *f = data;
	int ret = -EOPNOTSUPP;

	if (type != TC_SETUP_CLSFLOWER)
		return ret;
	mutex_lock(&b->lock);
	if (!b->lower_cb)
		goto unlock;
	if (f->command == FLOW_CLS_REPLACE) {
		ret = fmcs_bridge_replace(b, f);
	} else if ((f->command == FLOW_CLS_DESTROY || f->command == FLOW_CLS_STATS) &&
		   xa_load(&b->cookies, f->cookie)) {
		ret = b->lower_cb->cb(type, data, b->lower_cb->cb_priv);
		if (f->command == FLOW_CLS_DESTROY)
			xa_erase(&b->cookies, f->cookie);
	}
unlock:
	if (READ_ONCE(fmcs_bridge_debug))
		pr_info_ratelimited("fmcs bridge: cmd=%u cookie=%lx ret=%d lower_bound=%u\n",
				    f->command, f->cookie, ret, !!b->lower_cb);
	mutex_unlock(&b->lock);
	return ret;
}

static int fmcs_bridge_lower_bind(struct fmcs_bridge_offload *b)
{
	struct net_device *lower;
	struct flow_block_offload bo = {};
	int ret;

	if (b->lower_cb)
		return 0;
	rcu_read_lock();
	lower = rcu_dereference(b->priv->fttr_lower);
	if (lower)
		dev_hold(lower);
	rcu_read_unlock();
	if (!lower)
		return -EOPNOTSUPP;
	if (!lower->netdev_ops->ndo_setup_tc) {
		dev_put(lower);
		return -EOPNOTSUPP;
	}
	bo.net = dev_net(lower);
	bo.command = FLOW_BLOCK_BIND;
	bo.binder_type = FLOW_BLOCK_BINDER_TYPE_CLSACT_INGRESS;
	bo.block = &b->lower_block;
	bo.cb_list_head = &b->lower_block.cb_list;
	INIT_LIST_HEAD(&bo.cb_list);
	ret = lower->netdev_ops->ndo_setup_tc(lower, TC_SETUP_FT, &bo);
	if (ret) {
		dev_put(lower);
		return ret;
	}
	list_splice_tail_init(&bo.cb_list, &b->lower_block.cb_list);
	if (list_empty(&b->lower_block.cb_list)) {
		dev_put(lower);
		return -EOPNOTSUPP;
	}
	b->lower_cb = list_first_entry(&b->lower_block.cb_list, struct flow_block_cb, list);
	b->lower = lower;
	return 0;
}

int fmcs_bridge_setup_tc(struct fmcs_priv *priv, enum tc_setup_type type, void *data)
{
	struct fmcs_bridge_offload *b = priv->bridge_offload;
	struct flow_block_offload *bo = data;
	struct flow_block_cb *cb;
	int ret;

	if (type != TC_SETUP_FT || bo->binder_type != FLOW_BLOCK_BINDER_TYPE_CLSACT_INGRESS)
		return -EOPNOTSUPP;
	/* Netfilter invokes ndo_setup_tc without RTNL during nft transactions.
	 * Taking RTNL here would also invert lower-unregister/flow cleanup locks.
	 */
	mutex_lock(&b->lock);
	bo->driver_block_list = &b->driver_blocks;
	cb = flow_block_cb_lookup(bo->block, fmcs_bridge_cb, b);
	if (bo->command == FLOW_BLOCK_BIND) {
		ret = fmcs_bridge_lower_bind(b);
		if (ret)
			goto unlock;
		if (!cb) {
			cb = flow_block_cb_alloc(fmcs_bridge_cb, b, b, NULL);
			if (IS_ERR(cb)) {
				ret = PTR_ERR(cb);
				goto unlock;
			}
			flow_block_cb_add(cb, bo);
			list_add_tail(&cb->driver_list, &b->driver_blocks);
		}
		flow_block_cb_incref(cb);
		ret = 0;
		goto unlock;
	}
	if (bo->command != FLOW_BLOCK_UNBIND || !cb) {
		ret = -ENOENT;
		goto unlock;
	}
	if (!flow_block_cb_decref(cb)) {
		flow_block_cb_remove(cb, bo);
		list_del(&cb->driver_list);
	}
	ret = 0;
unlock:
	mutex_unlock(&b->lock);
	return ret;
}

/* Caller first stops admission and synchronously cleans all logical flows. */
void fmcs_bridge_unbind(struct fmcs_priv *priv, struct net_device *lower)
{
	struct fmcs_bridge_offload *b = priv->bridge_offload;
	struct flow_block_offload bo = {};
	struct flow_block_cb *cb, *next;
	unsigned long cookie;
	void *entry;

	ASSERT_RTNL();
	mutex_lock(&b->lock);
	if (!b->lower_cb)
		goto unlock;
	/* Also revoke any retained cookie if the flowtable has already gone away. */
	xa_for_each(&b->cookies, cookie, entry) {
		struct flow_cls_offload cls = {
			.command = FLOW_CLS_DESTROY,
			.cookie = cookie,
		};
		b->lower_cb->cb(TC_SETUP_CLSFLOWER, &cls, b->lower_cb->cb_priv);
		xa_erase(&b->cookies, cookie);
	}
	bo.net = dev_net(lower);
	bo.command = FLOW_BLOCK_UNBIND;
	bo.binder_type = FLOW_BLOCK_BINDER_TYPE_CLSACT_INGRESS;
	bo.block = &b->lower_block;
	bo.cb_list_head = &b->lower_block.cb_list;
	INIT_LIST_HEAD(&bo.cb_list);
	lower->netdev_ops->ndo_setup_tc(lower, TC_SETUP_FT, &bo);
	list_for_each_entry_safe(cb, next, &bo.cb_list, list) {
		list_del(&cb->list);
		flow_block_cb_free(cb);
	}
	b->lower_cb = NULL;
	dev_put(b->lower);
	b->lower = NULL;
unlock:
	mutex_unlock(&b->lock);
}

int fmcs_bridge_init(struct fmcs_priv *priv)
{
	struct fmcs_bridge_offload *b = kzalloc(sizeof(*b), GFP_KERNEL);

	if (!b)
		return -ENOMEM;
	b->priv = priv;
	mutex_init(&b->lock);
	INIT_LIST_HEAD(&b->lower_block.cb_list);
	INIT_LIST_HEAD(&b->driver_blocks);
	xa_init(&b->cookies);
	priv->bridge_offload = b;
	return 0;
}

void fmcs_bridge_exit(struct fmcs_priv *priv)
{
	struct fmcs_bridge_offload *b = priv->bridge_offload;

	WARN_ON(!list_empty(&b->driver_blocks));
	WARN_ON(b->lower_cb || !xa_empty(&b->cookies));
	xa_destroy(&b->cookies);
	kfree(b);
	priv->bridge_offload = NULL;
}
