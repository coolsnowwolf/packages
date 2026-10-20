#include <linux/kconfig.h>

#ifndef KBUILD_MODNAME
#define KBUILD_MODNAME "shadow_btf_private"
#endif
#ifndef KBUILD_BASENAME
#define KBUILD_BASENAME "shadow_btf_private"
#endif

#define BTF_KEEP(name, type) \
	static type * __attribute__((used, section(".discard.btf_keep"))) \
		__btf_keep_##name

#if IS_ENABLED(CONFIG_BRIDGE) && __has_include("net/bridge/br_private.h")
#include "net/bridge/br_private.h"
BTF_KEEP(net_bridge, struct net_bridge);
BTF_KEEP(net_bridge_port, struct net_bridge_port);
#endif

#if __has_include("fs/mount.h")
#include "fs/mount.h"
BTF_KEEP(mount, struct mount);
#endif
