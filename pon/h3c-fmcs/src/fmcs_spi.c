/*
 * H3C HM2004-DU Micro-OLT FPGA SPI Transport Implementation
 * Direct MMIO control for Airoha AN7581 SNFI/SPI controller (CS1)
 */

#include <linux/kernel.h>
#include <linux/module.h>
#include <linux/delay.h>
#include <linux/byteorder/generic.h>
#include <linux/io.h>
#include <linux/iopoll.h>
#include <linux/ratelimit.h>
#include "fmcs.h"

/* Use one dummy clock by default; expose this for wire-alignment diagnostics. */
static unsigned int spi_dummy = 1;
module_param(spi_dummy, uint, 0600);
MODULE_PARM_DESC(spi_dummy, "CS1 manual-mode dummy clocks (0..15)");

/* TN=10 samples complete response bytes; no software bit shift is needed. */
static unsigned int spi_rx_skip;
module_param(spi_rx_skip, uint, 0600);
MODULE_PARM_DESC(spi_rx_skip, "Leading CS1 response bits to clock past (0..7); captures an extra byte");

/* Write the low three sampling bits while retaining the enable bit. */
static unsigned int spi_tn = 10;
module_param(spi_tn, uint, 0600);
MODULE_PARM_DESC(spi_tn, "CS1 sampling TN (10 verified with CRC); restored for NAND");

static bool ploam_trace;
module_param(ploam_trace, bool, 0600);
MODULE_PARM_DESC(ploam_trace, "Log a raw aligned PLOAM RX sample at most once per 30 seconds");
static DEFINE_RATELIMIT_STATE(ploam_trace_limit, 30 * HZ, 1);

struct fmcs_spi_state {
	u32 idle, toggle, manual, dummy, cs, nfi, tn;
};

/* Caller owns both controller locks, as spi_mem_access_start() does. */
static void fmcs_spi_lock(struct fmcs_priv *priv)
{
	mutex_lock(&priv->lock);
	spi_bus_lock(priv->spi_ctrl);
	mutex_lock(&priv->spi_ctrl->io_mutex);
}

static void fmcs_spi_unlock(struct fmcs_priv *priv)
{
	mutex_unlock(&priv->spi_ctrl->io_mutex);
	spi_bus_unlock(priv->spi_ctrl);
	mutex_unlock(&priv->lock);
}

static void fmcs_spi_restore(struct fmcs_priv *priv,
			     const struct fmcs_spi_state *state)
{
	/* NAND exec_op does not select CS0: restore selection before unlocking. */
	writel(state->cs, priv->spi_base + REG_SPI_CS_SEL);
	writel(state->tn, priv->spi_base + REG_SPI_TN);
	writel(state->dummy, priv->spi_base + REG_SPI_CTRL_DUMMY);
	writel(state->toggle, priv->spi_base + REG_SPI_MANUAL_OP_CTRL);
	writel(state->manual, priv->spi_base + REG_SPI_CTRL_MANUAL_EN);
	writel(state->nfi, priv->spi_base + REG_SPI_NFI2SPI_EN);
	writel(state->idle, priv->spi_base + REG_SPI_INT_CLEAR);
	readl(priv->spi_base + REG_SPI_CS_SEL); /* Flush posted writes. */
}

static int fmcs_spi_prepare(struct fmcs_priv *priv,
			    struct fmcs_spi_state *state)
{
	u32 val;
	int ret;

	state->idle = readl(priv->spi_base + REG_SPI_INT_CLEAR);
	state->toggle = readl(priv->spi_base + REG_SPI_MANUAL_OP_CTRL);
	state->manual = readl(priv->spi_base + REG_SPI_CTRL_MANUAL_EN);
	state->dummy = readl(priv->spi_base + REG_SPI_CTRL_DUMMY);
	state->cs = readl(priv->spi_base + REG_SPI_CS_SEL);
	state->nfi = readl(priv->spi_base + REG_SPI_NFI2SPI_EN);
	state->tn = readl(priv->spi_base + REG_SPI_TN);

