/*
 * H3C HM2004-DU Micro-OLT Virtual Network Interfaces:
 * - molt_omci: OMCI control channel
 * - molt_ploam: PLOAM control channel
 * - fttr1 ~ fttr16: Downstream switch ports mapped to ONU-ID 0~15
 */

#include <linux/netdevice.h>
#include <linux/etherdevice.h>
#include <linux/skbuff.h>
#include <linux/if_vlan.h>
#include <linux/if_arp.h>
#include <linux/of_net.h>
#include <linux/rtnetlink.h>
#include <net/netfilter/nf_flow_table.h>
#include <net/switchdev.h>
#include "fmcs.h"

struct fmcs_fttr_port_priv {
	struct fmcs_priv *priv;
	u32 port_idx; /* 1-based port index: 1..16 */
};

static rx_handler_result_t fmcs_handle_frame(struct sk_buff **pskb);

/* Explicit opt-in, bit 0 = fttr1. Carrier loss revokes the affected bit. */
static unsigned int routed_offload_mask;
static unsigned int bridge_offload_mask;
static DEFINE_SPINLOCK(fmcs_bridge_gate_lock);
static unsigned int fmcs_bridge_epoch;
static struct fmcs_priv *fmcs_offload_priv; /* protected by RTNL */

static void fmcs_offload_cleanup(struct fmcs_priv *priv)
{
	struct net_device *lower = rtnl_dereference(priv->fttr_lower);
	int i;

	ASSERT_RTNL();
	for (i = 0; i < FMCS_MAX_FTTR_PORTS; i++)
		if (priv->fttr_devs[i])
			nf_flow_table_cleanup(priv->fttr_devs[i]);
	if (lower)
		nf_flow_table_cleanup(lower);
}

static int fmcs_offload_mask_set(const char *val, const struct kernel_param *kp)
{
	unsigned int mask;
	int ret = kstrtouint(val, 0, &mask);

	if (ret)
		return ret;
	if (mask & ~GENMASK(FMCS_MAX_FTTR_PORTS - 1, 0))
		return -ERANGE;
	rtnl_lock();
	WRITE_ONCE(routed_offload_mask, 0);
	/* Finish path users before deleting the old logical/physical keys. */
	synchronize_net();
	if (fmcs_offload_priv)
		fmcs_offload_cleanup(fmcs_offload_priv);
	WRITE_ONCE(routed_offload_mask, mask);
	rtnl_unlock();
	return 0;
}

static const struct kernel_param_ops fmcs_offload_mask_ops = {
	.set = fmcs_offload_mask_set,
	.get = param_get_uint,
};
module_param_cb(routed_offload_mask, &fmcs_offload_mask_ops,
		&routed_offload_mask, 0644);
MODULE_PARM_DESC(routed_offload_mask,
	"Experimental routed VLAN path: bit 0=fttr1; default 0; carrier-off revokes bit");

static void fmcs_bridge_revoke(void)
{
	unsigned long flags;

	spin_lock_irqsave(&fmcs_bridge_gate_lock, flags);
	WRITE_ONCE(bridge_offload_mask, 0);
	fmcs_bridge_epoch++;
	spin_unlock_irqrestore(&fmcs_bridge_gate_lock, flags);
}

static void fmcs_bridge_gc(struct work_struct *work)
{
	rtnl_lock();
	if (fmcs_offload_priv)
		fmcs_offload_cleanup(fmcs_offload_priv);
	rtnl_unlock();
}
static DECLARE_WORK(fmcs_bridge_gc_work, fmcs_bridge_gc);

/* Software FDB moves and bridge STP/VLAN changes must revoke the pair too.
 * Switchdev's atomic notifications cannot synchronously drain flow workers.
 */
static int fmcs_bridge_event(struct notifier_block *nb, unsigned long event, void *ptr)
{
	struct fmcs_priv *priv = READ_ONCE(fmcs_offload_priv);
	struct net_device *dev = switchdev_notifier_info_to_dev(ptr);
	unsigned int mask = READ_ONCE(bridge_offload_mask);
	bool relevant = false;
	int i;

	if (!mask || !priv)
		return NOTIFY_DONE;
	rcu_read_lock();
	for (i = 0; i < FMCS_MAX_FTTR_PORTS; i++) {
		struct net_device *port = priv->fttr_devs[i];

		if ((mask & BIT(i)) && port &&
		    (dev == port || dev == netdev_master_upper_dev_get_rcu(port))) {
			relevant = true;
			break;
		}
	}
	rcu_read_unlock();
	if (!relevant)
		return NOTIFY_DONE;
	switch (event) {
	case SWITCHDEV_FDB_ADD_TO_DEVICE:
	case SWITCHDEV_FDB_DEL_TO_DEVICE:
	case SWITCHDEV_PORT_ATTR_SET:
	case SWITCHDEV_PORT_OBJ_ADD:
	case SWITCHDEV_PORT_OBJ_DEL:
		if (READ_ONCE(bridge_offload_mask)) {
			pr_info_ratelimited("fmcs bridge: revoke mask=%x switchdev_event=%lu dev=%s\n",
					    mask, event, dev->name);
			fmcs_bridge_revoke();
			schedule_work(&fmcs_bridge_gc_work);
		}
		break;
	}
	return NOTIFY_DONE;
}
static struct notifier_block fmcs_bridge_nb = { .notifier_call = fmcs_bridge_event };
static struct notifier_block fmcs_bridge_blocking_nb = { .notifier_call = fmcs_bridge_event };

