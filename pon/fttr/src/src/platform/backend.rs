//! Platform operation contract and the single implementation registration point.
use super::{
    adapter::{PlatformDescriptor, PlatformId},
    context::{ChannelGuard, ManagementLease, PlatformContext},
    openwrt::wake::Wake,
    resources::Resources,
    runtime::SelectionError,
};
use crate::{
    controller::inventory::WorkerRequest,
    pon_engine::{omci::session::OmciTransport, ploam::PloamFrame},
};
use std::{
    fmt::Debug,
    io,
    path::Path,
    sync::{atomic::AtomicBool, mpsc::Receiver, Arc},
};

/// Shared workers hold this interface, never a concrete platform implementation.
pub(crate) trait PlatformBackend: Debug + Sync {
    fn descriptor(&self) -> PlatformDescriptor;
    fn resources(&self) -> &'static Resources;
    fn all_allocations(&self) -> &'static [super::resources::OnuResources] {
        self.resources().allocations
    }
    fn operations(&self) -> fttr_api::PlatformOperations {
        fttr_api::PlatformOperations::default()
    }
    fn transport(&self, id: u8) -> io::Result<Box<dyn OmciTransport>>;
    fn management_lease(&self, id: u8) -> io::Result<Box<dyn ManagementLease>>;
    fn channel(&self, id: u8, channel: u16) -> io::Result<Box<dyn ChannelGuard>>;
    fn exchange_channel(&self, id: u8, channel: u16) -> io::Result<u16>;
    fn send_ploam(&self, frame: &PloamFrame) -> io::Result<()>;
    fn revoke_business(&self, _id: u8) -> io::Result<()> {
        Err(unsupported("business forwarding revocation"))
    }
    fn run(&self, platform: PlatformContext) -> Result<(), SelectionError>;

    fn diagnostics(&self) -> Option<&dyn PlatformDiagnostics> {
        None
    }
    fn health_diagnostic(&self, _platform: PlatformContext) -> io::Result<()> {
        Err(unsupported("evidence diagnostic"))
    }
    fn watch(
        &self,
        _platform: PlatformContext,
        _id: u8,
        _cancellation: Arc<AtomicBool>,
        _vendor: [u8; 4],
        _requests: &Receiver<WorkerRequest>,
        _wake: Arc<Wake>,
    ) -> io::Result<()> {
        Err(unsupported("evidence monitor"))
    }
}

/// Optional hardware diagnostics. A platform need not have an FPGA or UX3326.
pub(crate) trait PlatformDiagnostics: Sync {
    /// Pure validation, before opening/configuring the endpoint. Limits belong
    /// to the platform, not to the generic CLI or Linux SPI transport.
    fn validate_register_access(
        &self,
        _space: super::diagnostics::RegisterSpace,
        _address: u32,
        _row: u32,
        _words: usize,
    ) -> io::Result<()> {
        Err(unsupported("register access"))
    }
    /// Open one exclusive raw-register session. Keep it alive across RMW and
    /// readback; opening separately for each transfer would lose serialization.
    fn open_registers(
        &self,
        _endpoint: &Path,
    ) -> io::Result<Box<dyn super::diagnostics::RegisterSession>> {
        Err(unsupported("register session"))
    }
    fn read_fpga_reg(&self, _address: u32) -> io::Result<u32> {
        Err(unsupported("FPGA read"))
    }
    fn write_fpga_reg(&self, _address: u32, _value: u32) -> io::Result<()> {
        Err(unsupported("FPGA write"))
    }
    fn read_bosa_reg(&self, _address: u32) -> io::Result<u32> {
        Err(unsupported("BOSA read"))
    }
    fn write_bosa_reg(&self, _address: u32, _value: u32) -> io::Result<()> {
        Err(unsupported("BOSA write"))
    }
    fn load_bitstream(&self, _path: &Path) -> io::Result<()> {
        Err(unsupported("FPGA loading"))
    }
    fn firmware_status(&self) -> String {
        "unsupported".into()
    }
    fn fpga_diag(&self, _args: &[String]) -> io::Result<()> {
        Err(unsupported("FPGA diagnostic"))
    }
    fn set_optical_tx_enable(&self, _enabled: bool) -> io::Result<()> {
        Err(unsupported("optical TX control"))
    }
    fn calibrate_bosa(
        &self,
        _source: &Path,
        _offset: u64,
    ) -> io::Result<super::diagnostics::BosaCalibrationEvidence> {
        Err(unsupported("BOSA calibration"))
    }
    fn get_optical_tx_enable(&self) -> io::Result<bool> {
        Err(unsupported("optical TX readback"))
    }
}

pub(crate) fn resolve(id: PlatformId) -> Result<&'static dyn PlatformBackend, SelectionError> {
    if !super::runtime::is_compiled(id) {
        return Err(SelectionError::PlatformNotCompiled(id));
    }
    match id {
        #[cfg(feature = "platform-h3c-hm2004-du")]
        PlatformId::H3cHm2004Du => Ok(&super::h3c_hm2004_du::Hm2004Du),
        #[cfg(feature = "platform-fiberhome-an4475-g2")]
        PlatformId::FiberhomeAn4475G2 => Ok(&super::fiberhome_an4475_g2::An4475G2),
        _ => Err(SelectionError::BackendNotImplemented(id)),
    }
}

pub(crate) fn unsupported(operation: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        format!("platform {operation} is unavailable"),
    )
}

pub(crate) fn resolve_diagnostics(
    id: PlatformId,
) -> Result<&'static dyn PlatformDiagnostics, SelectionError> {
    if !super::runtime::is_compiled(id) {
        return Err(SelectionError::PlatformNotCompiled(id));
    }
    #[cfg(feature = "platform-fiberhome-an4475-g2")]
    if id == PlatformId::FiberhomeAn4475G2 {
        return Ok(&super::fiberhome_an4475_g2::diagnostics::FslDiagnostics);
    }
    resolve(id)?
        .diagnostics()
        .ok_or(SelectionError::BackendNotImplemented(id))
}

/// Compile-time runtime operations; never opens a device or treats a diagnostic
/// implementation as a fully initialized runtime.
pub(crate) fn compiled_operations() -> impl Iterator<Item = fttr_api::PlatformOperations> {
    [
        PlatformId::H3cHm2004Du,
        PlatformId::FiberhomeAn4475G2,
        PlatformId::TianyiTyfza1,
    ]
    .into_iter()
    .filter_map(|id| resolve(id).ok())
    .map(|backend| backend.operations())
}
