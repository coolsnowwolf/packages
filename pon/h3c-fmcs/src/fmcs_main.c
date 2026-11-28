/*
 * H3C HM2004-DU Micro-OLT FPGA Management Driver (FMCS)
 *
 * Core driver lifecycle, IOCTL interface, FPGA bitstream loading, and IRQ.
 */

#include <linux/module.h>
#include <linux/init.h>
#include <linux/platform_device.h>
#include <linux/firmware.h>
#include <linux/gpio/consumer.h>
#include <linux/gpio/machine.h>
#include <linux/delay.h>
#include <linux/fs.h>
#include <linux/uaccess.h>
#include <linux/wait.h>
#include <linux/io.h>
#include <linux/of.h>
#include <linux/of_platform.h>
#include <linux/mod_devicetable.h>
#include "fmcs.h"

static struct fmcs_priv *g_fmcs_priv;

/* All queue operations and hardware collectors hold ploam_rx_lock. */
static void fmcs_ploam_flush_locked(struct fmcs_priv *priv)
{
	priv->rx_flushed += priv->ploam_rx_count;
	priv->ploam_rx_head = priv->ploam_rx_count = 0;
}

static void fmcs_ploam_enqueue_locked(struct fmcs_priv *priv,
				     const struct fmcs_ploam_msg *msg)
{
	u32 tail;

	priv->rx_received++;
	if (priv->ploam_rx_count == FMCS_PLOAM_RX_DEPTH) {
		priv->rx_dropped++;
		return;
	}
	tail = (priv->ploam_rx_head + priv->ploam_rx_count) % FMCS_PLOAM_RX_DEPTH;
	priv->ploam_rx_queue[tail] = *msg;
	priv->ploam_rx_count++;
	priv->rx_enqueued++;
}

static int fmcs_ploam_dequeue(struct fmcs_priv *priv, void __user *dest)
{
	struct fmcs_ploam_msg empty = { 0 };
	const struct fmcs_ploam_msg *msg;
	int ret = 0;

	mutex_lock(&priv->ploam_rx_lock);
	msg = priv->ploam_rx_count ? &priv->ploam_rx_queue[priv->ploam_rx_head] : &empty;
	/* Do not consume the head if the caller's buffer is invalid. */
	if (copy_to_user(dest, msg, sizeof(*msg))) {
		priv->rx_copy_faults++;
		ret = -EFAULT;
	} else if (priv->ploam_rx_count) {
		priv->ploam_rx_head = (priv->ploam_rx_head + 1) % FMCS_PLOAM_RX_DEPTH;
		priv->ploam_rx_count--;
		priv->rx_dequeued++;
	}
	mutex_unlock(&priv->ploam_rx_lock);
	return ret;
}

static unsigned int fmcs_ploam_collect(struct fmcs_priv *priv, bool irq)
{
	unsigned int n = 0;
	u32 events;

	mutex_lock(&priv->ploam_rx_lock);
	if (irq)
		priv->rx_irqs++;
	if (atomic_read(&priv->fpga_status) != FMCS_FPGA_STATUS_READY)
		goto out;
	if (irq) {
		/* Read raw register 0xb0; bit 0 indicates PLOAM. */
		if (fmcs_spi_read_fpga_reg(priv, 0xb0, &events)) {
			priv->rx_errors++;
			goto out;
		}
		if (events)
			priv->rx_last_events = events;
		if (!(events & 1))
			goto out;
	}
	/* Polling is a fallback even if an event edge was missed. Each SPI
	 * transaction releases the shared NAND bus; never drain unboundedly. */
	for (; n < FMCS_PLOAM_RX_BUDGET; n++) {
		struct fmcs_ploam_msg msg = { 0 };
		size_t len = 0;

		if (fmcs_spi_recv_ploam(priv, msg.data, &len)) {
			priv->rx_errors++;
			break;
		}
		if (!len) {
			priv->rx_empty++;
			break;
		}
		msg.len = len;
		fmcs_ploam_enqueue_locked(priv, &msg);
		/* Network delivery is a copy, never a competing hardware read. */
		fmcs_rx_ploam_frame(priv, msg.data, msg.len);
	}
	if (n == FMCS_PLOAM_RX_BUDGET)
		priv->rx_budget_hits++;
out:
	mutex_unlock(&priv->ploam_rx_lock);
	return n;
}