static int fmcs_bridge_mask_set(const char *val, const struct kernel_param *kp)
{
	unsigned long flags;
	unsigned int mask, epoch;
	int ret = kstrtouint(val, 0, &mask);

	if (ret)
		return ret;
	if (mask & ~GENMASK(FMCS_MAX_FTTR_PORTS - 1, 0))
		return -ERANGE;
	if (mask && hweight32(mask) < 2)
		return -EINVAL;
	rtnl_lock();
	fmcs_bridge_revoke();
	spin_lock_irqsave(&fmcs_bridge_gate_lock, flags);
	epoch = fmcs_bridge_epoch;
	spin_unlock_irqrestore(&fmcs_bridge_gate_lock, flags);
	synchronize_net();
	if (fmcs_offload_priv)
		fmcs_offload_cleanup(fmcs_offload_priv);
	spin_lock_irqsave(&fmcs_bridge_gate_lock, flags);
	if (epoch == fmcs_bridge_epoch)
		WRITE_ONCE(bridge_offload_mask, mask);
	else
		ret = -EAGAIN;
	spin_unlock_irqrestore(&fmcs_bridge_gate_lock, flags);
	rtnl_unlock();
	return ret;
}

static const struct kernel_param_ops fmcs_bridge_mask_ops = {
	.set = fmcs_bridge_mask_set,
	.get = param_get_uint,
};
module_param_cb(bridge_offload_mask, &fmcs_bridge_mask_ops, &bridge_offload_mask, 0644);
MODULE_PARM_DESC(bridge_offload_mask,
	"Untagged IPv4 bridge ports: at least two port bits or 0; topology changes revoke");

bool fmcs_bridge_port_enabled(struct fmcs_priv *priv, unsigned int port)
{
	struct net_device *dev = priv->fttr_devs[port - 1];

	return (READ_ONCE(bridge_offload_mask) & BIT(port - 1)) && dev &&
	       netif_running(dev) && netif_carrier_ok(dev);
}

bool fmcs_bridge_pair_enabled(struct fmcs_priv *priv, unsigned int src, unsigned int dst)
{
	unsigned int pair = BIT(src - 1) | BIT(dst - 1);

	return (READ_ONCE(bridge_offload_mask) & pair) == pair &&
	       netif_running(priv->fttr_devs[src - 1]) &&
	       netif_carrier_ok(priv->fttr_devs[src - 1]) &&
	       netif_running(priv->fttr_devs[dst - 1]) &&
	       netif_carrier_ok(priv->fttr_devs[dst - 1]);
}

/* Binding is by name only at discovery. Once bound, identity survives rename. */
static struct net_device *fmcs_lower_get(struct fmcs_priv *priv)
{
	struct net_device *lower;

	rcu_read_lock();
	lower = rcu_dereference(priv->fttr_lower);
	if (lower)
		dev_hold(lower);
	rcu_read_unlock();
	return lower;
}

int fmcs_fttr_set_carrier(struct fmcs_priv *priv, unsigned int port, bool carrier)
{
	struct net_device *dev;
	int ret = 0;

	if (port < 1 || port > FMCS_MAX_FTTR_PORTS)
		return -EINVAL;
	rtnl_lock();
	dev = priv->fttr_devs[port - 1];
	if (!dev) {
		ret = -ENODEV;
		goto unlock;
	}
	if (carrier) {
		netif_carrier_on(dev);
	} else {
		if (READ_ONCE(bridge_offload_mask) & BIT(port - 1))
			fmcs_bridge_revoke();
		WRITE_ONCE(routed_offload_mask,
			   routed_offload_mask & ~BIT(port - 1));
		netif_carrier_off(dev);
		synchronize_net();
		/* carrier_off does not emit NETDEV_DOWN. Revoking bridge admission
		 * invalidates every selected pair, including pairs on other ports.
		 * Repeat even when already off: this is an explicit cleanup request.
		 */
		fmcs_offload_cleanup(priv);
	}
unlock:
	rtnl_unlock();
	return ret;
}

static void fmcs_lower_unlink(struct fmcs_priv *priv)
{
	struct net_device *lower = rtnl_dereference(priv->fttr_lower);
	int i;

	ASSERT_RTNL();
	if (!lower)
		return;
	WRITE_ONCE(routed_offload_mask, 0);
	fmcs_bridge_revoke();
	rcu_assign_pointer(priv->fttr_lower, NULL);
	netdev_rx_handler_unregister(lower);
	/* unregister above waits for path readers; retain lower for cleanup. */
	fmcs_offload_cleanup(priv);
	nf_flow_table_cleanup(lower);
	fmcs_bridge_unbind(priv, lower);
	for (i = 0; i < FMCS_MAX_FTTR_PORTS; i++)
		netdev_upper_dev_unlink(lower, priv->fttr_devs[i]);
	dev_put(lower);
}