	/* Match the controller driver's manual-mode entry sequence. */
	writel(0, priv->spi_base + REG_SPI_NFI2SPI_EN);
	writel(0, priv->spi_base + REG_SPI_INT_CLEAR);
	ret = readl_poll_timeout(priv->spi_base + REG_SPI_MACMUX_STATUS,
				 val, !val, 1, 250000);
	if (ret)
		return ret;
	writel(9, priv->spi_base + REG_SPI_MANUAL_OP_CTRL);
	writel(1, priv->spi_base + REG_SPI_CTRL_MANUAL_EN);
	writel(READ_ONCE(spi_dummy) & 0xf, priv->spi_base + REG_SPI_CTRL_DUMMY);
	writel(1, priv->spi_base + REG_SPI_CS_SEL);
	writel(READ_ONCE(spi_tn) & 0xf, priv->spi_base + REG_SPI_TN);
	return 0;
}

static int an7581_spi_op_write(struct fmcs_priv *priv, u32 cmd, u32 len)
{
	int timeout = 10000;
	u32 val = ((cmd & 0x1f) << 9) | (len & 0x1ff);

	writel(val, priv->spi_base + REG_SPI_OPFIFO_WDATA);

	while (readl(priv->spi_base + REG_SPI_OPFIFO_FULL) && --timeout)
		cpu_relax();
	if (!timeout)
		return -ETIMEDOUT;

	writel(1, priv->spi_base + REG_SPI_OPFIFO_WR);

	timeout = 10000;
	while (!readl(priv->spi_base + REG_SPI_OPFIFO_EMPTY) && --timeout)
		cpu_relax();
	if (!timeout)
		return -ETIMEDOUT;

	return 0;
}

static int an7581_spi_write_bytes(struct fmcs_priv *priv, const u8 *buf, u32 len)
{
	u32 rem = len;
	const u8 *ptr = buf;

	while (rem > 0) {
		u32 chunk = min_t(u32, rem, 511);
		int ret = an7581_spi_op_write(priv, SPI_OP_TX_DFIFO, chunk);
		if (ret)
			return ret;

		for (u32 i = 0; i < chunk; i++) {
			int timeout = 10000;
			while (readl(priv->spi_base + REG_SPI_DFIFO_W_FULL) && --timeout)
				cpu_relax();
			if (!timeout)
				return -ETIMEDOUT;
			writel(*ptr++, priv->spi_base + REG_SPI_DFIFO_WDATA);
		}
		rem -= chunk;
	}
	return 0;
}

static int an7581_spi_read_bytes(struct fmcs_priv *priv, u8 *buf, u32 len)
{
	u32 rem = len;
	u8 *ptr = buf;

	while (rem > 0) {
		u32 chunk = min_t(u32, rem, 511);
		int ret = an7581_spi_op_write(priv, SPI_OP_RX_DFIFO, chunk);
		if (ret)
			return ret;

		for (u32 i = 0; i < chunk; i++) {
			int timeout = 10000;
			while (readl(priv->spi_base + REG_SPI_DFIFO_R_EMPTY) && --timeout)
				cpu_relax();
			if (!timeout)
				return -ETIMEDOUT;
			*ptr++ = (u8)readl(priv->spi_base + REG_SPI_DFIFO_RDATA);
			writel(1, priv->spi_base + REG_SPI_DFIFO_RD);
		}
		rem -= chunk;
	}
	return 0;
}

static int fmcs_raw_spi_write_locked(struct fmcs_priv *priv, const u8 *buf, u32 len)
{
	struct fmcs_spi_state state;
	int ret, cs_ret;

	if (!priv->spi_base || !priv->spi_ctrl)
		return -ENODEV;
	if (atomic_read(&priv->fpga_status) != FMCS_FPGA_STATUS_READY)
		return -EBUSY;

	ret = fmcs_spi_prepare(priv, &state);
	if (ret)
		goto out_restore;

	/* Assert CS (Active Low) */
	ret = an7581_spi_op_write(priv, SPI_OP_ASSERT_CS, 1);
	if (!ret)
		ret = an7581_spi_write_bytes(priv, buf, len);

	/* Deassert CS */
	cs_ret = an7581_spi_op_write(priv, SPI_OP_DEASSERT_CS, 1);
	if (!ret)
		ret = cs_ret;

out_restore:
	fmcs_spi_restore(priv, &state);
	return ret;
}