static void fmcs_ploam_rx_work(struct work_struct *work)
{
	struct fmcs_priv *priv = container_of(to_delayed_work(work),
					    struct fmcs_priv, ploam_rx_work);

	fmcs_ploam_collect(priv, false);
	schedule_delayed_work(&priv->ploam_rx_work, msecs_to_jiffies(10));
}

static ssize_t ploam_rx_stats_show(struct device *dev,
				  struct device_attribute *attr, char *buf)
{
	struct fmcs_priv *priv = dev_get_drvdata(dev);
	ssize_t len;

	mutex_lock(&priv->ploam_rx_lock);
	len = sysfs_emit(buf, "received=%llu enqueued=%llu dequeued=%llu depth=%u dropped=%llu flushed=%llu empty=%llu errors=%llu copy_faults=%llu budget_hits=%llu irqs=%llu last_events=0x%08x\n",
		priv->rx_received, priv->rx_enqueued, priv->rx_dequeued,
		priv->ploam_rx_count, priv->rx_dropped, priv->rx_flushed,
		priv->rx_empty, priv->rx_errors, priv->rx_copy_faults,
		priv->rx_budget_hits, priv->rx_irqs, priv->rx_last_events);
	mutex_unlock(&priv->ploam_rx_lock);
	return len;
}
static DEVICE_ATTR_RO(ploam_rx_stats);

static int fmcs_dev_open(struct inode *inode, struct file *file)
{
	file->private_data = g_fmcs_priv;
	return file->private_data ? 0 : -ENODEV;
}

static int fmcs_dev_release(struct inode *inode, struct file *file)
{
	return 0;
}

