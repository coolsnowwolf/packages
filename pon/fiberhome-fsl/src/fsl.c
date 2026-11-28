// SPDX-License-Identifier: GPL-2.0-only
// Bounded 0x1fbd4000 PIO transport only. FSL protocol/register/optical policy is userspace.
#include <linux/compat.h>
#include <linux/delay.h>
#include <linux/fs.h>
#include <linux/gpio/consumer.h>
#include <linux/io.h>
#include <linux/iopoll.h>
#include <linux/kref.h>
#include <linux/mfd/syscon.h>
#include <linux/miscdevice.h>
#include <linux/module.h>
#include <linux/mutex.h>
#include <linux/of.h>
#include <linux/platform_device.h>
#include <linux/regmap.h>
#include <linux/reset.h>
#include <linux/slab.h>
#include <linux/uaccess.h>

#include "fsl-uapi.h"

#define SPI_CTRL_CFG0		0x04
#define SPI_FIFO_CFG		0x14
#define SPI_BUSY_STS		0x18
#define SPI_MSG_EN		0x20
#define SPI_CMD_DONE		0x24
#define SPI_CMD_REG		0x28
#define SPI_CMD_BUSY		0x2c
#define SPI_CMD_TRIG		0x30
#define SPI_TX_BUSY		0x34
#define SPI_TX_DATA		0x38
#define SPI_RX_BUSY		0x3c
#define SPI_RX_TRIG		0x40
#define SPI_RX_DATA		0x44
#define SPI_PROBE_90		0x90
#define SPI_MODE_REG		0x98
#define SPI_CS_REG		0xe4
#define SPI_PROBE_130		0x130

#define SPI_OP_END		0x00
#define SPI_OP_START		0x01
#define SPI_OP_TX_FINAL		0x08
#define SPI_OP_RX		0x0c
#define SPI_OP_TX_CONT		0x10

#define SPI_MAX_XFER_LEN	0x1ff
#define SPI_POLL_TIMEOUT_US	10000

#define SCU_SPI_CLK_DIV_SEL	0x1c4
#define SCU_PCM_CLK_MASK	GENMASK(31, 16)
#define SCU_PCM_CLK_EN		BIT(16)

struct fsl_device {
	struct miscdevice misc;
	struct kref refs;
	/* Serializes remove against messages/open; opened supplies session-level
	 * exclusion without holding a mutex for a userspace session's lifetime.
	 */
	struct mutex lock;
	struct device *dev;
	void __iomem *base;
	struct regmap *scu;
	struct reset_control *rst;
	struct gpio_desc *reset_gpio;
	u32 max_speed_hz;
	u32 chip_select;
	bool opened;
};

static void fsl_free(struct kref *refs)
{
	struct fsl_device *fsl = container_of(refs, struct fsl_device, refs);

	mutex_destroy(&fsl->lock);
	kfree(fsl->misc.name);
	kfree(fsl);
}

/* SCU 0x1c4 divider field (div << 23 | BIT(16) enable) is reconstructed from
 * the stock fslstack code and confirmed on hardware; it is undocumented in
 * public datasheets. Keep the per-transfer write: SCU owns no other writer
 * today, but a future clock consumer must not wedge this bus silently.
 */
static int fsl_set_clk(struct fsl_device *fsl, u32 speed_hz)
{
	u32 mhz, div, val;
	int ret;

	if (!fsl->scu)
		return 0;

	if (!speed_hz)
		return -EINVAL;

	mhz = clamp_t(u32, speed_hz / 1000000U, 1U, 25U);
	div = DIV_ROUND_UP(100U, mhz);
	div = clamp_t(u32, div, 2U, 62U);
	val = (div << 23) | SCU_PCM_CLK_EN;

	ret = regmap_update_bits(fsl->scu, SCU_SPI_CLK_DIV_SEL,
				 SCU_PCM_CLK_MASK, val);
	if (ret)
		dev_err_ratelimited(fsl->dev, "scu clk set failed: %pe\n",
				    ERR_PTR(ret));
	return ret;
}

