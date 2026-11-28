//! H3C implementation of the shared platform operation boundary.
use super::{diagnostic_cli, firmware, fmcs, health, omci, runtime, Hm2004Du, RESOURCES};
use crate::{
    controller::inventory::WorkerRequest,
    platform::{
        adapter::{PlatformAdapter, PlatformDescriptor},
        backend::{PlatformBackend, PlatformDiagnostics},
        context::{ChannelGuard, ManagementLease, PlatformContext},
        openwrt::wake::Wake,
        resources::Resources,
        runtime::SelectionError,
    },
    pon_engine::{omci::session::OmciTransport, ploam::PloamFrame},
};
use std::{
    io,
    path::Path,
    sync::{atomic::AtomicBool, mpsc::Receiver, Arc},
};

impl PlatformBackend for Hm2004Du {
    fn descriptor(&self) -> PlatformDescriptor {
        Self::DESCRIPTOR
    }
    fn resources(&self) -> &'static Resources {
        &RESOURCES
    }
    fn operations(&self) -> fttr_api::PlatformOperations {
        fttr_api::PlatformOperations {
            optical_control: true,
            transceiver_telemetry: true,
            fpga_diagnostics: true,
            bosa_diagnostics: true,
            calibration_profile: Some(fttr_api::PlatformCalibrationProfile::Ux3326),
            npu_offload: cfg!(feature = "npu"),
            omci_mib: true,
            downstream_fec: true,
        }
    }
    fn transport(&self, id: u8) -> io::Result<Box<dyn OmciTransport>> {
        Ok(Box::new(omci::fmcs::FmcsTransport::open(id)?))
    }
    fn management_lease(&self, id: u8) -> io::Result<Box<dyn ManagementLease>> {
        Ok(Box::new(omci::leases::StockOmccLease::open(id)?))
    }
    fn channel(&self, id: u8, channel: u16) -> io::Result<Box<dyn ChannelGuard>> {
        Ok(Box::new(omci::leases::ChannelLease::open(id, channel)?))
    }
    fn exchange_channel(&self, id: u8, channel: u16) -> io::Result<u16> {
        fmcs::exchange_omci_channel(id, channel)
    }
    fn send_ploam(&self, frame: &PloamFrame) -> io::Result<()> {
        fmcs::send_ploam_msg(&frame.to_bytes())
    }
    fn run(&self, platform: PlatformContext) -> Result<(), SelectionError> {
        runtime::run(platform);
        Ok(())
    }
    fn diagnostics(&self) -> Option<&dyn PlatformDiagnostics> {
        Some(self)
    }
    fn health_diagnostic(&self, platform: PlatformContext) -> io::Result<()> {
        health::diagnostic(platform)
    }
    fn revoke_business(&self, id: u8) -> io::Result<()> {
        fmcs::set_fttr_carrier(u32::from(id), false)
    }
    fn watch(
        &self,
        platform: PlatformContext,
        id: u8,
        cancellation: Arc<AtomicBool>,
        vendor: [u8; 4],
        requests: &Receiver<WorkerRequest>,
        wake: Arc<Wake>,
    ) -> io::Result<()> {
        health::watch(platform, id, cancellation, vendor, requests, wake)
    }
}

impl PlatformDiagnostics for Hm2004Du {
    fn read_fpga_reg(&self, address: u32) -> io::Result<u32> {
        fmcs::read_fpga_reg(address)
    }
    fn write_fpga_reg(&self, address: u32, value: u32) -> io::Result<()> {
        fmcs::write_fpga_reg(address, value)
    }
    fn read_bosa_reg(&self, address: u32) -> io::Result<u32> {
        fmcs::read_bosa_reg(address)
    }
    fn write_bosa_reg(&self, address: u32, value: u32) -> io::Result<()> {
        fmcs::write_bosa_reg(address, value)
    }
    fn load_bitstream(&self, path: &Path) -> io::Result<()> {
        firmware::load_bitstream(path)
    }
    fn firmware_status(&self) -> String {
        firmware::get_fpga_status().to_string()
    }
    fn fpga_diag(&self, args: &[String]) -> io::Result<()> {
        diagnostic_cli::run(args)
    }
    fn set_optical_tx_enable(&self, enabled: bool) -> io::Result<()> {
        fmcs::set_optical_tx_enable(enabled)
    }
    fn get_optical_tx_enable(&self) -> io::Result<bool> {
        fmcs::get_optical_tx_enable()
    }
}
