use crate::{ipc::SubDevice, pon_engine::PonEngine};

pub(super) fn snapshot(
    engine: &PonEngine,
    services: &crate::controller::service::States,
    sources: (&[SubDevice], fttr_api::SyncStatusReply),
) -> fttr_api::StatusReply {
    crate::platform::status::snapshot(
        engine,
        services,
        sources,
        crate::platform::status::Hardware {
            switch_port_count: 16,
            supports_pinning: true,
            ranging_source: |id| {
                format!(
                    "FMCS directed ranging register 0x{:08x}",
                    0x01010020u32 + 4 * u32::from(id)
                )
            },
            has_pon_mac_fec: true,
            onu_us_fec: std::collections::BTreeMap::new(),
            data_path: super::dataplane::data_path,
            fpga_status: super::firmware::get_fpga_status().to_string(),
            pon_mac: Some(super::pon_mac::snapshot()),
            optical_transceiver: super::fmcs::get_optical_status(),
            npu: Some(crate::npu::status()),
        },
    )
}