static int fsl_cmd(struct fsl_device *fsl, u32 opcode, u32 len)
{
	u32 val;
	int ret;

	if (len > SPI_MAX_XFER_LEN)
		return -EINVAL;

	ret = readl_poll_timeout(fsl->base + SPI_CMD_BUSY, val, val == 0,
				 1, SPI_POLL_TIMEOUT_US);
	if (ret) {
		dev_err_ratelimited(fsl->dev, "opfifo busy timeout (op %#x)\n",
				    opcode);
		return ret;
	}

	writel((opcode << 9) | (len & SPI_MAX_XFER_LEN),
	       fsl->base + SPI_CMD_REG);
	writel(1, fsl->base + SPI_CMD_TRIG);

	ret = readl_poll_timeout(fsl->base + SPI_CMD_DONE, val, val != 0,
				 1, SPI_POLL_TIMEOUT_US);
	if (ret)
		dev_err_ratelimited(fsl->dev,
				    "op %#x len %u done timeout\n", opcode, len);
	return ret;
}

static int fsl_tx(struct fsl_device *fsl, const u8 *buf, u32 len, bool cont)
{
	u32 val, i;
	int ret;

	ret = fsl_cmd(fsl, cont ? SPI_OP_TX_CONT : SPI_OP_TX_FINAL, len);
	if (ret)
		return ret;

	for (i = 0; i < len; i++) {
		ret = readl_poll_timeout(fsl->base + SPI_TX_BUSY, val,
					 val == 0, 1, SPI_POLL_TIMEOUT_US);
		if (ret) {
			dev_err_ratelimited(fsl->dev,
					    "tx fifo full timeout (byte %u/%u)\n",
					    i, len);
			return ret;
		}

		writel(buf[i], fsl->base + SPI_TX_DATA);

		ret = readl_poll_timeout(fsl->base + SPI_TX_BUSY, val,
					 val == 0, 1, SPI_POLL_TIMEOUT_US);
		if (ret) {
			dev_err_ratelimited(fsl->dev,
					    "tx fifo drain timeout (byte %u/%u)\n",
					    i, len);
			return ret;
		}
	}

	return 0;
}

static int fsl_rx(struct fsl_device *fsl, u8 *buf, u32 len)
{
	u32 val, i;
	int ret;

	ret = fsl_cmd(fsl, SPI_OP_RX, len);
	if (ret)
		return ret;

	for (i = 0; i < len; i++) {
		ret = readl_poll_timeout(fsl->base + SPI_RX_BUSY, val,
					 val == 0, 1, SPI_POLL_TIMEOUT_US);
		if (ret) {
			dev_err_ratelimited(fsl->dev,
					    "rx fifo empty timeout (byte %u/%u)\n",
					    i, len);
			return ret;
		}

		buf[i] = readl(fsl->base + SPI_RX_DATA) & 0xff;
		writel(1, fsl->base + SPI_RX_TRIG);
	}

	return 0;
}

static int fsl_hw_transfer(struct fsl_device *fsl, const u8 *tx, u32 tx_len,
			   u8 *rx, u32 rx_len)
{
	u32 val;
	int ret;

	writel(0, fsl->base + SPI_MODE_REG);
	writel(fsl->chip_select, fsl->base + SPI_CS_REG);

	writel(0, fsl->base + SPI_CTRL_CFG0);
	ret = readl_poll_timeout(fsl->base + SPI_BUSY_STS, val, val == 0,
				 1, SPI_POLL_TIMEOUT_US);
	if (ret) {
		dev_err_ratelimited(fsl->dev, "controller busy timeout\n");
		goto out_close;
	}

	writel(1, fsl->base + SPI_MSG_EN);
	writel(9, fsl->base + SPI_FIFO_CFG);

	ret = fsl_cmd(fsl, SPI_OP_START, 1);
	if (ret)
		goto out_close;

	ret = fsl_set_clk(fsl, fsl->max_speed_hz);
	if (ret)
		goto out_close;

	/* Vendor Image selects TX_FINAL (0x08) before an RX segment or end of message. */
	ret = fsl_tx(fsl, tx, tx_len, false);
	if (ret)
		goto out_close;

	if (rx_len) {
		ret = fsl_rx(fsl, rx, rx_len);
		if (ret)
			goto out_close;
	}

	udelay(20);
	ret = fsl_cmd(fsl, SPI_OP_END, 1);

out_close:
	writel(0, fsl->base + SPI_FIFO_CFG);
	writel(0, fsl->base + SPI_MSG_EN);
	writel(1, fsl->base + SPI_CTRL_CFG0);
	return ret;
}

