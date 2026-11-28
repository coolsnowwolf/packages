//! Optional diagnostics delegated to the explicitly selected platform backend.
use super::{
    backend::{unsupported, PlatformDiagnostics},
    context::PlatformContext,
};
use std::{io, path::Path};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RegisterSpace {
    Register,
    Memory,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BosaCalibrationEvidence {
    pub chip_id: u8,
    pub page: u8,
    pub table_bytes: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OltInitEvidence {
    pub onu_id_offset: u8,
    pub serdes_status: u32,
    pub ram_init_status: u32,
    pub nni_eth_tpid: u32,
    pub nni_gem_port_offset: u32,
    pub vlan_type_cfg: u32,
}

/// One platform-owned session, including multiword and memory-row operations.
/// Addresses belong to the selected platform's protocol, never CPU MMIO.
pub trait RegisterSession {
    fn read(
        &mut self,
        space: RegisterSpace,
        address: u32,
        row: u32,
        words: usize,
    ) -> io::Result<Vec<u32>>;
    fn write(
        &mut self,
        space: RegisterSpace,
        address: u32,
        row: u32,
        words: &[u32],
    ) -> io::Result<()>;
    fn pon_mac_readback(&mut self) -> io::Result<super::resources::PonMacReadback> {
        Err(unsupported("MAC readback"))
    }
    fn set_optical_tx_enable(&mut self, _enabled: bool) -> io::Result<()> {
        Err(unsupported("optical TX control"))
    }
    fn set_downstream_fec_enable(&mut self, _enabled: bool) -> io::Result<()> {
        Err(unsupported("downstream FEC encoder control"))
    }
    fn initialize_olt(&mut self) -> io::Result<OltInitEvidence> {
        Err(unsupported("OLT initialization"))
    }
}

#[derive(Clone, Copy)]
pub struct HardwareDiagnostics(Option<&'static dyn PlatformDiagnostics>);
impl HardwareDiagnostics {
    pub fn new(platform: PlatformContext) -> Self {
        Self(platform.backend().diagnostics())
    }
    pub fn detect() -> Result<Self, super::runtime::SelectionError> {
        let id = super::runtime::detect_diagnostics()?;
        Ok(Self(Some(super::backend::resolve_diagnostics(id)?)))
    }
    fn operations(self) -> io::Result<&'static dyn PlatformDiagnostics> {
        self.0.ok_or_else(|| unsupported("raw diagnostics"))
    }
    pub fn open_registers(self, endpoint: &Path) -> io::Result<Box<dyn RegisterSession>> {
        self.operations()?.open_registers(endpoint)
    }
    pub fn validate_register_access(
        self,
        space: RegisterSpace,
        address: u32,
        row: u32,
        words: usize,
    ) -> io::Result<()> {
        self.operations()?
            .validate_register_access(space, address, row, words)
    }
    pub fn read_fpga_reg(self, address: u32) -> io::Result<u32> {
        self.operations()?.read_fpga_reg(address)
    }
    pub fn write_fpga_reg(self, address: u32, value: u32) -> io::Result<()> {
        self.operations()?.write_fpga_reg(address, value)
    }
    pub fn read_bosa_reg(self, address: u32) -> io::Result<u32> {
        self.operations()?.read_bosa_reg(address)
    }
    pub fn write_bosa_reg(self, address: u32, value: u32) -> io::Result<()> {
        self.operations()?.write_bosa_reg(address, value)
    }
    pub fn load_bitstream(self, path: &Path) -> io::Result<()> {
        self.operations()?.load_bitstream(path)
    }
    pub fn firmware_status(self) -> String {
        self.operations()
            .map(|ops| ops.firmware_status())
            .unwrap_or_else(|_| "unsupported".into())
    }
    pub fn fpga_diag(self, args: &[String]) -> io::Result<()> {
        self.operations()?.fpga_diag(args)
    }
    pub fn set_optical_tx_enable(self, enabled: bool) -> io::Result<()> {
        self.operations()?.set_optical_tx_enable(enabled)
    }
    pub fn get_optical_tx_enable(self) -> io::Result<bool> {
        self.operations()?.get_optical_tx_enable()
    }
    pub fn calibrate_bosa(self, source: &Path, offset: u64) -> io::Result<BosaCalibrationEvidence> {
        self.operations()?.calibrate_bosa(source, offset)
    }
}