static int fmcs_lower_link(struct fmcs_priv *priv, struct net_device *lower)
{
	int i, ret;

	ASSERT_RTNL();
	if (lower->type != ARPHRD_ETHER || lower->mtu < ETH_MIN_MTU)
		return -EINVAL;
	for (i = 0; i < FMCS_MAX_FTTR_PORTS; i++) {
		ret = netdev_upper_dev_link(lower, priv->fttr_devs[i], NULL);
		if (ret)
			goto rollback;
	}
	/* All ports are registered before the notifier's initial replay. */
	for (i = 0; i < FMCS_MAX_FTTR_PORTS; i++) {
		struct net_device *port = priv->fttr_devs[i];

		if (port->mtu > lower->mtu) {
			ret = dev_set_mtu(port, lower->mtu);
			if (ret) {
				i = FMCS_MAX_FTTR_PORTS;
				goto rollback;
			}
		}
		port->needed_headroom = lower->needed_headroom + VLAN_HLEN;
		port->needed_tailroom = lower->needed_tailroom;
	}
	ret = netdev_rx_handler_register(lower, fmcs_handle_frame, priv);
	if (ret) {
		i = FMCS_MAX_FTTR_PORTS;
		goto rollback;
	}
	dev_hold(lower);
	rcu_assign_pointer(priv->fttr_lower, lower);
	return 0;

rollback:
	while (--i >= 0)
		netdev_upper_dev_unlink(lower, priv->fttr_devs[i]);
	return ret;
}

static int fmcs_netdev_event(struct notifier_block *nb,
			     unsigned long event, void *ptr)
{
	struct fmcs_priv *priv = container_of(nb, struct fmcs_priv, fttr_netdev_nb);
	struct net_device *dev = netdev_notifier_info_to_dev(ptr);
	struct net_device *lower = rtnl_dereference(priv->fttr_lower);
	int i, ret;

	if (dev == lower) {
		if ((event == NETDEV_DOWN || event == NETDEV_CHANGEADDR ||
		     event == NETDEV_CHANGEMTU ||
		     (event == NETDEV_CHANGE && !netif_carrier_ok(dev))) &&
		    (routed_offload_mask || READ_ONCE(bridge_offload_mask))) {
			WRITE_ONCE(routed_offload_mask, 0);
			fmcs_bridge_revoke();
			synchronize_net();
			fmcs_offload_cleanup(priv);
		}
		if (event == NETDEV_UNREGISTER)
			fmcs_lower_unlink(priv);
		if (event == NETDEV_PRECHANGEMTU) {
			struct netdev_notifier_info_ext *info = ptr;

			/* Reject, rather than leave ports with an unusable MTU. */
			for (i = 0; i < FMCS_MAX_FTTR_PORTS; i++)
				if (priv->fttr_devs[i]->mtu > info->ext.mtu)
					return notifier_from_errno(-EBUSY);
		}
		return NOTIFY_OK;
	}
	for (i = 0; i < FMCS_MAX_FTTR_PORTS; i++) {
		if (dev != priv->fttr_devs[i] ||
		    !((routed_offload_mask | READ_ONCE(bridge_offload_mask)) & BIT(i)))
			continue;
		if (event == NETDEV_DOWN || event == NETDEV_CHANGEUPPER ||
		    event == NETDEV_CHANGEADDR || event == NETDEV_CHANGEMTU) {
			WRITE_ONCE(routed_offload_mask,
				   routed_offload_mask & ~BIT(i));
			fmcs_bridge_revoke();
			synchronize_net();
			fmcs_offload_cleanup(priv);
		}
		break;
	}
	if (!lower && (event == NETDEV_REGISTER || event == NETDEV_CHANGENAME) &&
	    net_eq(dev_net(dev), &init_net) && !strcmp(dev->name, "fttr0")) {
		ret = fmcs_lower_link(priv, dev);
		if (ret)
			dev_warn(priv->dev, "cannot bind fttr0 lower device: %d\n", ret);
		/* An optional late lower must not prevent its own registration. */
	}
	return NOTIFY_DONE;
}

/* Fallback prefix used only when the DT supplies no mac-address cell.
 * Locally administered (0x02) and identical on every unit, so it must not
 * be relied on: two units on the same L2 domain would collide. */
#define FMCS_FALLBACK_MAC_PREFIX	{ 0x02, 0xFE, 0x88, 0x75, 0x81, 0x00 }

/*
 * Increment a MAC by n with carry across all six bytes, so a block of
 * consecutive ports never wraps the last byte silently.
 */
static void fmcs_mac_add(u8 *mac, unsigned int n)
{
	unsigned int carry = n;
	int i;

	for (i = ETH_ALEN - 1; i >= 0 && carry; i--) {
		unsigned int v = mac[i] + (carry & 0xff);

		mac[i] = v & 0xff;
		carry = (carry >> 8) + (v >> 8);
	}
}

