use super::{
    calibration::{self, CalibrationTable},
    initialization::{
        self,
        defaults::{
            apply_gpon_defaults, configure_nni_vport, DEFAULT_NNI_ETH_TPID,
            DEFAULT_NNI_GEMPORT_OFFSET, NNI_ETH_TPID_CFG, NNI_GEMPORT_OFFSET_CFG, VLAN_TYPE_CFG,
        },
        InitConfig, RAM_INIT_STATUS, SERDES_STATUS,
    },
    optical::{i2c_dev::I2cDevice, OpticalBus},
    profile::{initialize_ux3326s, StockPacer, UX3326S_DEVICE_ADDRESS},
    protocol::{self, Bus, Fsl},
    transport::Device,
};
use crate::platform::{
    backend::PlatformDiagnostics,
    diagnostics::{BosaCalibrationEvidence, OltInitEvidence, RegisterSession, RegisterSpace},
    resources::PonMacReadback,
};
use std::{
    collections::BTreeMap,
    fs::File,
    io::{self, Read, Seek, SeekFrom},
    path::Path,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const DOWNSTREAM_OPTICAL_I2C_DEV: &str = "/dev/i2c-0";
const FSL_DEVICE_PATH: &str = "/dev/fsl-1fbd4000.fsl";

pub(crate) struct FslDiagnostics;
impl PlatformDiagnostics for FslDiagnostics {
    fn validate_register_access(
        &self,
        space: RegisterSpace,
        address: u32,
        row: u32,
        words: usize,
    ) -> io::Result<()> {
        protocol::validate(space, address, row, words)
    }
    fn open_registers(&self, endpoint: &Path) -> io::Result<Box<dyn RegisterSession>> {
        Ok(Box::new(Fsl::new(Device::open(endpoint)?)))
    }
    fn read_bosa_reg(&self, address: u32) -> io::Result<u32> {
        let reg = u8::try_from(address).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "UX3326S I2C register address must be within 0..=0xff",
            )
        })?;
        let mut bus = OpticalBus::new(I2cDevice::open(Path::new(DOWNSTREAM_OPTICAL_I2C_DEV))?);
        let mut val = [0u8; 1];
        bus.read(UX3326S_DEVICE_ADDRESS, reg, &mut val)?;
        Ok(u32::from(val[0]))
    }
    fn write_bosa_reg(&self, address: u32, value: u32) -> io::Result<()> {
        let reg = u8::try_from(address).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "UX3326S I2C register address must be within 0..=0xff",
            )
        })?;
        let byte = u8::try_from(value).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "UX3326S I2C register value must be within 0..=0xff",
            )
        })?;
        let mut bus = OpticalBus::new(I2cDevice::open(Path::new(DOWNSTREAM_OPTICAL_I2C_DEV))?);
        bus.write(UX3326S_DEVICE_ADDRESS, reg, &[byte])
    }
    fn calibrate_bosa(&self, source: &Path, offset: u64) -> io::Result<BosaCalibrationEvidence> {
        // Same precondition as the runtime path: programming happens only with
        // downstream TX off, and a refused TX-off must not start 802 writes.
        // The FSL device is exclusive, so this also fails fast while fttrd is
        // running; TX stays off afterwards (the operator re-enables explicitly).
        let mut fsl = Fsl::new(Device::open(Path::new(FSL_DEVICE_PATH))?);
        fsl.set_tx_enabled(false)?;
        drop(fsl);
        let mut file = File::open(source)?;
        file.seek(SeekFrom::Start(offset))?;
        let mut raw = [0u8; calibration::TABLE_BYTES];
        file.read_exact(&mut raw)?;
        program_table(&raw, || Ok(()))
    }
    fn set_optical_tx_enable(&self, enabled: bool) -> io::Result<()> {
        let mut fsl = Fsl::new(Device::open(Path::new(FSL_DEVICE_PATH))?);
        fsl.set_tx_enabled(enabled)
    }
    fn get_optical_tx_enable(&self) -> io::Result<bool> {
        let mut fsl = Fsl::new(Device::open(Path::new(FSL_DEVICE_PATH))?);
        fsl.is_tx_enabled()
    }
}

