/*
 * H3C HM2004-DU Micro-OLT FPGA Management Control System (FMCS)
 *
 * Header file defining IOCTL interface, SPI wire protocol, and driver state.
 */

#ifndef _H3C_FMCS_H_
#define _H3C_FMCS_H_

#include <linux/types.h>
#include <linux/ioctl.h>
#include <linux/io.h>
#include <linux/netdevice.h>
#include <linux/interrupt.h>
#include <linux/cdev.h>
#include <linux/wait.h>
#include <linux/gpio/consumer.h>
#include <linux/spi/spi.h>

#define FMCS_DRV_NAME "h3c-fmcs"
#define FMCS_DEV_NAME "fmcs_mci"

/* 
 * IOCTL Interface (Magic: 0xA5)
 * Discovered via reverse engineering of FmcsMciIoctl.
 */
#define FMCS_IOC_MAGIC 0xA5

struct fmcs_reg_op {
	__u32 addr;
	__u32 val;
};

struct fmcs_bosa_op {
	__u32 addr;
	__u32 val;
};

struct fmcs_attr_op {
	__u32 cmd;
	__u32 param;
};

struct fmcs_ploam_msg {
	__u8  data[16];
	__u32 len;
};

struct fmcs_carrier_req {
	__u32 port;
	__u32 carrier;
};

#define FMCS_IOC_WRITE_FPGA_REG  _IOW(FMCS_IOC_MAGIC, 0, struct fmcs_reg_op)
#define FMCS_IOC_READ_FPGA_REG   _IOWR(FMCS_IOC_MAGIC, 1, struct fmcs_reg_op)
#define FMCS_IOC_WRITE_BOSA_REG  _IOW(FMCS_IOC_MAGIC, 2, struct fmcs_bosa_op)
#define FMCS_IOC_READ_BOSA_REG   _IOWR(FMCS_IOC_MAGIC, 3, struct fmcs_bosa_op)
#define FMCS_IOC_DEV_ATTR_MODIFY _IOW(FMCS_IOC_MAGIC, 4, struct fmcs_attr_op)
#define FMCS_IOC_RECV_PLOAM      _IOR(FMCS_IOC_MAGIC, 5, struct fmcs_ploam_msg)
#define FMCS_IOC_SEND_PACKET     _IOW(FMCS_IOC_MAGIC, 6, struct fmcs_ploam_msg)
#define FMCS_IOC_SET_CARRIER     _IOW(FMCS_IOC_MAGIC, 7, struct fmcs_carrier_req)
#define FMCS_IOC_GET_FPGA_STATUS _IOR(FMCS_IOC_MAGIC, 8, __u32)
/* Local extension: addr=ONU-ID 0..15, val=0 (default) or 0x100+ID.
 * Exchanges the Ethernet OMCI channel; returns the previous val.
 */
#define FMCS_IOC_OMCI_CHANNEL _IOWR(FMCS_IOC_MAGIC, 9, struct fmcs_reg_op)

enum fmcs_fpga_status {
	FMCS_FPGA_STATUS_NOT_STARTED = 0,
	FMCS_FPGA_STATUS_PROGRAMMING = 1,
	FMCS_FPGA_STATUS_READY       = 2,
	FMCS_FPGA_STATUS_FAILED      = 3,
};

/*
 * AN7581 SNFI / SPI Controller Physical Addresses & Register Map
 */
#define AN7581_SPI_BASE_PHYS      0x1FA10000
#define AN7581_SPI_SIZE           0x140
#define AN7581_NFI_BASE_PHYS      0x1FA11000
#define AN7581_NFI_SIZE           0x160

/* Historical name: this is READ_IDLE_EN, not an interrupt-clear register. */
#define REG_SPI_INT_CLEAR         0x0004
#define REG_SPI_MANUAL_OP_CTRL    0x0014
#define REG_SPI_MACMUX_STATUS     0x0018
#define REG_SPI_CTRL_MANUAL_EN    0x0020
#define REG_SPI_OPFIFO_EMPTY      0x0024
#define REG_SPI_OPFIFO_WDATA      0x0028
#define REG_SPI_OPFIFO_FULL       0x002c
#define REG_SPI_OPFIFO_WR         0x0030
#define REG_SPI_DFIFO_W_FULL      0x0034
#define REG_SPI_DFIFO_WDATA       0x0038
#define REG_SPI_DFIFO_R_EMPTY     0x003c
#define REG_SPI_DFIFO_RD          0x0040
#define REG_SPI_DFIFO_RDATA       0x0044
#define REG_SPI_CTRL_DUMMY        0x0080
#define REG_SPI_TN                0x009c
#define REG_SPI_CS_SEL            0x00E4
#define REG_SPI_NFI2SPI_EN        0x0130

