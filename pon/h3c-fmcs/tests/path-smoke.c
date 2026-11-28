// SPDX-License-Identifier: GPL-2.0
/* Target-kernel integration test; no packets or hardware rules are injected. */
#include <linux/module.h>
#include <linux/netdevice.h>
#include <linux/if_vlan.h>
#include <net/net_namespace.h>

static char *port = "fttr2";
static unsigned int vid = 257;
static bool enabled;
module_param(port, charp, 0444);
module_param(vid, uint, 0444);
module_param(enabled, bool, 0444);

static int __init path_smoke_init(void)
{
	struct net_device_path_stack stack;
	struct net_device_path_ctx ctx = {};
	struct net_device_path path = {};
	struct net_device *dev, *lower;
	u8 daddr[ETH_ALEN] = { 0x02, 0, 0, 0, 0, 1 };
	int ret, err = -EINVAL;

	dev = dev_get_by_name(&init_net, port);
	lower = dev_get_by_name(&init_net, "fttr0");
	if (!dev || !lower)
		goto out;
	rcu_read_lock();
	ret = dev_fill_forward_path(dev, daddr, &stack);
	if (!enabled) {
		err = ret < 0 ? 0 : -EINVAL;
		goto unlock;
	}
	if (ret || stack.num_paths != 2 ||
	    stack.path[0].type != DEV_PATH_VLAN ||
	    stack.path[0].dev != dev || stack.path[0].encap.id != vid ||
	    stack.path[0].encap.proto != htons(ETH_P_8021Q) ||
	    stack.path[1].type != DEV_PATH_ETHERNET ||
	    stack.path[1].dev != lower)
		goto unlock;
	ctx.dev = dev;
	ctx.num_vlans = 1;
	ctx.vlan[0].id = 20;
	ctx.vlan[0].proto = htons(ETH_P_8021Q);
	ret = dev->netdev_ops->ndo_fill_forward_path(&ctx, &path);
	if (ret || ctx.dev != lower || ctx.num_vlans != 2 ||
	    ctx.vlan[0].id != 20 || ctx.vlan[1].id != vid)
		goto unlock;
	ctx.dev = dev;
	/* A third on-wire VLAN must be rejected. */
	if (dev->netdev_ops->ndo_fill_forward_path(&ctx, &path) != -ENOSPC)
		goto unlock;
	ctx.num_vlans = 1;
	ctx.vlan[0].proto = htons(ETH_P_8021AD);
	if (dev->netdev_ops->ndo_fill_forward_path(&ctx, &path) != -EOPNOTSUPP)
		goto unlock;
	ctx.vlan[0].proto = htons(ETH_P_8021Q);
	ctx.vlan[0].id = 0;
	if (dev->netdev_ops->ndo_fill_forward_path(&ctx, &path) != -EOPNOTSUPP)
		goto unlock;
	err = 0;
unlock:
	rcu_read_unlock();
out:
	if (dev)
		dev_put(dev);
	if (lower)
		dev_put(lower);
	pr_info("fmcs path smoke: %s port=%s enabled=%d\n",
		err ? "FAIL" : "PASS", port, enabled);
	return err;
}

static void __exit path_smoke_exit(void) {}
module_init(path_smoke_init);
module_exit(path_smoke_exit);
MODULE_LICENSE("GPL");
MODULE_DESCRIPTION("FMCS standard VLAN path integration smoke test");