static int fmcs_raw_spi_read_locked(struct fmcs_priv *priv, const u8 *send_buf, u32 send_len,
				    u8 *recv_buf, u32 recv_len)
{
	struct fmcs_spi_state state;
	u8 wire[17];
	u32 skip = READ_ONCE(spi_rx_skip);
	int ret, cs_ret;

	if (!priv->spi_base || !priv->spi_ctrl)
		return -ENODEV;
	if (atomic_read(&priv->fpga_status) != FMCS_FPGA_STATUS_READY)
		return -EBUSY;
	if (skip > 7 || recv_len > sizeof(wire) - 1)
		return -EINVAL;

	ret = fmcs_spi_prepare(priv, &state);
	if (ret)
		goto out_restore;

	/* Assert CS (Active Low) */
	ret = an7581_spi_op_write(priv, SPI_OP_ASSERT_CS, 1);
	if (!ret && send_buf && send_len > 0)
		ret = an7581_spi_write_bytes(priv, send_buf, send_len);

	if (!ret && recv_buf && recv_len > 0) {
		/* Preserve every payload bit, including the LSB, on the wire. */
		ret = an7581_spi_read_bytes(priv, wire, recv_len + !!skip);
		if (!ret) {
			for (u32 i = 0; i < recv_len; ++i)
				recv_buf[i] = skip ? (wire[i] << skip) |
					(wire[i + 1] >> (8 - skip)) : wire[i];
		}
	}

	/* Deassert CS */
	cs_ret = an7581_spi_op_write(priv, SPI_OP_DEASSERT_CS, 1);
	if (!ret)
		ret = cs_ret;

out_restore:
	fmcs_spi_restore(priv, &state);
	return ret;
}

int fmcs_spi_write_fpga_reg(struct fmcs_priv *priv, u32 addr, u32 val)
{
	u8 tx_buf[9];
	int ret;

	if (!priv || !priv->spi_base)
		return -EINVAL;

	tx_buf[0] = FMCS_SPI_CMD_WRITE_REG;
	*(__be32 *)&tx_buf[1] = cpu_to_be32(addr);
	*(__be32 *)&tx_buf[5] = cpu_to_be32(val);

	fmcs_spi_lock(priv);

	ret = fmcs_raw_spi_write_locked(priv, tx_buf, sizeof(tx_buf));

	fmcs_spi_unlock(priv);

	pr_debug("fmcs: SPI write fpga reg 0x%08x = 0x%08x, ret %d\n", addr, val, ret);
	return ret;
}

int fmcs_spi_read_fpga_reg(struct fmcs_priv *priv, u32 addr, u32 *val)
{
	u8 tx_req[5];
	u8 rx_cmd = FMCS_SPI_CMD_READ_RESP;
	u8 rx_resp[4] = { 0 };
	int ret;

	if (!priv || !priv->spi_base || !val)
		return -EINVAL;

	/* Phase 1: Send Read Request (0xA0 + addr) */
	tx_req[0] = FMCS_SPI_CMD_READ_REQ;
	*(__be32 *)&tx_req[1] = cpu_to_be32(addr);

	fmcs_spi_lock(priv);

	ret = fmcs_raw_spi_write_locked(priv, tx_req, sizeof(tx_req));
	if (ret)
		goto out_unlock;

	/* FPGA internal processing delay: 50us. */
	udelay(50);

	/* Phase 2: Read 4 bytes of register data with command 0x50 */
	ret = fmcs_raw_spi_read_locked(priv, &rx_cmd, 1, rx_resp, sizeof(rx_resp));
	if (ret)
		goto out_unlock;

	*val = be32_to_cpup((__be32 *)rx_resp);

out_unlock:
	fmcs_spi_unlock(priv);
	if (!ret)
		pr_debug("fmcs: SPI read fpga reg 0x%08x = 0x%08x\n", addr, *val);
	return ret;
}