/*
 * Resolve the base MAC for the fttr1..fttr16 block.
 *
 * The cell is consumed through of_get_mac_address(), so the DT side declares
 * it exactly like any other ethernet node:
 *	nvmem-cell-names = "mac-address";
 *	nvmem-cells = <&base_mac 9>;
 *
 * fttr1 takes the cell verbatim and fttr2..fttr16 are derived from it, which
 * reserves base+9 .. base+24 of the mac-base index space for this driver.
 *
 * Returns 0 on success, -EPROBE_DEFER when the nvmem provider is not up yet
 * (the caller must propagate it), or a negative errno when no cell exists.
 */
static int fmcs_get_base_mac(struct fmcs_priv *priv, u8 *mac)
{
	static const u8 fallback[ETH_ALEN] = FMCS_FALLBACK_MAC_PREFIX;
	int ret;

	if (!priv->dev || !priv->dev->of_node)
		ret = -ENODEV;
	else
		ret = of_get_mac_address(priv->dev->of_node, mac);

	if (ret == -EPROBE_DEFER)
		return ret;

	if (ret) {
		dev_warn(priv->dev,
			 "no mac-address in DT (%d); fttr ports fall back to the locally-administered 02:fe:88:75:81:xx block\n",
			 ret);
		memcpy(mac, fallback, ETH_ALEN);
	}

	if (!is_valid_ether_addr(mac)) {
		dev_warn(priv->dev, "base MAC from DT is invalid; using fallback block\n");
		memcpy(mac, fallback, ETH_ALEN);
	}

	return 0;
}

static netdev_tx_t fmcs_omci_xmit(struct sk_buff *skb, struct net_device *dev)
{
	struct fmcs_priv *priv = *(struct fmcs_priv **)netdev_priv(dev);
	struct net_device *dst_dev;
	u32 msg_type, onu_id;
	u16 vlan_tci;

	dev->stats.tx_packets++;
	dev->stats.tx_bytes += skb->len;

	/*
	 * The OMCI transport uses an 8-byte header:
	 * [0..3]: htonl(3) (OMCI frame type)
	 * [4..7]: htonl(onu_id)
	 * [8..55]: 48-byte OMCI payload
	 */
	if (skb->len < 8) {
		dev_kfree_skb_any(skb);
		return NETDEV_TX_OK;
	}

	msg_type = be32_to_cpup((__be32 *)skb->data);
	onu_id = be32_to_cpup((__be32 *)(skb->data + 4));

	if (msg_type == 3 && onu_id <= 15 && skb->len == 56 &&
	    !skb_linearize(skb)) {
		/* The bound lower is the FPGA SerDes link, including after rename. */
		dst_dev = fmcs_lower_get(priv);

		if (dst_dev) {
			/* Allocate fresh skb with headroom for 16-byte header */
			u32 omci_len = skb->len - 8;
			struct sk_buff *nskb = alloc_skb(16 + omci_len + 32, GFP_ATOMIC);
			if (nskb) {
				u8 *p;
				skb_reserve(nskb, 2); /* align IP/payload */
				p = skb_put(nskb, 16 + omci_len);

				/* Dest MAC: AA:AA:AA:AA:AA:AA */
				memset(p, 0xaa, 6);
				/* Src MAC: 55:55:55:55:55:55 */
				memset(p + 6, 0x55, 6);
				/* EtherType: 0x8100 (802.1Q) */
				p[12] = 0x81;
				p[13] = 0x00;
				/* VLAN TCI: Priority=1 (0x2000) | ONU_ID */
				vlan_tci = READ_ONCE(priv->omci_channel[onu_id]);
				vlan_tci = 0x2000 | (vlan_tci ? vlan_tci : onu_id);
				p[14] = (u8)(vlan_tci >> 8);
				p[15] = (u8)(vlan_tci & 0xff);

				/* Copy 48-byte OMCI payload */
				memcpy(p + 16, skb->data + 8, omci_len);

				/* HSGMII TX pads the proprietary envelope to 68 bytes.
				 * A 64-byte baseline envelope was observed to receive no reply.
				 * Padding follows the OMCI CRC and is not part of the PDU.
				 */
				if (nskb->len < 68)
					skb_put_zero(nskb, 68 - nskb->len);

				nskb->dev = dst_dev;
				skb_reset_mac_header(nskb);
				skb_reset_network_header(nskb);
				nskb->protocol = htons(0x88b5);
				pr_debug("fmcs: OMCI TX fttr0 onu_id %u len %u\n", onu_id, nskb->len);
				dev_queue_xmit(nskb);
			}
			dev_put(dst_dev);
		}
	}

	(void)priv;
	dev_kfree_skb_any(skb);
	return NETDEV_TX_OK;
}

static void fmcs_ploam_tx_work(struct work_struct *work)
{
	struct fmcs_priv *priv = container_of(work, struct fmcs_priv, ploam_tx_work);
	struct sk_buff *skb;

	while ((skb = skb_dequeue(&priv->ploam_tx_queue))) {
		if (fmcs_spi_send_ploam(priv, skb->data, skb->len))
			priv->ploam_dev->stats.tx_errors++;
		dev_kfree_skb(skb);
	}
}