/// Read one word per memory row. Counter rows are indexed by ONU slot;
/// no reset or clear-on-read operation is performed here.
fn read_mem_rows<B: Bus>(
    fsl: &mut Fsl<B>,
    addr: u32,
    base_row: u32,
    rows: u32,
) -> io::Result<Vec<u32>> {
    (0..rows)
        .map(|i| Ok(fsl.read(RegisterSpace::Memory, addr, base_row + i, 1)?[0]))
        .collect()
}

/// Read-only AN4475-G2 FSL PON counter and PHY configuration snapshot.
/// Pure register/memory reads: no state is cleared, enabled or disabled.
/// The returned counters are raw hardware observations; they are not ONU FEC
/// PM and do not by themselves prove link readiness or traffic forwarding.
pub(crate) fn read_pon_stats<B: Bus>(
    fsl: &mut Fsl<B>,
    onu_index: u16,
) -> io::Result<fttr_api::PonStatsSnapshot> {
    let started_unix_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    let gtc = fsl.read(RegisterSpace::Register, 0x732, 0, 14)?;
    let ugem = fsl.read(RegisterSpace::Register, 0x740, 0, 12)?;
    let dgem = fsl.read(RegisterSpace::Register, 0x74c, 0, 14)?;
    let rsm_eth = fsl.read(RegisterSpace::Register, 0x82c, 0, 8)?;
    let rsm_drop = fsl.read(RegisterSpace::Register, 0x828, 0, 3)?;
    let nni_us = fsl.read(RegisterSpace::Register, 0x934, 0, 13)?;
    let nni_ds = fsl.read(RegisterSpace::Register, 0x942, 0, 15)?;
    let slot = u32::from(onu_index);
    let det = read_mem_rows(fsl, 0x762, 8 * slot + 1, 4)?;
    let onu_us = read_mem_rows(fsl, 0x818, 8 * slot, 8)?;
    let onu_ds = read_mem_rows(fsl, 0x763, 16 * slot, 9)?;

    let mut phy_cfg = BTreeMap::new();
    for (name, addr) in [
        ("serdes_status", 0x110f),
        ("pon_sds_cfg", 0x101),
        ("gpon_sds_cfg", 0x20a),
        ("dlm_match_bit", 0x20b),
        ("opt_reset_cfg", 0x209),
        ("rsm_alm", 0x810),
        ("nni_ds_alm", 0x930),
        ("nni_us_alm", 0x931),
        ("debug_pin_sel", 0x3),
        ("eth_type_cfg", 0x2),
        ("pon_sds_en", 0x100),
        ("opt_cfg", 0x102),
        ("bmen_mode", 0x103),
        ("bmen_dly", 0x104),
        ("bmen_clr", 0x106),
        ("afifo_dly", 0x108),
        ("burst_win_mod", 0x10a),
        ("opt_light", 0x10e),
        ("bmen_sd_clr", 0x117),
        ("discover_cfg", 0x119),
        ("preamble_cfg", 0x11a),
        ("oversamp_dly", 0x11b),
        ("oversamp_sel", 0x11c),
        ("ext_burst_1g", 0x206),
        ("guard_time", 0x207),
        ("opt_sd_cfg", 0x208),
        ("ext_burst_2p5", 0x21e),
        ("us_fec_corr_en", 0x221),
        ("us_fec_en", 0x22d),
        ("fifo_sta", 0x240),
        ("ploam_profile_mode", 0x410),
        ("ploam_pre_len", 0x441),
        ("pd_pll_sds", 0x1104),
        ("pon_pll", 0x1105),
        ("pon_pll_frac", 0x1106),
        ("hsst_cfg", 0x110b),
    ] {
        phy_cfg.insert(
            name.to_string(),
            serde_json::json!(fsl.read_register(addr)?),
        );
    }
    for (name, addr, words) in [
        ("us_rate_sel", 0x212, 4),
        ("bmp_up_rate", 0x62f, 4),
        ("overhead_1g", 0x203, 3),
        ("overhead_2p5", 0x21b, 3),
        ("ploam_burst0", 0x429, 6),
        ("ploam_burst1", 0x42f, 6),
    ] {
        phy_cfg.insert(
            name.to_string(),
            serde_json::json!(fsl.read(RegisterSpace::Register, addr, 0, words)?),
        );
    }

    Ok(fttr_api::PonStatsSnapshot {
        schema_version: 1,
        onu_index,
        started_unix_ms,
        us_gtc: fttr_api::PonUsGtc {
            det_ok_normal: gtc[0],
            det_ok_ranging: gtc[1] & 0xffff,
            det_fail_normal: gtc[2] & 0xffff,
            det_fail_ranging: gtc[2] >> 16,
            fec_block: gtc[3],
            fec_correct: gtc[4],
            fec_correct_byte: gtc[5],
            fec_uncorrect: gtc[6],
            bip_err: gtc[7],
            burst: gtc[8],
            normal_ploam_crc_err: gtc[10] & 0xffff,
            dbru_crc_err: gtc[10] >> 16,
            normal_opt_rst: gtc[12] >> 16,
            lof_int: gtc[13] & 0xffff,
        },
        us_gem: fttr_api::PonUsGem {
            omci_lf0: ugem[0],
            omci_lf1: ugem[1],
            eth_lf0: ugem[2],
            eth_lf1: ugem[3],
            omci_bytes: ugem[4],
            eth_bytes: ugem[5],
            idle_bytes: ugem[6],
            hec_uncorrect: ugem[7] & 0xffff,
            hec_correct: ugem[7] >> 16,
            pli_err: ugem[8],
        },
        ds_gem: fttr_api::PonDsGem {
            omci_lf0: dgem[0],
            omci_lf1: dgem[1],
            eth_lf0: dgem[2],
            eth_lf1: dgem[3],
            eth_bytes: dgem[5],
            uc_pkt: dgem[7],
            mc_pkt: dgem[8],
            bc_pkt: dgem[9],
        },
        onu_det: fttr_api::PonOnuDet {
            us_bip_err: det[0],
            us_burst: det[1],
            us_det_fail: det[2],
            us_det_drift: det[3],
        },
        onu_us_rsm: fttr_api::PonOnuUsRsm {
            omci_pkt: onu_us[0],
            uc_pkt: onu_us[2],
            eth_bytes: onu_us[5],
            mc_pkt: onu_us[6],
            bc_pkt: onu_us[7],
        },
        onu_ds_pkt: fttr_api::PonOnuDsPkt {
            omci_pkt: onu_ds[0],
            eth_pkt: onu_ds[1],
            uc_pkt: onu_ds[4],
            mc_pkt: onu_ds[5],
            bc_pkt: onu_ds[6],
        },
        rsm_eth_sizes: rsm_eth,
        rsm_drop_raw: rsm_drop,
        nni_us: fttr_api::PonNniUs {
            eth_rx: nni_us[0],
            omci_rx: nni_us[1],
            eth_tx: nni_us[3],
            omci_tx: nni_us[4],
            eth_shm2adp: nni_us[8],
        },
        nni_ds: fttr_api::PonNniDs {
            eth_rx: nni_ds[0],
            omci_rx: nni_ds[1],
            err_rx: nni_ds[2],
            eth_wr: nni_ds[4],
            eth_tx: nni_ds[6],
            switch_tx: nni_ds[8],
            switch_rx: nni_ds[9],
            novlan: nni_ds[10] & 0xffff,
            switch_fail: nni_ds[10] >> 16,
        },
        eth_mac: fttr_api::PonEthMac {
            rx_code_err: fsl.read_register(0xc32)?,
            rx_fcs_ok: fsl.read_register(0xc33)?,
            rx_fcs_err: fsl.read_register(0xc34)?,
            rx_good: fsl.read_register(0xc39)?,
            rx_bad: fsl.read_register(0xc3a)?,
            rx_uc: fsl.read_register(0xc3e)?,
            rx_bc: fsl.read_register(0xc40)?,
            tx_good: fsl.read_register(0xc61)?,
            tx_uc: fsl.read_register(0xc52)?,
            tx_mc: fsl.read_register(0xc53)?,
            tx_bc: fsl.read_register(0xc54)?,
            tx_undersize: fsl.read_register(0xc4c)?,
            tx_oversize: fsl.read_register(0xc4e)?,
        },
        phy_cfg,
    })
}

