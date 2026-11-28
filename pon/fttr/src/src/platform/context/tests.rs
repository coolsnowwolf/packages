use super::*;
use crate::platform::{
    adapter::{PlatformAdapter, PlatformDescriptor, PlatformImplementation},
    backend::{unsupported, PlatformDiagnostics},
    diagnostics::HardwareDiagnostics,
    resources::OnuResources,
};
use std::sync::{Arc, Mutex};

type Events = Arc<Mutex<Vec<String>>>;

#[derive(Debug)]
pub(super) struct TestBackend {
    descriptor: PlatformDescriptor,
    resources: fn() -> &'static Resources,
    events: Option<Events>,
}

pub(super) static TEST_BACKEND: TestBackend = TestBackend {
    descriptor: crate::platform::h3c_hm2004_du::Hm2004Du::DESCRIPTOR,
    resources: || crate::pon_engine::test_engine().resources,
    events: None,
};

impl TestBackend {
    fn record(&self, event: String) -> io::Result<Events> {
        let events = self
            .events
            .as_ref()
            .ok_or_else(|| unsupported("test hardware"))?;
        events.lock().unwrap().push(event);
        Ok(events.clone())
    }
}

impl PlatformBackend for TestBackend {
    fn descriptor(&self) -> PlatformDescriptor {
        self.descriptor
    }
    fn resources(&self) -> &'static Resources {
        (self.resources)()
    }
    fn transport(&self, id: u8) -> io::Result<Box<dyn OmciTransport>> {
        Ok(Box::new(MemoryTransport {
            events: self.record(format!("transport:{id}"))?,
            pending: None,
        }))
    }
    fn management_lease(&self, id: u8) -> io::Result<Box<dyn ManagementLease>> {
        Ok(Box::new(Lease {
            events: self.record(format!("lease:{id}"))?,
            committed: false,
        }))
    }
    fn channel(&self, id: u8, channel: u16) -> io::Result<Box<dyn ChannelGuard>> {
        Ok(Box::new(Guard(
            self.record(format!("channel:{id}:{channel}"))?,
        )))
    }
    fn exchange_channel(&self, id: u8, channel: u16) -> io::Result<u16> {
        self.record(format!("exchange:{id}:{channel}"))?;
        Ok(901)
    }
    fn send_ploam(&self, frame: &PloamFrame) -> io::Result<()> {
        self.record(format!("ploam:{}", frame.onu_id))?;
        Ok(())
    }
    fn run(&self, platform: PlatformContext) -> Result<(), SelectionError> {
        assert_eq!(platform.id(), self.descriptor.id);
        self.record("run".into()).unwrap();
        Ok(())
    }
    fn diagnostics(&self) -> Option<&dyn PlatformDiagnostics> {
        self.events
            .as_ref()
            .map(|_| self as &dyn PlatformDiagnostics)
    }
    fn health_diagnostic(&self, _platform: PlatformContext) -> io::Result<()> {
        self.record("health".into())?;
        Ok(())
    }
    fn watch(
        &self,
        _platform: PlatformContext,
        id: u8,
        _cancellation: Arc<std::sync::atomic::AtomicBool>,
        _vendor: [u8; 4],
        _requests: &std::sync::mpsc::Receiver<crate::controller::inventory::WorkerRequest>,
        _wake: Arc<crate::platform::openwrt::wake::Wake>,
    ) -> io::Result<()> {
        self.record(format!("watch:{id}"))?;
        Ok(())
    }
}

impl PlatformDiagnostics for TestBackend {
    fn read_fpga_reg(&self, address: u32) -> io::Result<u32> {
        self.record(format!("read:{address}"))?;
        Ok(0x1234)
    }
}

struct MemoryTransport {
    events: Events,
    pending: Option<BaselineFrame>,
}
impl OmciTransport for MemoryTransport {
    fn set_deadline(&mut self, deadline: Option<std::time::Instant>) {
        self.events
            .lock()
            .unwrap()
            .push(format!("deadline:{deadline:?}"));
    }
    fn send(&mut self, frame: &BaselineFrame) -> io::Result<()> {
        self.events.lock().unwrap().push("send".into());
        self.pending = Some(frame.clone());
        Ok(())
    }
    fn receive(&mut self) -> io::Result<Option<BaselineFrame>> {
        Ok(self.pending.take())
    }
}
struct Lease {
    events: Events,
    committed: bool,
}
impl ManagementLease for Lease {
    fn set_business_grant_floor(&mut self, bytes: u16) -> io::Result<()> {
        self.events
            .lock()
            .unwrap()
            .push(format!("business:{bytes}"));
        Ok(())
    }
    fn set_management_grant_floor(&mut self, bytes: u16) -> io::Result<()> {
        self.events
            .lock()
            .unwrap()
            .push(format!("management:{bytes}"));
        Ok(())
    }
    fn disarm(&mut self) -> io::Result<()> {
        self.committed = true;
        Ok(())
    }
}
impl Drop for Lease {
    fn drop(&mut self) {
        self.events.lock().unwrap().push(
            if self.committed {
                "committed"
            } else {
                "rollback"
            }
            .into(),
        );
    }
}
struct Guard(Events);
impl ChannelGuard for Guard {}
impl Drop for Guard {
    fn drop(&mut self) {
        self.0.lock().unwrap().push("restore".into());
    }
}