static netdev_tx_t fmcs_ploam_xmit(struct sk_buff *skb, struct net_device *dev)
{
	struct fmcs_priv *priv = *(struct fmcs_priv **)netdev_priv(dev);

	dev->stats.tx_packets++;
	dev->stats.tx_bytes += skb->len;

	/* ndo_start_xmit runs in atomic context; SPI controller locks sleep. */
	if (priv && atomic_read(&priv->fpga_status) == FMCS_FPGA_STATUS_READY &&
	    skb->len == 13 && skb_queue_len(&priv->ploam_tx_queue) < 64 &&
	    !skb_linearize(skb)) {
		skb_queue_tail(&priv->ploam_tx_queue, skb);
		schedule_work(&priv->ploam_tx_work);
		return NETDEV_TX_OK;
	}

	dev->stats.tx_dropped++;
	dev_kfree_skb_any(skb);
	return NETDEV_TX_OK;
}

static netdev_tx_t fmcs_fttr_xmit(struct sk_buff *skb, struct net_device *dev)
{
	struct fmcs_fttr_port_priv *port_priv = netdev_priv(dev);
	struct net_device *dst_dev;
	u16 vlan_tci;

	dev->stats.tx_packets++;
	dev->stats.tx_bytes += skb->len;

	dst_dev = fmcs_lower_get(port_priv->priv);
	if (dst_dev) {
		/* A business VLAN in metadata must remain INSIDE the FPGA tag. */
		if (skb_vlan_tag_present(skb)) {
			skb = __vlan_hwaccel_push_inside(skb);
			if (!skb)
				goto drop_put;
		}
		/* port N maps to ONU-ID N-1 and GEM 0x100 + ONU-ID. */
		vlan_tci = FMCS_PORT_TO_FPGA_VID(port_priv->port_idx);
		skb = vlan_insert_tag_set_proto(skb, htons(ETH_P_8021Q), vlan_tci);
		if (skb && !skb_put_padto(skb, 68)) {
			skb->dev = dst_dev;
			dev_queue_xmit(skb);
			dev_put(dst_dev);
			return NETDEV_TX_OK;
		}
	drop_put:
		dev_put(dst_dev);
		dev->stats.tx_dropped++;
		return NETDEV_TX_OK;
	}

	dev->stats.tx_dropped++;
	dev_kfree_skb_any(skb);
	return NETDEV_TX_OK;
}

static int fmcs_fttr_open(struct net_device *dev)
{
	/* Carrier state is maintained externally by ONU O5 link status */
	return 0;
}

static int fmcs_fttr_stop(struct net_device *dev)
{
	return 0;
}

static int fmcs_fttr_get_iflink(const struct net_device *dev)
{
	const struct fmcs_fttr_port_priv *port = netdev_priv(dev);
	struct net_device *lower;
	int iflink = 0;

	rcu_read_lock();
	lower = rcu_dereference(port->priv->fttr_lower);
	if (lower)
		iflink = lower->ifindex;
	rcu_read_unlock();
	return iflink;
}

static int fmcs_fttr_change_mtu(struct net_device *dev, int mtu)
{
	struct fmcs_fttr_port_priv *port = netdev_priv(dev);
	struct net_device *lower = rtnl_dereference(port->priv->fttr_lower);

	if (lower && mtu > lower->mtu)
		return -ERANGE;
	WRITE_ONCE(dev->mtu, mtu);
	return 0;
}

static inline bool fmcs_in_bridge_path(void)
{
	void *ret0 = __builtin_return_address(0);
	void *ret1 = __builtin_return_address(1);
	void *fn = (void *)dev_fill_bridge_path;

	if ((ret0 >= fn && ret0 < fn + 0x400) ||
	    (ret1 >= fn && ret1 < fn + 0x400))
		return true;

	return false;
}

static int fmcs_fttr_fill_forward_path(struct net_device_path_ctx *ctx,
				      struct net_device_path *path)
{
	const struct net_device *dev = ctx->dev;
	const struct fmcs_fttr_port_priv *port = netdev_priv(dev);
	struct net_device *lower = rcu_dereference(port->priv->fttr_lower);
	unsigned int mask = READ_ONCE(routed_offload_mask);
	int i;

	/* If called within bridge path resolution (dev_fill_bridge_path),
	 * keep software bridge tuples on fttrN; only the hardware rule gets
	 * transport VLAN/physical redirect via fmcs_bridge_setup_tc.
	 */
	if (fmcs_in_bridge_path()) {
		if (!fmcs_bridge_port_enabled(port->priv, port->port_idx) ||
		    ctx->num_vlans || !lower || !netif_running(lower) ||
		    !netif_carrier_ok(lower))
			return -EOPNOTSUPP;
		path->type = DEV_PATH_ETHERNET;
		path->dev = dev;
		ctx->dev = NULL;
		return 0;
	}

	/* Routed path resolution (e.g. WAN <-> FTTR ONU). */
	if (!(mask & BIT(port->port_idx - 1)) || !lower ||
	    !netif_running(dev) || !netif_carrier_ok(dev) ||
	    !netif_running(lower) || !netif_carrier_ok(lower))
		return -ENODEV;
	if (!lower->netdev_ops->ndo_setup_tc ||
	    !(lower->features & NETIF_F_HW_TC))
		return -EOPNOTSUPP;
	if (ctx->num_vlans >= ARRAY_SIZE(ctx->vlan))
		return -ENOSPC;
	/* One transport tag plus at most one C-tag; no S-tag or VID 0. */
	for (i = 0; i < ctx->num_vlans; i++)
		if (ctx->vlan[i].proto != htons(ETH_P_8021Q) ||
		    !ctx->vlan[i].id)
			return -EOPNOTSUPP;