static long fmcs_dev_ioctl(struct file *file, unsigned int cmd, unsigned long arg)
{
	struct fmcs_priv *priv = file->private_data;
	int ret = 0;

	if (_IOC_TYPE(cmd) != FMCS_IOC_MAGIC)
		return -ENOTTY;

	if (!priv)
		return -ENODEV;

	if (cmd != FMCS_IOC_GET_FPGA_STATUS && cmd != FMCS_IOC_SET_CARRIER) {
		int st = atomic_read(&priv->fpga_status);
		if (st != FMCS_FPGA_STATUS_READY && st != FMCS_FPGA_STATUS_FAILED)
			return -EBUSY;
		if (st == FMCS_FPGA_STATUS_FAILED)
			return -EIO;
	}

	switch (cmd) {
	case FMCS_IOC_OMCI_CHANNEL: {
		struct fmcs_reg_op op;
		u32 channel;
		if (copy_from_user(&op, (void __user *)arg, sizeof(op)))
			return -EFAULT;
		if (op.addr > 15 || (op.val && op.val != 0x100 + op.addr))
			return -EINVAL;
		channel = op.val;
		mutex_lock(&priv->lock);
		op.val = READ_ONCE(priv->omci_channel[op.addr]);
		if (copy_to_user((void __user *)arg, &op, sizeof(op)))
			ret = -EFAULT;
		else
			WRITE_ONCE(priv->omci_channel[op.addr], channel);
		mutex_unlock(&priv->lock);
		break;
	}
	case FMCS_IOC_GET_FPGA_STATUS: {
		__u32 st = atomic_read(&priv->fpga_status);
		if (copy_to_user((void __user *)arg, &st, sizeof(st)))
			ret = -EFAULT;
		else
			ret = 0;
		break;
	}
	case FMCS_IOC_WRITE_FPGA_REG: {
		struct fmcs_reg_op op;
		if (copy_from_user(&op, (void __user *)arg, sizeof(op))) {
			ret = -EFAULT;
			break;
		}
		ret = fmcs_spi_write_fpga_reg(priv, op.addr, op.val);
		break;
	}
	case FMCS_IOC_READ_FPGA_REG: {
		struct fmcs_reg_op op;
		if (copy_from_user(&op, (void __user *)arg, sizeof(op))) {
			ret = -EFAULT;
			break;
		}
		ret = fmcs_spi_read_fpga_reg(priv, op.addr, &op.val);
		if (!ret && copy_to_user((void __user *)arg, &op, sizeof(op)))
			ret = -EFAULT;
		break;
	}
	case FMCS_IOC_WRITE_BOSA_REG: {
		struct fmcs_bosa_op op;
		if (copy_from_user(&op, (void __user *)arg, sizeof(op))) {
			ret = -EFAULT;
			break;
		}
		ret = fmcs_spi_write_bosa_reg(priv, op.addr, op.val);
		break;
	}
	case FMCS_IOC_READ_BOSA_REG: {
		struct fmcs_bosa_op op;
		if (copy_from_user(&op, (void __user *)arg, sizeof(op))) {
			ret = -EFAULT;
			break;
		}
		ret = fmcs_spi_read_bosa_reg(priv, op.addr, &op.val);
		if (!ret && copy_to_user((void __user *)arg, &op, sizeof(op)))
			ret = -EFAULT;
		break;
	}
	case FMCS_IOC_DEV_ATTR_MODIFY: {
		/* Used for setting debug flags and multicast GEM ports */
		break;
	}
	case FMCS_IOC_RECV_PLOAM: {
		ret = fmcs_ploam_dequeue(priv, (void __user *)arg);
		break;
	}
	case FMCS_IOC_SEND_PACKET: {
		struct fmcs_ploam_msg msg;
		if (copy_from_user(&msg, (void __user *)arg, sizeof(msg))) {
			ret = -EFAULT;
			break;
		}
		if (msg.len != 13) {
			ret = -EINVAL;
			break;
		}
		ret = fmcs_spi_send_ploam(priv, msg.data, msg.len);
		break;
	}
	case FMCS_IOC_SET_CARRIER: {
		struct fmcs_carrier_req req;
		if (copy_from_user(&req, (void __user *)arg, sizeof(req))) {
			ret = -EFAULT;
			break;
		}
		ret = fmcs_fttr_set_carrier(priv, req.port, !!req.carrier);
		break;
	}
	default:
		ret = -EINVAL;
		break;
	}

	return ret;
}

static const struct file_operations fmcs_fops = {
	.owner          = THIS_MODULE,
	.open           = fmcs_dev_open,
	.release        = fmcs_dev_release,
	.unlocked_ioctl = fmcs_dev_ioctl,
};

static irqreturn_t fmcs_irq_handler(int irq, void *dev_id)
{
	struct fmcs_priv *priv = dev_id;

	/* Bound repeated level interrupts with no PLOAM work. Do not guess
	 * a hardware ACK: non-PLOAM event handling is investigated separately. */
	if (!fmcs_ploam_collect(priv, true))
		msleep(10);

	return IRQ_HANDLED;
}

/* Older HM2004 DTBs expose only clock/data/IRQ. FPGA programming also
 * requires GPIO 5 (configuration reset), 22 (enable), 27 (INIT), 28 (DONE).
 * Keep this compatibility mapping restricted to the verified board.
 */
static struct gpiod_lookup_table hm2004_fpga_gpios = {
	.dev_id = "fmcs",
	.table = {
		GPIO_LOOKUP("1fbf0200.system-controller:pinctrl", 5, "fpga-config", GPIO_ACTIVE_HIGH),
		GPIO_LOOKUP("1fbf0200.system-controller:pinctrl", 22, "fpga-enable", GPIO_ACTIVE_HIGH),
		GPIO_LOOKUP("1fbf0200.system-controller:pinctrl", 27, "fpga-init", GPIO_ACTIVE_HIGH),
		GPIO_LOOKUP("1fbf0200.system-controller:pinctrl", 28, "fpga-done", GPIO_ACTIVE_HIGH),
		{ }
	},
};

