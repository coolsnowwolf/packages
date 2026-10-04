/* SPDX-License-Identifier: GPL-2.0-only
 * Copyright (C) 2026 coolsnowwolf <coolsnowwolf@gmail.com>
 * One local hostname, UDP/137 only. Wire format: RFC 1002 section 4.2.
 */
#define _GNU_SOURCE
#include <arpa/inet.h>
#include <errno.h>
#include <net/if.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/socket.h>
#include <time.h>
#include <unistd.h>

struct entry {
	uint8_t name[15];
	uint8_t workgroup[15];
	struct in_addr ip;
	uint8_t mac[6];
};

static volatile sig_atomic_t stopped;

static void stop(int sig)
{
	(void)sig;
	stopped = 1;
}

static uint16_t get16(const uint8_t *p)
{
	return (uint16_t)p[0] << 8 | p[1];
}

static void put16(uint8_t *p, uint16_t v)
{
	p[0] = v >> 8;
	p[1] = v;
}

static int set_name(uint8_t out[15], const char *s)
{
	size_t n = strlen(s), i;

	if (!n || n > 15)
		return -1;
	memset(out, ' ', 15);
	for (i = 0; i < n; i++) {
		unsigned char c = s[i];
		if (!((c >= 'a' && c <= 'z') || (c >= 'A' && c <= 'Z') ||
		      (c >= '0' && c <= '9') || c == '-' || c == '_'))
			return -1;
		out[i] = c >= 'a' && c <= 'z' ? c - 'a' + 'A' : c;
	}
	return 0;
}

static void encode(uint8_t *out, const uint8_t name[16])
{
	unsigned int i;

	out[0] = 32;
	for (i = 0; i < 16; i++) {
		out[1 + i * 2] = 'A' + (name[i] >> 4);
		out[2 + i * 2] = 'A' + (name[i] & 15);
	}
	out[33] = 0;
}

/* Fixed-size, unscoped, single-question requests only. Reject compression,
 * extra records and trailing data instead of following untrusted pointers.
 * The output buffer must hold 159 bytes; no packet causes an allocation.
 */
static size_t respond(const uint8_t *in, size_t len, uint8_t out[159],
		      const struct entry *e)
{
	static const uint8_t wildcard[16] = { '*' };
	uint8_t name[16];
	uint16_t flags, type;
	unsigned int i;
	int wild, own;
	size_t rdlen;

	if (len != 50 || in[12] != 32 || in[45] != 0)
		return 0;
	flags = get16(in + 2);
	/* Only RD and broadcast flags are meaningful in a request. */
	if ((flags & ~0x0110) || get16(in + 4) != 1 ||
	    get16(in + 6) || get16(in + 8) || get16(in + 10) ||
	    get16(in + 48) != 1)
		return 0;
	type = get16(in + 46);
	if (type != 0x20 && type != 0x21)
		return 0;
	for (i = 0; i < 16; i++) {
		uint8_t hi = in[13 + i * 2], lo = in[14 + i * 2];
		if (hi < 'A' || hi > 'P' || lo < 'A' || lo > 'P')
			return 0;
		name[i] = ((hi - 'A') << 4) | (lo - 'A');
	}
	wild = !memcmp(name, wildcard, 16);
	for (i = 0; i < 15; i++)
		if (name[i] >= 'a' && name[i] <= 'z')
			name[i] -= 'a' - 'A';
	own = !memcmp(name, e->name, 15) && (name[15] == 0 || name[15] == 0x20);
	/* libdsm discovery sends wildcard NB first, then wildcard NBSTAT. */
	if (!own && !wild)
		return 0;

	memset(out, 0, 159);
	memcpy(out, in, 2);
	put16(out + 2, 0x8400 | (type == 0x20 ? flags & 0x0100 : 0));
	put16(out + 6, 1);
	encode(out + 12, name);
	put16(out + 46, type);
	put16(out + 48, 1);
	if (type == 0x20) {
		/* Cache the address for 300 seconds; B-node, unique name. */
		put16(out + 52, 300);
		rdlen = 6;
		memcpy(out + 58, &e->ip.s_addr, 4);
	} else {
		/* Three status names and the RFC's 46-byte statistics block. */
		rdlen = 1 + 3 * 18 + 46;
		out[56] = 3;
		memcpy(out + 57, e->name, 15);
		put16(out + 73, 0x0400); /* ACTIVE, unique workstation */
		memcpy(out + 75, e->name, 15);
		out[90] = 0x20;
		put16(out + 91, 0x0400); /* ACTIVE, unique file server */
		memcpy(out + 93, e->workgroup, 15);
		put16(out + 109, 0x8400); /* ACTIVE, group */
		memcpy(out + 111, e->mac, 6);
	}
	put16(out + 54, rdlen);
	return 56 + rdlen;
}