	path->type = DEV_PATH_VLAN;
	path->dev = dev;
	path->encap.id = FMCS_PORT_TO_FPGA_VID(port->port_idx);
	path->encap.proto = htons(ETH_P_8021Q);
	ctx->vlan[ctx->num_vlans].id = path->encap.id;
	ctx->vlan[ctx->num_vlans].proto = path->encap.proto;
	ctx->num_vlans++;
	ctx->dev = lower;
	return 0;
}

static int fmcs_fttr_setup_tc(struct net_device *dev, enum tc_setup_type type, void *data)
{
	struct fmcs_fttr_port_priv *port = netdev_priv(dev);

	return fmcs_bridge_setup_tc(port->priv, type, data);
}

static const struct net_device_ops fmcs_omci_ops = {
	.ndo_start_xmit = fmcs_omci_xmit,
};

static const struct net_device_ops fmcs_ploam_ops = {
	.ndo_start_xmit = fmcs_ploam_xmit,
};

static const struct net_device_ops fmcs_fttr_ops = {
	.ndo_open       = fmcs_fttr_open,
	.ndo_stop       = fmcs_fttr_stop,
	.ndo_start_xmit = fmcs_fttr_xmit,
	.ndo_set_mac_address = eth_mac_addr,
	.ndo_validate_addr   = eth_validate_addr,
	.ndo_get_iflink      = fmcs_fttr_get_iflink,
	.ndo_change_mtu      = fmcs_fttr_change_mtu,
	.ndo_fill_forward_path = fmcs_fttr_fill_forward_path,
	.ndo_setup_tc        = fmcs_fttr_setup_tc,
};

void fmcs_rx_omci_frame(struct fmcs_priv *priv, const u8 *data, size_t len)
{
	struct sk_buff *skb;

	if (!priv->omci_dev || !(priv->omci_dev->flags & IFF_UP))
		return;

	skb = netdev_alloc_skb(priv->omci_dev, len + 2);
	if (!skb)
		return;

	skb_reserve(skb, 2);
	skb_put_data(skb, data, len);
	skb->dev = priv->omci_dev;
	skb_reset_mac_header(skb);
	skb->protocol = htons(ETH_P_802_2);

	priv->omci_dev->stats.rx_packets++;
	priv->omci_dev->stats.rx_bytes += len;
	netif_rx(skb);
}

void fmcs_rx_ploam_frame(struct fmcs_priv *priv, const u8 *data, size_t len)
{
	struct sk_buff *skb;

	if (!priv->ploam_dev || !(priv->ploam_dev->flags & IFF_UP))
		return;

	skb = netdev_alloc_skb(priv->ploam_dev, len + 2);
	if (!skb)
		return;

	skb_reserve(skb, 2);
	skb_put_data(skb, data, len);
	skb->dev = priv->ploam_dev;
	skb_reset_mac_header(skb);
	skb->protocol = htons(ETH_P_802_2);

	priv->ploam_dev->stats.rx_packets++;
	priv->ploam_dev->stats.rx_bytes += len;
	netif_rx(skb);
}