static int fmcs_load_fpga_bitstream(struct fmcs_priv *priv, const char *fw_name)
{
	const struct firmware *fw;
	size_t i;
	int bit;
	int ret;

	if (!priv->gpiod_clk || !priv->gpiod_data || !priv->gpiod_config ||
	    !priv->gpiod_enable || !priv->gpiod_init || !priv->gpiod_done) {
		dev_warn(priv->dev, "FPGA programming GPIOs not configured\n");
		atomic_set(&priv->fpga_status, FMCS_FPGA_STATUS_FAILED);
		priv->fpga_error = -ENODEV;
		return -ENODEV;
	}

	/* Serialize the transition with all existing SPI operations. */
	mutex_lock(&priv->ploam_rx_lock);
	mutex_lock(&priv->lock);
	if (atomic_xchg(&priv->fpga_status, FMCS_FPGA_STATUS_PROGRAMMING) == FMCS_FPGA_STATUS_PROGRAMMING) {
		mutex_unlock(&priv->lock);
		mutex_unlock(&priv->ploam_rx_lock);
		dev_warn(priv->dev, "FPGA bitstream programming already in progress, ignoring duplicate request\n");
		return -EBUSY;
	}

	mutex_unlock(&priv->lock);
	fmcs_ploam_flush_locked(priv);
	mutex_unlock(&priv->ploam_rx_lock);
	priv->fpga_error = 0;

	ret = request_firmware(&fw, fw_name, priv->dev);
	if (ret) {
		dev_warn(priv->dev, "FPGA bitstream %s not found (%d)\n", fw_name, ret);
		atomic_set(&priv->fpga_status, FMCS_FPGA_STATUS_FAILED);
		priv->fpga_error = ret;
		return ret;
	}

	dev_info(priv->dev, "Programming FPGA bitstream %s (%zu bytes) in background...\n", fw_name, fw->size);

	gpiod_direction_output(priv->gpiod_clk, 0);
	gpiod_direction_output(priv->gpiod_data, 0);
	/* Hold reset low for >=1us, release, then wait 5ms for INIT. */
	gpiod_set_value_cansleep(priv->gpiod_enable, 0);
	gpiod_set_value_cansleep(priv->gpiod_config, 0);
	udelay(10);
	gpiod_set_value_cansleep(priv->gpiod_config, 1);
	msleep(5);
	ret = gpiod_get_value_cansleep(priv->gpiod_init);
	if (ret <= 0) {
		ret = ret < 0 ? ret : -EIO;
		goto failed;
	}
	udelay(300);

	for (i = 0; i < fw->size; i++) {
		u8 b = fw->data[i];
		for (bit = 7; bit >= 0; bit--) {
			gpiod_set_value(priv->gpiod_data, (b >> bit) & 1);
			gpiod_set_value(priv->gpiod_clk, 1);
			ndelay(50);
			gpiod_set_value(priv->gpiod_clk, 0);
			ndelay(50);
		}
		if ((i & 0x7FFF) == 0)
			cond_resched();
	}

	ret = gpiod_get_value_cansleep(priv->gpiod_done);
	if (ret <= 0) {
		ret = ret < 0 ? ret : -EIO;
		goto failed;
	}
	msleep(1);
	gpiod_set_value_cansleep(priv->gpiod_enable, 1);
	atomic_set(&priv->fpga_status, FMCS_FPGA_STATUS_READY);
	priv->fpga_error = 0;
	dev_info(priv->dev, "FPGA bitstream programmed successfully! Micro-OLT active.\n");
	release_firmware(fw);
	return 0;
failed:
	priv->fpga_error = ret;
	atomic_set(&priv->fpga_status, FMCS_FPGA_STATUS_FAILED);
	dev_err(priv->dev, "FPGA configuration handshake failed (%d); Micro-OLT disabled\n", ret);
	release_firmware(fw);
	return ret;
}

static void fmcs_fpga_work_fn(struct work_struct *work)
{
	struct fmcs_priv *priv = container_of(work, struct fmcs_priv, fpga_work);
	fmcs_load_fpga_bitstream(priv, "FTTR_TOP.sbit");
}