fn recording_platform() -> (PlatformContext, Events) {
    static RESOURCES: Resources = Resources {
        onu_ids: &[72],
        management_interfaces: &[],
        capture_interface: None,
        downstream_interfaces: &[],
        allocations: &[OnuResources {
            onu_id: 72,
            alloc_id: 900,
            gem_id: 1200,
            interface: None,
        }],
    };
    let events = Arc::new(Mutex::new(Vec::new()));
    let backend = Box::leak(Box::new(TestBackend {
        descriptor: PlatformDescriptor {
            implementation: PlatformImplementation::Runtime,
            ..crate::platform::fiberhome_an4475_g2::An4475G2::DESCRIPTOR
        },
        resources: || &RESOURCES,
        events: Some(events.clone()),
    }));
    (PlatformContext::from_backend(backend).unwrap(), events)
}

#[test]
fn session_deadlines_reach_backend_and_nested_timeout_restores_outer_deadline() {
    use crate::pon_engine::omci::OmciSession;
    use std::time::{Duration, Instant};

    let (platform, events) = recording_platform();
    let mut session = OmciSession::new(platform.transport(72).unwrap());
    let outer = Instant::now() + Duration::from_secs(10);
    session.finish_before(outer);
    session
        .with_timeout(Duration::from_millis(50), |session| {
            // An inner operation must not relax its caller's shorter deadline.
            session.with_timeout(Duration::from_secs(20), |_| {
                Err::<(), _>(io::Error::other("failed"))
            })
        })
        .unwrap_err();
    let events = events.lock().unwrap();
    assert_eq!(events.len(), 6);
    assert_eq!(events[1], format!("deadline:{:?}", Some(outer)));
    assert_ne!(events[1], events[2]);
    assert_eq!(events[2], events[3]);
    assert_eq!(events[2], events[4]);
    assert_eq!(events[1], events[5]);
}

#[test]
fn invalid_backend_resources_fail_selection_before_any_hardware_operation() {
    static INCOMPLETE: Resources = Resources {
        onu_ids: &[72],
        management_interfaces: &[],
        capture_interface: None,
        downstream_interfaces: &[],
        allocations: &[],
    };
    let events = Arc::new(Mutex::new(Vec::new()));
    let backend = Box::leak(Box::new(TestBackend {
        descriptor: crate::platform::fiberhome_an4475_g2::An4475G2::DESCRIPTOR,
        resources: || &INCOMPLETE,
        events: Some(events.clone()),
    }));
    let error = PlatformContext::from_backend(backend).unwrap_err();
    assert_eq!(
        error,
        SelectionError::InvalidResources(
            PlatformId::FiberhomeAn4475G2,
            "ONU-ID 72 has no platform resource assignment".into(),
        )
    );
    assert!(error.to_string().contains("fiberhome-an4475-g2"));
    assert!(events.lock().unwrap().is_empty());
}

#[cfg(feature = "platform-h3c-hm2004-du")]
#[test]
fn registered_h3c_resources_pass_selection_without_opening_hardware() {
    let platform = PlatformContext::resolve(PlatformId::H3cHm2004Du).unwrap();
    assert_eq!(
        platform.resources().onu_ids.len(),
        crate::platform::h3c_hm2004_du::ONU_COUNT
    );
    let info = platform.info();
    assert_eq!(info.selection, fttr_api::PlatformSelection::Selected);
    assert_eq!(info.platform_id, "h3c-hm2004-du");
    assert_eq!(info.onus.len(), crate::platform::h3c_hm2004_du::ONU_COUNT);
    assert_eq!(info.onus[0].onu_id, 0);
    assert_eq!(info.onus[0].interface.as_deref(), Some("fttr1"));
    assert_eq!(info.onus[15].onu_id, 15);
    assert_eq!(info.onus[15].interface.as_deref(), Some("fttr16"));
    assert!(info.operations.optical_control);
    assert!(info.operations.transceiver_telemetry);
    assert!(info.operations.fpga_diagnostics);
    assert!(info.operations.bosa_diagnostics);
    assert_eq!(
        info.operations.calibration_profile,
        Some(fttr_api::PlatformCalibrationProfile::Ux3326)
    );
    assert!(info.operations.omci_mib);
    assert!(info.operations.downstream_fec);
    assert_eq!(info.operations.npu_offload, cfg!(feature = "npu"));
}

