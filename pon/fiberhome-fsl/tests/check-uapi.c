/* SPDX-License-Identifier: GPL-2.0-only */
/* Cross-language/compat ABI contract: also asserted by the Rust transport tests.
 * Compile with host cc, aarch64-linux-gnu-gcc, and a 32-bit compiler when present.
 */
#include <stddef.h>
#include "../src/fsl-uapi.h"

_Static_assert(sizeof(struct fsl_transfer) == 272, "native/compat struct size");
_Static_assert(offsetof(struct fsl_transfer, tx) == 16, "TX ABI offset");
_Static_assert(offsetof(struct fsl_transfer, rx) == 144, "RX ABI offset");
_Static_assert(FSL_IOC_TRANSFER == 0xc1104601U, "Rust ioctl command contract");

int main(void)
{
	return 0;
}