#define SPI_OP_DEASSERT_CS        0x00
#define SPI_OP_ASSERT_CS          0x01
#define SPI_OP_TX_DFIFO           0x08
#define SPI_OP_RX_DFIFO           0x0C

/*
 * SPI Wire Protocol Commands
 */
#define FMCS_SPI_CMD_WRITE_REG    0xA8
#define FMCS_SPI_CMD_READ_REQ     0xA0
#define FMCS_SPI_CMD_READ_RESP    0x50

#define FMCS_MAX_FTTR_PORTS 16
#define FMCS_FPGA_TRANSPORT_VID_BASE 0x100
#define FMCS_PORT_TO_FPGA_VID(port)  ((u16)(FMCS_FPGA_TRANSPORT_VID_BASE + (port) - 1))
#define FMCS_PLOAM_RX_DEPTH 64
#define FMCS_PLOAM_RX_BUDGET 16

/* Driver private structure */
struct fmcs_priv {
	struct device *dev;
	void __iomem *spi_base;
	void __iomem *nfi_base;
	struct spi_controller *spi_ctrl;
	int irq;
	struct gpio_desc *gpiod_int;
	struct gpio_desc *gpiod_clk;
	struct gpio_desc *gpiod_data;
	struct gpio_desc *gpiod_config;
	struct gpio_desc *gpiod_enable;
	struct gpio_desc *gpiod_init;
	struct gpio_desc *gpiod_done;

	/* Char device */
	dev_t devno;
	struct cdev cdev;
	struct class *class;

	/* Virtual Net Devices */
	struct net_device *omci_dev;
	u16 omci_channel[16];
	struct net_device *ploam_dev;
	struct net_device *fttr_devs[FMCS_MAX_FTTR_PORTS];
	/* Published after all lower/upper links exist; protected by RTNL/RCU. */
	struct net_device __rcu *fttr_lower;
	struct notifier_block fttr_netdev_nb;
	struct fmcs_bridge_offload *bridge_offload;

	/* Synchronization */
	struct mutex lock;
	/* Only the collector consumes hardware; ioctl consumes this FIFO. */
	struct mutex ploam_rx_lock;
	struct delayed_work ploam_rx_work;
	struct fmcs_ploam_msg ploam_rx_queue[FMCS_PLOAM_RX_DEPTH];
	u32 ploam_rx_head, ploam_rx_count;
	u64 rx_received, rx_enqueued, rx_dequeued, rx_dropped, rx_flushed;
	u64 rx_empty, rx_errors, rx_copy_faults, rx_budget_hits, rx_irqs;
	u32 rx_last_events;

	/* Asynchronous FPGA Bitstream Loader & Status */
	struct work_struct fpga_work;
	atomic_t fpga_status;
	int fpga_error;

	/* Deferred PLOAM transmission */
	struct sk_buff_head ploam_tx_queue;
	struct work_struct ploam_tx_work;
};

/* Function Prototypes */
int fmcs_spi_write_fpga_reg(struct fmcs_priv *priv, u32 addr, u32 val);
int fmcs_spi_read_fpga_reg(struct fmcs_priv *priv, u32 addr, u32 *val);
int fmcs_spi_write_bosa_reg(struct fmcs_priv *priv, u32 addr, u32 val);
int fmcs_spi_read_bosa_reg(struct fmcs_priv *priv, u32 addr, u32 *val);
int fmcs_spi_send_ploam(struct fmcs_priv *priv, const u8 *data, size_t len);
int fmcs_spi_recv_ploam(struct fmcs_priv *priv, u8 *data, size_t *len);

int fmcs_netdevs_init(struct fmcs_priv *priv);
void fmcs_netdevs_exit(struct fmcs_priv *priv);
int fmcs_fttr_set_carrier(struct fmcs_priv *priv, unsigned int port, bool carrier);
int fmcs_bridge_init(struct fmcs_priv *priv);
void fmcs_bridge_exit(struct fmcs_priv *priv);
void fmcs_bridge_unbind(struct fmcs_priv *priv, struct net_device *lower);
int fmcs_bridge_setup_tc(struct fmcs_priv *priv, enum tc_setup_type type, void *data);
bool fmcs_bridge_port_enabled(struct fmcs_priv *priv, unsigned int port);
bool fmcs_bridge_pair_enabled(struct fmcs_priv *priv, unsigned int src, unsigned int dst);
void fmcs_rx_omci_frame(struct fmcs_priv *priv, const u8 *data, size_t len);
void fmcs_rx_ploam_frame(struct fmcs_priv *priv, const u8 *data, size_t len);

#endif /* _H3C_FMCS_H_ */