static int fsl_open(struct inode *inode, struct file *file)
{
	struct fsl_device *fsl = container_of(file->private_data,
					    struct fsl_device, misc);
	int ret = 0;

	mutex_lock(&fsl->lock);
	if (!fsl->base) {
		ret = -ENODEV;
	} else if (fsl->opened) {
		ret = -EBUSY;
	} else {
		kref_get(&fsl->refs);
		fsl->opened = true;
		file->private_data = fsl;
	}
	mutex_unlock(&fsl->lock);
	if (!ret)
		return stream_open(inode, file);
	return ret;
}

static int fsl_release(struct inode *inode, struct file *file)
{
	struct fsl_device *fsl = file->private_data;

	mutex_lock(&fsl->lock);
	fsl->opened = false;
	mutex_unlock(&fsl->lock);
	kref_put(&fsl->refs, fsl_free);
	return 0;
}

static long fsl_ioctl(struct file *file, unsigned int command,
		      unsigned long argument)
{
	struct fsl_device *fsl = file->private_data;
	struct fsl_transfer *request;
	u8 *tx = NULL, *rx = NULL;
	void __user *user = (void __user *)argument;
	int ret;

	if (command != FSL_IOC_TRANSFER)
		return -ENOTTY;
	request = memdup_user(user, sizeof(*request));
	if (IS_ERR(request))
		return PTR_ERR(request);
	/* Check each length before adding, to avoid overflow. No FSL opcode or
	 * address semantics belong here. Reserved fields must be zero.
	 */
	if (!request->tx_len || request->tx_len > FSL_MAX_TRANSFER ||
	    request->rx_len > FSL_MAX_TRANSFER ||
	    request->tx_len + request->rx_len > FSL_MAX_TRANSFER ||
	    request->reserved[0] || request->reserved[1]) {
		ret = -EINVAL;
		goto out;
	}
	memset(request->rx, 0, sizeof(request->rx));
	tx = kmemdup(request->tx, request->tx_len, GFP_KERNEL);
	if (request->rx_len)
		rx = kzalloc(request->rx_len, GFP_KERNEL);
	if (!tx || (request->rx_len && !rx)) {
		ret = -ENOMEM;
		goto out;
	}

	mutex_lock(&fsl->lock);
	if (!fsl->base)
		ret = -ENODEV;
	else
		ret = fsl_hw_transfer(fsl, tx, request->tx_len, rx,
				      request->rx_len);
	mutex_unlock(&fsl->lock);

	if (!ret && request->rx_len)
		memcpy(request->rx, rx, request->rx_len);
	if (!ret && copy_to_user(user, request, sizeof(*request)))
		ret = -EFAULT;
out:
	kfree(rx);
	kfree(tx);
	kfree(request);
	return ret;
}

#ifdef CONFIG_COMPAT
static long fsl_compat_ioctl(struct file *file, unsigned int command,
			     unsigned long argument)
{
	return fsl_ioctl(file, command, (unsigned long)compat_ptr(argument));
}
#endif

static const struct file_operations fsl_fops = {
	.owner = THIS_MODULE,
	.open = fsl_open,
	.release = fsl_release,
	.unlocked_ioctl = fsl_ioctl,
#ifdef CONFIG_COMPAT
	.compat_ioctl = fsl_compat_ioctl,
#endif
};