#[test]
fn non_h3c_backend_drives_transport_resources_leases_and_runtime_without_enum_dispatch() {
    let (platform, events) = recording_platform();
    assert_eq!(platform.id(), PlatformId::FiberhomeAn4475G2);
    assert_eq!(platform.resources().onu(72).unwrap().gem_id, 1200);
    let info = platform.info();
    assert_eq!(info.onus.len(), 1);
    assert_eq!(info.onus[0].onu_id, 72);
    assert_eq!(info.onus[0].interface, None);
    assert_eq!(info.operations, fttr_api::PlatformOperations::default());
    // Unsupported IDs never reach a hardware operation, even through another entry.
    assert!(platform.transport(0).is_err());
    assert!(platform.management_lease(0).is_err());
    assert!(platform.channel(0, 1200).is_err());
    assert!(platform.exchange_channel(0, 1200).is_err());
    assert!(events.lock().unwrap().is_empty());

    let mut transport = platform.transport(72).unwrap();
    let mut payload = [0; 32];
    payload[..2].copy_from_slice(&0x8000u16.to_be_bytes());
    let frame = BaselineFrame::new(1, 0x49, 256, 0, payload);
    transport.send(&frame).unwrap();
    assert_eq!(
        transport.receive().unwrap().unwrap().to_bytes(),
        frame.to_bytes()
    );
    assert!(transport.receive().unwrap().is_none());
    let mut lease = platform.management_lease(72).unwrap();
    lease.set_business_grant_floor(100).unwrap();
    lease.set_management_grant_floor(200).unwrap();
    let channel = platform.channel(72, 1200).unwrap();
    assert_eq!(platform.exchange_channel(72, 1201).unwrap(), 901);
    platform
        .send_ploam(&PloamFrame::assign_alloc_id(72))
        .unwrap();
    drop(channel);
    drop(lease);
    let mut committed = platform.management_lease(72).unwrap();
    committed.disarm().unwrap();
    drop(committed);
    crate::platform::runtime::run_selected(platform).unwrap();
    platform.backend().health_diagnostic(platform).unwrap();
    let (_, requests) = std::sync::mpsc::channel();
    crate::controller::health::watch(
        platform,
        72,
        Arc::new(std::sync::atomic::AtomicBool::new(false)),
        *b"TEST",
        &requests,
        Arc::new(crate::platform::openwrt::wake::Wake::new().unwrap()),
    )
    .unwrap();
    assert_eq!(
        *events.lock().unwrap(),
        [
            "transport:72",
            "send",
            "lease:72",
            "business:100",
            "management:200",
            "channel:72:1200",
            "exchange:72:1201",
            "ploam:72",
            "restore",
            "rollback",
            "lease:72",
            "committed",
            "run",
            "health",
            "watch:72"
        ]
    );
}

#[test]
fn optional_diagnostics_do_not_require_fpga_or_bosa_and_never_fall_back_to_h3c() {
    let absent = HardwareDiagnostics::new(PlatformContext::test_context());
    assert_eq!(absent.firmware_status(), "unsupported");
    assert_eq!(
        absent.read_fpga_reg(8).unwrap_err().kind(),
        io::ErrorKind::Unsupported
    );
    let (platform, events) = recording_platform();
    let partial = HardwareDiagnostics::new(platform);
    assert_eq!(partial.read_fpga_reg(8).unwrap(), 0x1234);
    assert_eq!(
        partial.read_bosa_reg(8).unwrap_err().kind(),
        io::ErrorKind::Unsupported
    );
    assert_eq!(*events.lock().unwrap(), ["read:8"]);
}

#[test]
fn resolver_rejects_stubs_and_uncompiled_platforms() {
    let stub = PlatformId::TianyiTyfza1;
    let error = PlatformContext::resolve(stub).unwrap_err();
    assert_eq!(
        error,
        if crate::platform::runtime::is_compiled(stub) {
            SelectionError::BackendNotImplemented(stub)
        } else {
            SelectionError::PlatformNotCompiled(stub)
        }
    );
    let an4475 = PlatformId::FiberhomeAn4475G2;
    assert_eq!(
        PlatformContext::resolve(an4475).is_ok(),
        crate::platform::runtime::is_compiled(an4475)
    );
}