pub(super) fn program_table(
    raw: &[u8],
    mut checkpoint: impl FnMut() -> io::Result<()>,
) -> io::Result<BosaCalibrationEvidence> {
    checkpoint()?;
    let table = CalibrationTable::parse(&raw)?;
    let mut bus = OpticalBus::new(I2cDevice::open(Path::new(DOWNSTREAM_OPTICAL_I2C_DEV))?);
    struct SleepPacer;
    impl StockPacer for SleepPacer {
        fn after_write(&mut self, _loop_iterations: u32) -> io::Result<()> {
            std::thread::sleep(Duration::from_micros(200));
            Ok(())
        }
    }
    let mut pacer = SleepPacer;
    let chip = initialize_ux3326s(&mut bus, &table, &mut pacer, checkpoint)?;
    Ok(BosaCalibrationEvidence {
        chip_id: chip.discriminator(),
        page: chip.current_page(),
        table_bytes: raw.len(),
    })
}

impl RegisterSession for Fsl<Device> {
    fn read(
        &mut self,
        space: RegisterSpace,
        address: u32,
        row: u32,
        words: usize,
    ) -> io::Result<Vec<u32>> {
        Fsl::read(self, space, address, row, words)
    }
    fn write(
        &mut self,
        space: RegisterSpace,
        address: u32,
        row: u32,
        words: &[u32],
    ) -> io::Result<()> {
        Fsl::write(self, space, address, row, words)
    }
    fn pon_mac_readback(&mut self) -> io::Result<PonMacReadback> {
        Fsl::pon_mac_readback(self)
    }
    fn set_optical_tx_enable(&mut self, enabled: bool) -> io::Result<()> {
        self.set_tx_enabled(enabled)
    }
    fn set_downstream_fec_enable(&mut self, enabled: bool) -> io::Result<()> {
        self.set_fec_enabled(enabled)
    }
    fn initialize_olt(&mut self) -> io::Result<OltInitEvidence> {
        let init = InitConfig::default();
        initialization::release_reset(self, &init, &mut |delay| {
            std::thread::sleep(delay);
            Ok(())
        })?;
        let onu_id_offset = apply_gpon_defaults(self, init.max_onu_id)?;
        configure_nni_vport(self, DEFAULT_NNI_GEMPORT_OFFSET, DEFAULT_NNI_ETH_TPID)?;
        Ok(OltInitEvidence {
            onu_id_offset,
            serdes_status: self.read_register(SERDES_STATUS)?,
            ram_init_status: self.read_register(RAM_INIT_STATUS)?,
            nni_eth_tpid: self.read_register(NNI_ETH_TPID_CFG)? & 0xffff,
            nni_gem_port_offset: self.read_register(NNI_GEMPORT_OFFSET_CFG)? & 0x0fff,
            vlan_type_cfg: self.read_register(VLAN_TYPE_CFG)? & 0xffff,
        })
    }
}