static ssize_t fpga_status_show(struct device *dev, struct device_attribute *attr, char *buf)
{
	struct fmcs_priv *priv = dev_get_drvdata(dev);
	int st;

	if (!priv)
		return sysfs_emit(buf, "unknown\n");

	st = atomic_read(&priv->fpga_status);
	switch (st) {
	case FMCS_FPGA_STATUS_PROGRAMMING:
		return sysfs_emit(buf, "programming\n");
	case FMCS_FPGA_STATUS_READY:
		return sysfs_emit(buf, "ready\n");
	case FMCS_FPGA_STATUS_FAILED:
		return sysfs_emit(buf, "failed\n");
	case FMCS_FPGA_STATUS_NOT_STARTED:
	default:
		return sysfs_emit(buf, "not_started\n");
	}
}
static DEVICE_ATTR_RO(fpga_status);

static ssize_t reload_fpga_store(struct device *dev, struct device_attribute *attr,
				  const char *buf, size_t count)
{
	struct fmcs_priv *priv = dev_get_drvdata(dev);

	if (priv) {
		if (atomic_read(&priv->fpga_status) == FMCS_FPGA_STATUS_PROGRAMMING) {
			dev_warn(dev, "FPGA bitstream programming already running\n");
			return -EBUSY;
		}
		schedule_work(&priv->fpga_work);
	}

	return count;
}
static DEVICE_ATTR_WO(reload_fpga);

static int fmcs_match_spi_child(struct device *dev, const void *data)
{
	return dev->class && !strcmp(dev->class->name, "spi_master");
}

static struct spi_controller *fmcs_get_spi_controller(struct device_node *np)
{
	struct platform_device *pdev;
	struct device *child;

	pdev = of_find_device_by_node(np);
	if (!pdev)
		return NULL;

	child = device_find_child(&pdev->dev, NULL, fmcs_match_spi_child);
	platform_device_put(pdev);
	if (!child)
		return NULL;

	return container_of(child, struct spi_controller, dev);
}