static int fsl_probe(struct platform_device *pdev)
{
	struct fsl_device *fsl;
	u32 max_hz = 10000000;
	int ret;

	fsl = kzalloc(sizeof(*fsl), GFP_KERNEL);
	if (!fsl)
		return -ENOMEM;

	kref_init(&fsl->refs);
	mutex_init(&fsl->lock);
	fsl->dev = &pdev->dev;

	fsl->base = devm_platform_ioremap_resource(pdev, 0);
	if (IS_ERR(fsl->base)) {
		ret = dev_err_probe(&pdev->dev, PTR_ERR(fsl->base),
				    "cannot map registers\n");
		fsl->base = NULL;
		goto fail;
	}

	fsl->scu = syscon_regmap_lookup_by_phandle(pdev->dev.of_node,
						   "airoha,scu");
	if (IS_ERR(fsl->scu))
		fsl->scu = syscon_regmap_lookup_by_compatible(
			"airoha,en7581-chip-scu");
	if (PTR_ERR(fsl->scu) == -EPROBE_DEFER) {
		/* The SCU driver may simply not have probed yet; retrying
		 * gets the divider programmed instead of silently keeping
		 * the bootloader value.
		 */
		ret = dev_err_probe(&pdev->dev, -EPROBE_DEFER,
				    "SCU syscon not ready\n");
		goto fail;
	}
	if (IS_ERR(fsl->scu)) {
		fsl->scu = NULL;
		/* Non-fatal: transfers then run at whatever divider the
		 * bootloader left, so say so instead of failing silently.
		 */
		dev_warn(&pdev->dev,
			 "SCU syscon unavailable; SPI clock divider stays as configured by the bootloader\n");
	}

	fsl->rst = devm_reset_control_get_optional_exclusive(&pdev->dev, "pcm");
	if (IS_ERR(fsl->rst)) {
		ret = dev_err_probe(&pdev->dev, PTR_ERR(fsl->rst),
				    "cannot get pcm reset\n");
		goto fail;
	}
	if (fsl->rst) {
		ret = reset_control_deassert(fsl->rst);
		if (ret) {
			ret = dev_err_probe(&pdev->dev, ret,
					    "cannot deassert pcm reset\n");
			goto fail;
		}
	}

	/* Release active-low reset if wired to GPIO4; do not pulse on every open. */
	fsl->reset_gpio = devm_gpiod_get_optional(&pdev->dev, "reset",
						  GPIOD_OUT_LOW);
	if (IS_ERR(fsl->reset_gpio)) {
		ret = dev_err_probe(&pdev->dev, PTR_ERR(fsl->reset_gpio),
				    "cannot get reset gpio\n");
		goto fail;
	}

	of_property_read_u32(pdev->dev.of_node, "spi-max-frequency", &max_hz);
	if (!max_hz) {
		ret = dev_err_probe(&pdev->dev, -EINVAL,
				    "invalid spi-max-frequency\n");
		goto fail;
	}
	fsl->max_speed_hz = min(max_hz, 10000000U);
	fsl->chip_select = 0;

	writel(0, fsl->base + SPI_PROBE_90);
	writel(0, fsl->base + SPI_PROBE_130);

	fsl->misc.minor = MISC_DYNAMIC_MINOR;
	fsl->misc.name = kasprintf(GFP_KERNEL, "fsl-%s", dev_name(&pdev->dev));
	fsl->misc.fops = &fsl_fops;
	fsl->misc.parent = &pdev->dev;
	fsl->misc.mode = 0600;
	if (!fsl->misc.name) {
		ret = dev_err_probe(&pdev->dev, -ENOMEM,
				    "cannot allocate device name\n");
		goto fail;
	}

	ret = misc_register(&fsl->misc);
	if (ret) {
		dev_err_probe(&pdev->dev, ret, "cannot register misc device\n");
		goto fail;
	}

	platform_set_drvdata(pdev, fsl);
	dev_info(&pdev->dev, "registered /dev/%s (max_hz=%u)\n",
		 fsl->misc.name, fsl->max_speed_hz);
	return 0;

fail:
	fsl->base = NULL;
	kref_put(&fsl->refs, fsl_free);
	return ret;
}

static void fsl_remove(struct platform_device *pdev)
{
	struct fsl_device *fsl = platform_get_drvdata(pdev);

	/* No new opens; existing files keep their own ref. Wait for an active
	 * transfer before detaching the MMIO pointer. Late ioctls return ENODEV.
	 */
	misc_deregister(&fsl->misc);
	mutex_lock(&fsl->lock);
	fsl->base = NULL;
	mutex_unlock(&fsl->lock);
	kref_put(&fsl->refs, fsl_free);
}

static const struct of_device_id fsl_of_match[] = {
	{ .compatible = "fiberhome,an4475-fsl" },
	{}
};
MODULE_DEVICE_TABLE(of, fsl_of_match);

static struct platform_driver fsl_driver = {
	.driver = {
		.name = "fiberhome-fsl",
		.of_match_table = fsl_of_match,
	},
	.probe = fsl_probe,
	.remove = fsl_remove,
};
module_platform_driver(fsl_driver);

MODULE_DESCRIPTION("FiberHome FSL exclusive 0x1fbd4000 transport (protocol in userspace)");
MODULE_AUTHOR("openwrt-fttr contributors");
MODULE_LICENSE("GPL");
