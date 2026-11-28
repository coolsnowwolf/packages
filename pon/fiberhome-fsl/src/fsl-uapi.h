/* SPDX-License-Identifier: GPL-2.0-only WITH Linux-syscall-note */
#ifndef FIBERHOME_FSL_UAPI_H
#define FIBERHOME_FSL_UAPI_H

#include <linux/ioctl.h>
#include <linux/types.h>

/* Version 1: inline bytes, no native pointers; identical compat/native ABI.
 * One message: TX followed by optional RX, with CS held between transfers.
 * Lengths are bytes; tx_len + rx_len <= FSL_MAX_TRANSFER. Reserved must be 0.
 * ioctl returns 0 only after a complete transfer; negative errno otherwise.
 * An error can have an unknown hardware outcome: callers must not replay it.
 */
#define FSL_MAX_TRANSFER 128
struct fsl_transfer {
	__u32 tx_len;
	__u32 rx_len;
	__u32 reserved[2];
	__u8 tx[FSL_MAX_TRANSFER];
	__u8 rx[FSL_MAX_TRANSFER];
};

#define FSL_IOC_TRANSFER _IOWR('F', 1, struct fsl_transfer)

#endif