static int fmcs_probe(struct platform_device *pdev)
{
	struct device *dev = &pdev->dev;
	struct fmcs_priv *priv;
	int ret;

	priv = devm_kzalloc(dev, sizeof(*priv), GFP_KERNEL);
	if (!priv)
		return -ENOMEM;

	priv->dev = dev;
	mutex_init(&priv->lock);
	mutex_init(&priv->ploam_rx_lock);
	INIT_DELAYED_WORK(&priv->ploam_rx_work, fmcs_ploam_rx_work);
	atomic_set(&priv->fpga_status, FMCS_FPGA_STATUS_NOT_STARTED);
	INIT_WORK(&priv->fpga_work, fmcs_fpga_work_fn);

	/* Map AN7581 SNFI/SPI and NFI MMIO Registers */
	priv->spi_base = ioremap(AN7581_SPI_BASE_PHYS, AN7581_SPI_SIZE);
	if (!priv->spi_base) {
		dev_err(dev, "Failed to ioremap SPI controller at 0x%08x\n", AN7581_SPI_BASE_PHYS);
		return -ENOMEM;
	}

	priv->nfi_base = ioremap(AN7581_NFI_BASE_PHYS, AN7581_NFI_SIZE);
	if (!priv->nfi_base) {
		dev_err(dev, "Failed to ioremap NFI controller at 0x%08x\n", AN7581_NFI_BASE_PHYS);
		iounmap(priv->spi_base);
		return -ENOMEM;
	}

	dev_info(dev, "Mapped AN7581 MMIO: SPI=%px, NFI=%px\n", priv->spi_base, priv->nfi_base);

	/* Enable SPI CS1 pad/pinmux on AN7581. */
	{
		void __iomem *pad_base = ioremap(0x1FA20218, 4);
		if (pad_base) {
			u32 val = readl(pad_base);
			dev_info(dev, "AN7581 SPI CS1 pad reg (0x1FA20218): before=0x%08x\n", val);
			writel(val | 1, pad_base);
			dev_info(dev, "AN7581 SPI CS1 pad reg (0x1FA20218): after=0x%08x\n", readl(pad_base));
			iounmap(pad_base);
		}
	}

	/* Bind to SPI Controller for hardware bus arbitration locking */
	{
		struct device_node *spi_node = of_parse_phandle(dev->of_node, "spi-controller", 0);
		if (!spi_node)
			spi_node = of_find_compatible_node(NULL, NULL, "airoha,en7581-snand");
		if (spi_node) {
			priv->spi_ctrl = fmcs_get_spi_controller(spi_node);
			of_node_put(spi_node);
			if (priv->spi_ctrl)
				dev_info(dev, "Bound to SPI controller %s for bus arbitration locking\n",
					 dev_name(&priv->spi_ctrl->dev));
			else
				dev_warn(dev, "SPI controller found in DT but child device not found\n");
		}
	}
	if (!priv->spi_ctrl) {
		ret = dev_err_probe(dev, -EPROBE_DEFER,
				    "SPI controller required for shared NAND bus locking\n");
		goto err_maps;
	}

	/* Parse GPIO and Interrupts from Device Tree */
	priv->gpiod_int = devm_gpiod_get_optional(dev, "fpga-spi-int", GPIOD_IN);
	priv->gpiod_clk = devm_gpiod_get_optional(dev, "fpga-clk", GPIOD_OUT_LOW);
	priv->gpiod_data = devm_gpiod_get_optional(dev, "fpga-data", GPIOD_OUT_LOW);
	if (of_machine_is_compatible("h3c,hm2004-du"))
		gpiod_add_lookup_table(&hm2004_fpga_gpios);
	priv->gpiod_config = devm_gpiod_get_optional(dev, "fpga-config", GPIOD_OUT_HIGH);
	priv->gpiod_enable = devm_gpiod_get_optional(dev, "fpga-enable", GPIOD_OUT_LOW);
	priv->gpiod_init = devm_gpiod_get_optional(dev, "fpga-init", GPIOD_IN);
	priv->gpiod_done = devm_gpiod_get_optional(dev, "fpga-done", GPIOD_IN);
	if (of_machine_is_compatible("h3c,hm2004-du"))
		gpiod_remove_lookup_table(&hm2004_fpga_gpios);
	if (IS_ERR(priv->gpiod_config) || IS_ERR(priv->gpiod_enable) ||
	    IS_ERR(priv->gpiod_init) || IS_ERR(priv->gpiod_done)) {
		ret = IS_ERR(priv->gpiod_config) ? PTR_ERR(priv->gpiod_config) :
		      IS_ERR(priv->gpiod_enable) ? PTR_ERR(priv->gpiod_enable) :
		      IS_ERR(priv->gpiod_init) ? PTR_ERR(priv->gpiod_init) : PTR_ERR(priv->gpiod_done);
		goto err_maps;
	}
	if (IS_ERR(priv->gpiod_int) || IS_ERR(priv->gpiod_clk) ||
	    IS_ERR(priv->gpiod_data)) {
		ret = IS_ERR(priv->gpiod_int) ? PTR_ERR(priv->gpiod_int) :
		      IS_ERR(priv->gpiod_clk) ? PTR_ERR(priv->gpiod_clk) :
		      PTR_ERR(priv->gpiod_data);
		goto err_maps;
	}

	if (priv->gpiod_int)
		priv->irq = gpiod_to_irq(priv->gpiod_int);
	else
		priv->irq = platform_get_irq_optional(pdev, 0);

	/* Register Character Device /dev/fmcs_mci */
	ret = alloc_chrdev_region(&priv->devno, 0, 1, FMCS_DEV_NAME);
	if (ret)
		goto err_maps;

	cdev_init(&priv->cdev, &fmcs_fops);
	priv->cdev.owner = THIS_MODULE;
	ret = cdev_add(&priv->cdev, priv->devno, 1);
	if (ret)
		goto err_cdev;

	priv->class = class_create(FMCS_DEV_NAME);
	if (IS_ERR(priv->class)) {
		ret = PTR_ERR(priv->class);
		goto err_class;
	}

	{
		struct device *mci_dev;

		mci_dev = device_create(priv->class, NULL, priv->devno, NULL, FMCS_DEV_NAME);
		if (IS_ERR(mci_dev)) {
			ret = PTR_ERR(mci_dev);
			goto err_device;
		}
	}

	/* Initialize molt_omci and molt_ploam */
	ret = fmcs_netdevs_init(priv);
	if (ret)
		goto err_netdevs;

	/* Publish only fully initialized state; remove IRQ before tearing it down. */
	g_fmcs_priv = priv;
	platform_set_drvdata(pdev, priv);
	if (priv->irq > 0) {
		ret = devm_request_threaded_irq(dev, priv->irq, NULL,
					      fmcs_irq_handler,
					      IRQF_TRIGGER_HIGH | IRQF_ONESHOT | IRQF_SHARED,
					      "fmcs-irq", priv);
		if (ret) {
			dev_warn(dev, "Failed to request IRQ %d: %d\n", priv->irq, ret);
			priv->irq = 0;
		}
	}

	/* Initialize FPGA async loader and sysfs status */
	priv->fpga_error = 0;
	schedule_work(&priv->fpga_work);

	ret = device_create_file(dev, &dev_attr_reload_fpga);
	if (ret)
		dev_warn(dev, "Failed to create reload_fpga sysfs attribute: %d\n", ret);
	ret = device_create_file(dev, &dev_attr_fpga_status);
	if (ret)
		dev_warn(dev, "Failed to create fpga_status sysfs attribute: %d\n", ret);
	ret = device_create_file(dev, &dev_attr_ploam_rx_stats);
	if (ret)
		dev_warn(dev, "Failed to create ploam_rx_stats: %d\n", ret);
	schedule_delayed_work(&priv->ploam_rx_work, msecs_to_jiffies(10));

	dev_info(dev, "H3C Micro-OLT FPGA Management Driver (FMCS) initialized successfully.\n");
	return 0;

err_netdevs:
	device_destroy(priv->class, priv->devno);
err_device:
	class_destroy(priv->class);
err_class:
	cdev_del(&priv->cdev);
err_cdev:
	unregister_chrdev_region(priv->devno, 1);
err_maps:
	if (priv->spi_ctrl)
		spi_controller_put(priv->spi_ctrl);
	iounmap(priv->nfi_base);
	iounmap(priv->spi_base);
	return ret;
}

