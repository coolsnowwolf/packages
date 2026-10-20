#include <linux/kconfig.h>

#ifndef KBUILD_MODNAME
#define KBUILD_MODNAME "shadow_btf_core"
#endif
#ifndef KBUILD_BASENAME
#define KBUILD_BASENAME "shadow_btf_core"
#endif

#include <linux/types.h>
#include <linux/ptrace.h>
#include <linux/sched.h>
#include <linux/mm_types.h>
#include <linux/fs.h>
#include <linux/dcache.h>
#include <linux/cred.h>
#include <linux/module.h>
#include <linux/skbuff.h>
#include <linux/netdevice.h>
#include <linux/if_ether.h>
#include <linux/ip.h>
#include <linux/tcp.h>
#include <linux/udp.h>
#include <linux/icmp.h>
#include <linux/bpf.h>
#include <linux/bpf_perf_event.h>
#include <net/sock.h>
#include <net/inet_sock.h>
#include <net/inet_connection_sock.h>
#include <net/net_namespace.h>
#include <net/dst.h>
#include <net/neighbour.h>
#include <net/xdp.h>
#include <net/ip.h>

#if __has_include(<net/netdev_rx_queue.h>)
#include <net/netdev_rx_queue.h>
BTF_KEEP(netdev_rx_queue, struct netdev_rx_queue);
#endif

#if __has_include(<linux/bpf_iter.h>)
#include <linux/bpf_iter.h>
#endif

#if IS_ENABLED(CONFIG_CGROUPS)
#include <linux/cgroup.h>
#endif

#define BTF_KEEP(name, type) \
	static type * __attribute__((used, section(".discard.btf_keep"))) \
		__btf_keep_##name

BTF_KEEP(pt_regs, struct pt_regs);
BTF_KEEP(task_struct, struct task_struct);
BTF_KEEP(mm_struct, struct mm_struct);
BTF_KEEP(file, struct file);
BTF_KEEP(dentry, struct dentry);
BTF_KEEP(inode, struct inode);
BTF_KEEP(cred, struct cred);
BTF_KEEP(module, struct module);
BTF_KEEP(sk_buff, struct sk_buff);
BTF_KEEP(net_device, struct net_device);
BTF_KEEP(netdev_queue, struct netdev_queue);
BTF_KEEP(sock, struct sock);
BTF_KEEP(inet_sock, struct inet_sock);
BTF_KEEP(inet_connection_sock, struct inet_connection_sock);
BTF_KEEP(tcp_sock, struct tcp_sock);
BTF_KEEP(udp_sock, struct udp_sock);
BTF_KEEP(net, struct net);
BTF_KEEP(dst_entry, struct dst_entry);
BTF_KEEP(neighbour, struct neighbour);
BTF_KEEP(xdp_buff, struct xdp_buff);
BTF_KEEP(bpf_perf_event_data, struct bpf_perf_event_data);
#if IS_ENABLED(CONFIG_CGROUPS)
BTF_KEEP(cgroup, struct cgroup);
#endif