static rx_handler_result_t fmcs_handle_frame(struct sk_buff **pskb)
{
	struct sk_buff *skb = *pskb;
	struct fmcs_priv *priv = rcu_dereference(skb->dev->rx_handler_data);
	u16 vlan_tci, vlan_id, priority;
	u16 onu_id;
	int mac_len;

	/* __netif_receive_skb_core() has already moved an inline outer VLAN
	 * into metadata. Do not claim unrelated lower-device traffic.
	 */
	if (!skb_vlan_tag_present(skb) || skb->vlan_proto != htons(ETH_P_8021Q))
		return RX_HANDLER_PASS;
	vlan_tci = skb_vlan_tag_get(skb);

	priority = (vlan_tci >> 13) & 0x07;
	vlan_id = vlan_tci & 0x0fff;
	onu_id = vlan_id;
	if (vlan_id >= 0x100 && vlan_id <= 0x10f &&
	    READ_ONCE(priv->omci_channel[vlan_id - 0x100]) == vlan_id)
		onu_id = vlan_id - 0x100;

	/* Priority 1 (0x2000): OMCI response frame from sub-gateway */
	if (priority == 1 && onu_id <= 15) {
		u8 omci_buf[56];

		/* No inner EtherType: the first two OMCI bytes occupy h_proto
		 * after VLAN untagging, i.e. two bytes before skb->data.
		 */
		if (!skb_mac_header_was_set(skb) ||
		    skb_mac_header(skb) < skb->head ||
		    skb_mac_header(skb) > skb->data)
			goto consumed;
		mac_len = skb->data - skb_mac_header(skb);
		if (mac_len != ETH_HLEN || skb->len < 46)
			goto consumed;

		/*
		 * Deliver 56-byte frame to molt_omci:
		 * [0..3]: htonl(3)
		 * [4..7]: htonl(mapped ONU-ID)
		 * [8..55]: 48-byte OMCI payload
		 */
		*(__be32 *)&omci_buf[0] = cpu_to_be32(3);
		*(__be32 *)&omci_buf[4] = cpu_to_be32(onu_id);
		if (skb_copy_bits(skb, 12 - mac_len,
				  &omci_buf[8], 48))
			goto consumed;

		pr_debug("fmcs: OMCI RX fttr0 onu_id %u len %zu\n", onu_id, sizeof(omci_buf));
		fmcs_rx_omci_frame(priv, omci_buf, sizeof(omci_buf));
		goto consumed;
	}

	/* Priority 0: GEM 0x100..0x10f maps to fttr1..fttr16. */
	if (priority == 0 && vlan_id >= 0x100 && vlan_id <= 0x10f) {
		struct net_device *sub_dev = priv->fttr_devs[vlan_id - 0x100];

		if (sub_dev && (sub_dev->flags & IFF_UP) && netif_carrier_ok(sub_dev)) {
			if (!skb_mac_header_was_set(skb) ||
			    skb_mac_header(skb) < skb->head ||
			    skb_mac_header(skb) > skb->data)
				goto consumed;
			mac_len = skb->data - skb_mac_header(skb);
			if (mac_len != ETH_HLEN)
				goto consumed;
			/* AF_PACKET listeners may still hold a reference. */
			skb = skb_share_check(skb, GFP_ATOMIC);
			*pskb = skb;
			if (!skb)
				return RX_HANDLER_CONSUMED;
			/* Remove ONLY the FPGA tag. The next receive round handles
			 * any business VLAN on the logical port, including VID 0.
			 */
			__vlan_hwaccel_clear_tag(skb);
			skb_push_rcsum(skb, mac_len);
			skb_scrub_packet(skb, false);
			skb->dev = sub_dev;
			skb->protocol = eth_type_trans(skb, sub_dev);
			skb_postpull_rcsum(skb, skb_mac_header(skb), ETH_HLEN);
			sub_dev->stats.rx_packets++;
			sub_dev->stats.rx_bytes += skb->len;
			return RX_HANDLER_ANOTHER;
		}
		goto consumed;
	}
	return RX_HANDLER_PASS;

consumed:
	dev_kfree_skb_any(skb);
	*pskb = NULL;
	return RX_HANDLER_CONSUMED;
}