static void fmcs_remove(struct platform_device *pdev)
{
	struct fmcs_priv *priv = platform_get_drvdata(pdev);

	if (!priv)
		return;

	g_fmcs_priv = NULL;
	device_remove_file(priv->dev, &dev_attr_ploam_rx_stats);
	device_remove_file(priv->dev, &dev_attr_fpga_status);
	device_remove_file(priv->dev, &dev_attr_reload_fpga);
	if (priv->irq > 0)
		devm_free_irq(priv->dev, priv->irq, priv);
	cancel_delayed_work_sync(&priv->ploam_rx_work);
	cancel_work_sync(&priv->fpga_work);
	fmcs_netdevs_exit(priv);
	device_destroy(priv->class, priv->devno);
	class_destroy(priv->class);
	cdev_del(&priv->cdev);
	unregister_chrdev_region(priv->devno, 1);

	if (priv->spi_base)
		iounmap(priv->spi_base);
	if (priv->nfi_base)
		iounmap(priv->nfi_base);
	if (priv->spi_ctrl)
		spi_controller_put(priv->spi_ctrl);

}

static const struct of_device_id fmcs_of_match[] = {
	{ .compatible = "h3c,fmcs" },
	{ /* sentinel */ }
};
MODULE_DEVICE_TABLE(of, fmcs_of_match);

static struct platform_driver fmcs_driver = {
	.probe  = fmcs_probe,
	.remove = fmcs_remove,
	.driver = {
		.name           = FMCS_DRV_NAME,
		.of_match_table = fmcs_of_match,
		/* Open character-device handles cannot survive platform unbind. */
		.suppress_bind_attrs = true,
	},
};

module_platform_driver(fmcs_driver);

MODULE_AUTHOR("LazuliKao");
MODULE_DESCRIPTION("H3C HM2004-DU Micro-OLT FPGA Management Driver");
MODULE_LICENSE("GPL");
MODULE_FIRMWARE("FTTR_TOP.sbit");