int fmcs_spi_write_bosa_reg(struct fmcs_priv *priv, u32 addr, u32 val)
{
	if (addr < 0x10000)
		addr = 0x06006300 | addr;
	return fmcs_spi_write_fpga_reg(priv, addr, val);
}

int fmcs_spi_read_bosa_reg(struct fmcs_priv *priv, u32 addr, u32 *val)
{
	u8 tx_req[5];
	u8 rx_cmd = FMCS_SPI_CMD_READ_RESP;
	u8 rx_resp[4] = { 0 };
	int ret;

	if (!priv || !priv->spi_base || !val)
		return -EINVAL;

	if (addr < 0x10000)
		addr = 0x06006300 | addr;

	tx_req[0] = FMCS_SPI_CMD_READ_REQ;
	*(__be32 *)&tx_req[1] = cpu_to_be32(addr);

	fmcs_spi_lock(priv);

	ret = fmcs_raw_spi_write_locked(priv, tx_req, sizeof(tx_req));
	if (ret)
		goto out_unlock;

	/* BOSA controller internal processing delay: 600us. */
	udelay(600);

	ret = fmcs_raw_spi_read_locked(priv, &rx_cmd, 1, rx_resp, sizeof(rx_resp));
	if (ret)
		goto out_unlock;

	/* BOSA controller returns 8-bit value in the last byte rx_resp[3] */
	*val = (u32)rx_resp[3];

out_unlock:
	fmcs_spi_unlock(priv);
	return ret;
}

int fmcs_spi_send_ploam(struct fmcs_priv *priv, const u8 *data, size_t len)
{
	u8 buf[18] = { 0 };
	int ret;

	if (!priv || !priv->spi_base || !data || len != 13)
		return -EINVAL;

	/*
	 * PLOAM SPI transmit wire layout:
	 * buf[0]    = 0xAA (SPI Send PLOAM command)
	 * buf[1..4] = 0x00, 0x00, 0x00, 0x00 (4 bytes dummy/padding header)
	 * buf[5..17]= 13 bytes PLOAM frame (ONU-ID, Msg-ID, 10 bytes payload, CRC)
	 * Total SPI write length = 18 bytes (0x12)
	 */
	buf[0] = 0xAA;
	memcpy(&buf[5], data, len);

	fmcs_spi_lock(priv);

	ret = fmcs_raw_spi_write_locked(priv, buf, 18);

	fmcs_spi_unlock(priv);
	pr_debug("fmcs: SPI send PLOAM frame len 18, ret %d\n", ret);
	return ret;
}

int fmcs_spi_recv_ploam(struct fmcs_priv *priv, u8 *data, size_t *len)
{
	/*
	 * PLOAM SPI receive request layout:
	 * Byte 0:    0xA2 (SPI read request command)
	 * Byte 1..4: 0x00, 0x00, 0x00, 0x10 (little-endian 0x10000000)
	 */
	u8 req[5] = { 0xA2, 0x00, 0x00, 0x00, 0x10 };
	u8 cmd = 0x52;
	u8 rx[13] = { 0 };
	int ret;

	if (!priv || !priv->spi_base || !data)
		return -EINVAL;

	fmcs_spi_lock(priv);

	ret = fmcs_raw_spi_write_locked(priv, req, sizeof(req));
	if (ret)
		goto out_unlock;

	udelay(50);

	ret = fmcs_raw_spi_read_locked(priv, &cmd, 1, rx, sizeof(rx));
	if (ret)
		goto out_unlock;

	if (rx[0] != 0 || rx[1] != 0) {
		memcpy(data, rx, sizeof(rx));
		if (len)
			*len = sizeof(rx);
		pr_debug("fmcs: SPI recv PLOAM frame: msg_id 0x%02x, onu_id 0x%02x\n", rx[1], rx[0]);
	} else {
		if (len)
			*len = 0;
	}

out_unlock:
	fmcs_spi_unlock(priv);
	if (READ_ONCE(ploam_trace) && __ratelimit(&ploam_trace_limit))
		pr_info("fmcs: PLOAM RX sample ret=%d raw=%*ph (onu=%02x msg=%02x)\n",
			ret, (int)sizeof(rx), rx, rx[0], rx[1]);
	return ret;
}
