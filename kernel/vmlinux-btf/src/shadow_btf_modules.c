#include <linux/kconfig.h>

#ifndef KBUILD_MODNAME
#define KBUILD_MODNAME "shadow_btf_modules"
#endif
#ifndef KBUILD_BASENAME
#define KBUILD_BASENAME "shadow_btf_modules"
#endif

#include <linux/types.h>

#define BTF_KEEP(name, type) \
	static type * __attribute__((used, section(".discard.btf_keep"))) \
		__btf_keep_##name

#if IS_ENABLED(CONFIG_INET)
#include <net/route.h>
#include <net/ip_fib.h>
BTF_KEEP(rtable, struct rtable);
#endif

#if IS_ENABLED(CONFIG_IPV6)
#include <net/ipv6.h>
#include <net/if_inet6.h>
BTF_KEEP(ipv6_pinfo, struct ipv6_pinfo);
#endif

#if IS_ENABLED(CONFIG_NETFILTER)
#include <linux/netfilter.h>
BTF_KEEP(nf_hook_state, struct nf_hook_state);
#endif

#if IS_ENABLED(CONFIG_NF_CONNTRACK)
#include <net/netfilter/nf_conntrack.h>
#include <net/netfilter/nf_conntrack_tuple.h>
BTF_KEEP(nf_conn, struct nf_conn);
BTF_KEEP(nf_conntrack_tuple, struct nf_conntrack_tuple);
#endif

#if IS_ENABLED(CONFIG_NF_NAT) && __has_include(<net/netfilter/nf_nat.h>)
#include <net/netfilter/nf_nat.h>
BTF_KEEP(nf_conn_nat, struct nf_conn_nat);
#endif

#if IS_ENABLED(CONFIG_NF_TABLES) && __has_include(<net/netfilter/nf_tables.h>)
#include <net/netfilter/nf_tables.h>
BTF_KEEP(nft_pktinfo, struct nft_pktinfo);
#endif

#if IS_ENABLED(CONFIG_NF_FLOW_TABLE) && __has_include(<net/netfilter/nf_flow_table.h>)
#include <net/netfilter/nf_flow_table.h>
BTF_KEEP(flow_offload, struct flow_offload);
#endif

#if IS_ENABLED(CONFIG_NETFILTER_XTABLES)
#include <linux/netfilter/x_tables.h>
BTF_KEEP(xt_table, struct xt_table);
BTF_KEEP(xt_action_param, struct xt_action_param);
#endif

#if IS_ENABLED(CONFIG_NET_SCHED)
#include <net/sch_generic.h>
#include <net/pkt_sched.h>
BTF_KEEP(qdisc, struct Qdisc);
#endif

#if IS_ENABLED(CONFIG_NET_CLS) && __has_include(<net/pkt_cls.h>)
#include <net/pkt_cls.h>
BTF_KEEP(tcf_result, struct tcf_result);
#endif

#if IS_ENABLED(CONFIG_NET_CLS_ACT) && __has_include(<net/act_api.h>)
#include <net/act_api.h>
#endif

#if IS_ENABLED(CONFIG_XDP_SOCKETS) && __has_include(<net/xdp_sock.h>)
#include <net/xdp_sock.h>
BTF_KEEP(xdp_sock, struct xdp_sock);
#endif