int fmcs_netdevs_init(struct fmcs_priv *priv)
{
	int ret;
	int i;
	u8 base_mac[ETH_ALEN];
	u8 port_mac[ETH_ALEN];

	skb_queue_head_init(&priv->ploam_tx_queue);
	INIT_WORK(&priv->ploam_tx_work, fmcs_ploam_tx_work);

	/* Create molt_omci */
	priv->omci_dev = alloc_netdev(sizeof(struct fmcs_priv *), "molt_omci",
				      NET_NAME_UNKNOWN, ether_setup);
	if (!priv->omci_dev)
		return -ENOMEM;

	priv->omci_dev->netdev_ops = &fmcs_omci_ops;
	priv->omci_dev->type = ARPHRD_NONE;
	priv->omci_dev->hard_header_len = 0;
	priv->omci_dev->header_ops = NULL;
	priv->omci_dev->flags = IFF_NOARP;
	*(struct fmcs_priv **)netdev_priv(priv->omci_dev) = priv;
	eth_hw_addr_random(priv->omci_dev);

	ret = register_netdev(priv->omci_dev);
	if (ret) {
		free_netdev(priv->omci_dev);
		priv->omci_dev = NULL;
		return ret;
	}

	/* Create molt_ploam */
	priv->ploam_dev = alloc_netdev(sizeof(struct fmcs_priv *), "molt_ploam",
				       NET_NAME_UNKNOWN, ether_setup);
	if (!priv->ploam_dev) {
		unregister_netdev(priv->omci_dev);
		free_netdev(priv->omci_dev);
		priv->omci_dev = NULL;
		return -ENOMEM;
	}

	priv->ploam_dev->netdev_ops = &fmcs_ploam_ops;
	priv->ploam_dev->type = ARPHRD_NONE;
	priv->ploam_dev->hard_header_len = 0;
	priv->ploam_dev->header_ops = NULL;
	priv->ploam_dev->flags = IFF_NOARP;
	*(struct fmcs_priv **)netdev_priv(priv->ploam_dev) = priv;
	eth_hw_addr_random(priv->ploam_dev);

	ret = register_netdev(priv->ploam_dev);
	if (ret) {
		free_netdev(priv->ploam_dev);
		priv->ploam_dev = NULL;
		unregister_netdev(priv->omci_dev);
		free_netdev(priv->omci_dev);
		priv->omci_dev = NULL;
		return ret;
	}

	/*
	 * Resolve a per-unit base MAC for the downstream FTTR switch ports.
	 *
	 * These 16 ports end up enslaved in br-lan, and a Linux bridge adopts
	 * the numerically smallest slave MAC as its own. With the old
	 * hardcoded 02:fe:88:75:81:xx block that meant the bridge - and
	 * everything downstream sees as the gateway - carried a locally
	 * administered address that was also identical on every unit. Deriving
	 * from the factory label MAC instead keeps br-lan on the label address
	 * and keeps the block unique per board.
	 */
	ret = fmcs_get_base_mac(priv, base_mac);
	if (ret) {
		i = 0; /* err_unreg_fttr walks the already-registered ports back */
		goto err_unreg_fttr;
	}
	ret = fmcs_bridge_init(priv);
	if (ret) {
		i = 0;
		goto err_unreg_fttr;
	}

	/* Create downstream DSA-style switch ports fttr1 ~ fttr16 */
	for (i = 0; i < FMCS_MAX_FTTR_PORTS; i++) {
		char ifname[IFNAMSIZ];
		struct net_device *dev;
		struct fmcs_fttr_port_priv *port_priv;

		snprintf(ifname, sizeof(ifname), "fttr%d", i + 1);
		dev = alloc_netdev(sizeof(struct fmcs_fttr_port_priv), ifname,
				   NET_NAME_UNKNOWN, ether_setup);
		if (!dev) {
			ret = -ENOMEM;
			goto err_unreg_fttr;
		}

		dev->netdev_ops = &fmcs_fttr_ops;
		dev->priv_flags &= ~IFF_TX_SKB_SHARING;
		/* The transport and its receive handler live in init_net. */
		dev->netns_immutable = true;
		dev->min_mtu = ETH_MIN_MTU;
		dev->max_mtu = ETH_MAX_MTU;
		dev->needed_headroom = VLAN_HLEN;
		port_priv = netdev_priv(dev);
		port_priv->priv = priv;
		port_priv->port_idx = i + 1;

		/* fttr1 takes the DT cell verbatim, fttr2..fttr16 follow it */
		memcpy(port_mac, base_mac, ETH_ALEN);
		fmcs_mac_add(port_mac, i);
		eth_hw_addr_set(dev, port_mac);

		/* Default carrier is OFF until ONU establishes O5 link */
		netif_carrier_off(dev);

		ret = register_netdev(dev);
		if (ret) {
			free_netdev(dev);
			goto err_unreg_fttr;
		}

		priv->fttr_devs[i] = dev;
	}

	priv->fttr_netdev_nb.notifier_call = fmcs_netdev_event;
	ret = register_netdevice_notifier(&priv->fttr_netdev_nb);
	if (ret) {
		rtnl_lock();
		fmcs_lower_unlink(priv);
		rtnl_unlock();
		goto err_unreg_fttr;
	}
	ret = register_switchdev_notifier(&fmcs_bridge_nb);
	if (ret)
		goto err_notifier;
	ret = register_switchdev_blocking_notifier(&fmcs_bridge_blocking_nb);
	if (ret) {
		unregister_switchdev_notifier(&fmcs_bridge_nb);
		goto err_notifier;
	}
	rtnl_lock();
	fmcs_offload_priv = priv;
	rtnl_unlock();

	return 0;

err_notifier:
	unregister_netdevice_notifier(&priv->fttr_netdev_nb);
	rtnl_lock();
	fmcs_lower_unlink(priv);
	rtnl_unlock();
	i = FMCS_MAX_FTTR_PORTS;
err_unreg_fttr:
	while (--i >= 0) {
		if (priv->fttr_devs[i]) {
			unregister_netdev(priv->fttr_devs[i]);
			free_netdev(priv->fttr_devs[i]);
			priv->fttr_devs[i] = NULL;
		}
	}
	if (priv->bridge_offload)
		fmcs_bridge_exit(priv);
	unregister_netdev(priv->ploam_dev);
	free_netdev(priv->ploam_dev);
	priv->ploam_dev = NULL;
	unregister_netdev(priv->omci_dev);
	free_netdev(priv->omci_dev);
	priv->omci_dev = NULL;
	return ret;
}

void fmcs_netdevs_exit(struct fmcs_priv *priv)
{
	int i;

	unregister_switchdev_blocking_notifier(&fmcs_bridge_blocking_nb);
	unregister_switchdev_notifier(&fmcs_bridge_nb);
	fmcs_bridge_revoke();
	cancel_work_sync(&fmcs_bridge_gc_work);
	unregister_netdevice_notifier(&priv->fttr_netdev_nb);
	rtnl_lock();
	fmcs_offload_priv = NULL;
	fmcs_lower_unlink(priv);
	rtnl_unlock();

	for (i = 0; i < FMCS_MAX_FTTR_PORTS; i++) {
		if (priv->fttr_devs[i]) {
			unregister_netdev(priv->fttr_devs[i]);
			free_netdev(priv->fttr_devs[i]);
			priv->fttr_devs[i] = NULL;
		}
	}
	fmcs_bridge_exit(priv);

	if (priv->ploam_dev) {
		unregister_netdev(priv->ploam_dev);
		cancel_work_sync(&priv->ploam_tx_work);
		skb_queue_purge(&priv->ploam_tx_queue);
		free_netdev(priv->ploam_dev);
		priv->ploam_dev = NULL;
	}
	if (priv->omci_dev) {
		unregister_netdev(priv->omci_dev);
		free_netdev(priv->omci_dev);
		priv->omci_dev = NULL;
	}
}