int main(int argc, char **argv)
{
	struct entry e = { 0 };
	struct ifreq ifr = { 0 };
	struct in_addr mask;
	struct sockaddr_in local = { .sin_family = AF_INET, .sin_port = htons(137) };
	struct sigaction sa = { .sa_handler = stop };
	const char *device = NULL, *name = NULL, *workgroup = "WORKGROUP";
	unsigned int ifindex, tokens = 100;
	struct timespec last;
	int fd, opt, one = 1, result = 1;

	while ((opt = getopt(argc, argv, "i:n:w:h")) != -1) {
		switch (opt) {
		case 'i': device = optarg; break;
		case 'n': name = optarg; break;
		case 'w': workgroup = optarg; break;
		default:
			fprintf(stderr, "Usage: nbnsd -i DEVICE -n NAME [-w WORKGROUP]\n");
			return opt == 'h' ? 0 : 1;
		}
	}
	if (!device || !name || optind != argc || strlen(device) >= IFNAMSIZ ||
	    set_name(e.name, name) || set_name(e.workgroup, workgroup)) {
		fprintf(stderr, "nbnsd: specify an interface and 1-15 character names (ASCII letters, digits, - or _)\n");
		return 1;
	}
	fd = socket(AF_INET, SOCK_DGRAM | SOCK_CLOEXEC, 0);
	if (fd < 0) {
		perror("nbnsd: socket");
		return 1;
	}
	strcpy(ifr.ifr_name, device);
	if (ioctl(fd, SIOCGIFINDEX, &ifr))
		goto error;
	ifindex = ifr.ifr_ifindex;
	if (ioctl(fd, SIOCGIFADDR, &ifr))
		goto error;
	e.ip = ((struct sockaddr_in *)&ifr.ifr_addr)->sin_addr;
	if (ioctl(fd, SIOCGIFNETMASK, &ifr))
		goto error;
	mask = ((struct sockaddr_in *)&ifr.ifr_netmask)->sin_addr;
	if (ioctl(fd, SIOCGIFHWADDR, &ifr))
		goto error;
	memcpy(e.mac, ifr.ifr_hwaddr.sa_data, 6);
	/* Wildcard bind receives broadcasts, device binding excludes WAN.
	 * Deliberately no SO_REUSEADDR: an existing nmbd must cause failure.
	 */
	if (setsockopt(fd, SOL_SOCKET, SO_BINDTODEVICE, device, strlen(device) + 1) ||
	    setsockopt(fd, IPPROTO_IP, IP_PKTINFO, &one, sizeof(one)) ||
	    bind(fd, (struct sockaddr *)&local, sizeof(local)))
		goto error;
	sigemptyset(&sa.sa_mask);
	if (sigaction(SIGTERM, &sa, NULL) || sigaction(SIGINT, &sa, NULL))
		goto error;
	clock_gettime(CLOCK_MONOTONIC, &last);
	fprintf(stderr, "nbnsd: %.15s at %s on %s UDP/137\n", e.name, inet_ntoa(e.ip), device);
	while (!stopped) {
		uint8_t in[512], out[159];
		union { struct cmsghdr align; char data[CMSG_SPACE(sizeof(struct in_pktinfo))]; } control;
		struct sockaddr_in peer;
		struct iovec iov = { .iov_base = in, .iov_len = sizeof(in) };
		struct msghdr msg = { .msg_name = &peer, .msg_namelen = sizeof(peer),
			.msg_iov = &iov, .msg_iovlen = 1,
			.msg_control = control.data, .msg_controllen = sizeof(control.data) };
		struct cmsghdr *cmsg;
		struct in_pktinfo *info = NULL;
		struct timespec now;
		ssize_t n;
		size_t size;
		uint32_t source;

		n = recvmsg(fd, &msg, 0);
		if (n < 0) {
			if (errno == EINTR)
				continue;
			goto error;
		}
		if (msg.msg_flags & (MSG_TRUNC | MSG_CTRUNC))
			continue;
		for (cmsg = CMSG_FIRSTHDR(&msg); cmsg; cmsg = CMSG_NXTHDR(&msg, cmsg))
			if (cmsg->cmsg_level == IPPROTO_IP && cmsg->cmsg_type == IP_PKTINFO &&
			    cmsg->cmsg_len >= CMSG_LEN(sizeof(*info)))
				info = (struct in_pktinfo *)CMSG_DATA(cmsg);
		if (!info || (unsigned int)info->ipi_ifindex != ifindex)
			continue;
		source = ntohl(peer.sin_addr.s_addr);
		if (!peer.sin_port || !source || source >= 0xe0000000 ||
		    ((peer.sin_addr.s_addr ^ e.ip.s_addr) & mask.s_addr) ||
		    (mask.s_addr != 0xffffffff &&
		     ((peer.sin_addr.s_addr & ~mask.s_addr) == 0 ||
		      (peer.sin_addr.s_addr & ~mask.s_addr) == ~mask.s_addr)))
			continue;
		size = respond(in, n, out, &e);
		if (!size)
			continue;
		/* Bound broadcast response traffic to 100 packets per second. */
		clock_gettime(CLOCK_MONOTONIC, &now);
		if (now.tv_sec > last.tv_sec) {
			tokens = 100;
			last = now;
		}
		if (!tokens)
			continue;
		tokens--;
		iov.iov_base = out;
		iov.iov_len = size;
		memset(&control, 0, sizeof(control));
		msg.msg_controllen = sizeof(control.data);
		cmsg = CMSG_FIRSTHDR(&msg);
		cmsg->cmsg_level = IPPROTO_IP;
		cmsg->cmsg_type = IP_PKTINFO;
		cmsg->cmsg_len = CMSG_LEN(sizeof(*info));
		info = (struct in_pktinfo *)CMSG_DATA(cmsg);
		info->ipi_ifindex = ifindex;
		info->ipi_spec_dst = e.ip;
		/* A vanished peer or route must not terminate the daemon. */
		(void)sendmsg(fd, &msg, MSG_DONTWAIT);
	}
	result = 0;
	goto done;
error:
	perror("nbnsd");
done:
	close(fd);
	return result;
}
