//! Whole-device compile-time registry. Core activation/inventory never depend on adapters.
use crate::cwmp;
use crate::model::BusinessPhase;

pub fn observe_mqtt_report(source: &str, payload: &[u8], received: u64) -> bool {
    #[cfg(all(feature = "device-hl202", feature = "mqtt"))]
    {
        hl202::telemetry::store::observe(source, payload, received)
    }
    #[cfg(not(all(feature = "device-hl202", feature = "mqtt")))]
    {
        let _ = (source, payload, received);
        false
    }
}
#[cfg(feature = "device-fiberhome")]
pub(crate) mod fiberhome;
pub mod generic;
#[cfg(feature = "device-hl202")]
pub mod hl202;
#[cfg(feature = "device-hwtc")]
pub mod hwtc;
#[cfg(feature = "device-realtek")]
pub(crate) mod realtek;
#[cfg(feature = "device-scty")]
pub(crate) mod scty;
#[cfg(feature = "device-star")]
pub(crate) mod star;
#[cfg(feature = "device-zte")]
pub(crate) mod zte;

/// Optional device identity rules, independent of the shared OMCI bridge implementation.
pub(crate) fn matches_cwmp_identity(optical_sn: &str, product_class: &str, serial: &str) -> bool {
    for family in ["hl202", "hwtc", "star", "zte"] {
        if matches_cwmp_adapter(family, optical_sn, product_class, serial) {
            return true;
        }
    }
    if cwmp::vendor::ct::matches_identity(optical_sn, product_class, serial) {
        return true;
    }
    let _ = (optical_sn, product_class, serial);
    false
}

/// Match a CWMP device record against the expected optical serial and adapter.
pub fn matches_cwmp_adapter(
    adapter: &str,
    optical_sn: &str,
    product_class: &str,
    serial: &str,
) -> bool {
    match adapter {
        #[cfg(feature = "device-hl202")]
        "hl202" => {
            optical_sn.starts_with("H3CT")
                && product_class == "HL202-DU-C"
                && serial.contains(optical_sn)
        }
        #[cfg(feature = "device-hwtc")]
        "hwtc" => {
            if !optical_sn.starts_with("HWTC") || optical_sn.len() < 4 {
                return false;
            }
            let hex = format!(
                "{}{}",
                optical_sn[..4]
                    .bytes()
                    .map(|b| format!("{b:02X}"))
                    .collect::<String>(),
                &optical_sn[4..]
            );
            serial.eq_ignore_ascii_case(&hex)
        }
        #[cfg(feature = "device-star")]
        "star" => star::matches_cwmp(optical_sn, product_class, serial),
        #[cfg(feature = "device-zte")]
        "zte" => zte::matches_cwmp(optical_sn, product_class, serial),
        "generic" => {
            if cwmp::vendor::ct::matches_identity(optical_sn, product_class, serial) {
                return true;
            }
            #[cfg(feature = "device-zte")]
            if zte::matches_cwmp(optical_sn, product_class, serial) {
                return true;
            }
            false
        }
        _ => false,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Adapter {
    #[cfg(feature = "device-hl202")]
    Hl202,
    #[cfg(feature = "device-hwtc")]
    Hwtc(hwtc::Profile),
    #[cfg(feature = "device-star")]
    Star,
    #[cfg(feature = "device-zte")]
    Zte(zte::Profile),
    #[cfg(feature = "device-scty")]
    Scty,
    #[cfg(feature = "device-fiberhome")]
    Fiberhome,
    #[cfg(feature = "device-realtek")]
    Realtek,
    GenericBridge,
    Unavailable,
}

#[derive(Clone, Copy)]
pub enum ManagementWanStage {
    Configuring,
    Verifying,
}
pub fn ensure_mqtt_management_wan(
    device_id: &str,
    vlans: &[u32],
    priority: u32,
    persist_matching: bool,
    progress: impl FnMut(ManagementWanStage) -> bool,
) -> Result<u32, String> {
    #[cfg(all(feature = "device-hl202", feature = "mqtt"))]
    {
        hl202::management_wan::ensure(device_id, vlans, priority, persist_matching, progress)
    }
    #[cfg(not(all(feature = "device-hl202", feature = "mqtt")))]
    {
        let _ = (device_id, vlans, priority, persist_matching, progress);
        Err("MQTT management WAN unavailable: build requires mqtt and device-hl202".into())
    }
}
#[derive(Clone, Default)]
pub struct CwmpCredentials {
    pub username: String,
    pub password: String,
}
pub(crate) fn cwmp_credentials(product_class: &str) -> Option<CwmpCredentials> {
    #[cfg(feature = "device-hl202")]
    if let Some(credentials) = hl202::cwmp_credentials(product_class) {
        return Some(credentials);
    }
    let _ = product_class;
    None
}
pub async fn ensure_mqtt_management_wan_async<F: std::future::Future<Output = bool>>(
    device_id: &str,
    vlans: &[u32],
    priority: u32,
    persist_matching: bool,
    progress: impl FnMut(ManagementWanStage) -> F,
) -> Result<u32, String> {
    #[cfg(all(feature = "device-hl202", feature = "mqtt"))]
    {
        hl202::management_wan::ensure_async(device_id, vlans, priority, persist_matching, progress)
            .await
    }
    #[cfg(not(all(feature = "device-hl202", feature = "mqtt")))]
    {
        let _ = (device_id, vlans, priority, persist_matching, progress);
        Err("MQTT management WAN unavailable: build requires mqtt and device-hl202".into())
    }
}
pub async fn read_authenticated_cwmp_settings_async(
    target: &str,
    url: &str,
    credentials: &CwmpCredentials,
) -> Result<bool, String> {
    #[cfg(all(feature = "device-hl202", feature = "mqtt"))]
    {
        hl202::mqtt_lifecycle::read_authenticated_settings_async(target, url, credentials).await
    }
    #[cfg(not(all(feature = "device-hl202", feature = "mqtt")))]
    {
        let _ = (target, url, credentials);
        Err("MQTT CWMP unavailable: build requires mqtt and device-hl202".into())
    }
}
pub async fn configure_authenticated_cwmp_settings_async(
    target: &str,
    url: &str,
    credentials: &CwmpCredentials,
) -> Result<(), String> {
    #[cfg(all(feature = "device-hl202", feature = "mqtt"))]
    {
        hl202::mqtt_lifecycle::configure_settings_async(target, url, credentials).await
    }
    #[cfg(not(all(feature = "device-hl202", feature = "mqtt")))]
    {
        let _ = (target, url, credentials);
        Err("MQTT CWMP unavailable: build requires mqtt and device-hl202".into())
    }
}
pub fn read_authenticated_cwmp_settings(
    target: &str,
    url: &str,
    credentials: &CwmpCredentials,
) -> Result<bool, String> {
    #[cfg(all(feature = "device-hl202", feature = "mqtt"))]
    {
        hl202::mqtt_lifecycle::read_authenticated_settings(target, url, credentials)
    }
    #[cfg(not(all(feature = "device-hl202", feature = "mqtt")))]
    {
        let _ = (target, url, credentials);
        Err("MQTT CWMP unavailable: build requires mqtt and device-hl202".into())
    }
}
pub fn configure_authenticated_cwmp_settings(
    target: &str,
    url: &str,
    credentials: &CwmpCredentials,
) -> Result<(), String> {
    #[cfg(all(feature = "device-hl202", feature = "mqtt"))]
    {
        hl202::mqtt_lifecycle::configure_settings(target, url, credentials)
    }
    #[cfg(not(all(feature = "device-hl202", feature = "mqtt")))]
    {
        let _ = (target, url, credentials);
        Err("MQTT CWMP unavailable: build requires mqtt and device-hl202".into())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Capabilities {
    pub business: bool,
    pub shell_commands: bool,
}
impl Adapter {
    pub fn capabilities(self) -> Capabilities {
        match self {
            #[cfg(feature = "device-hl202")]
            Self::Hl202 => Capabilities {
                business: true,
                shell_commands: cfg!(feature = "mqtt"),
            },
            #[cfg(feature = "device-hwtc")]
            Self::Hwtc(_) => Capabilities {
                business: true,
                shell_commands: false,
            },
            #[cfg(feature = "device-star")]
            Self::Star => Capabilities {
                business: true,
                shell_commands: false,
            },
            #[cfg(feature = "device-zte")]
            Self::Zte(_) => Capabilities {
                business: true,
                shell_commands: false,
            },
            Self::GenericBridge => Capabilities {
                business: true,
                shell_commands: false,
            },
            #[cfg(feature = "device-scty")]
            Self::Scty => Capabilities {
                business: true,
                shell_commands: false,
            },
            #[cfg(feature = "device-fiberhome")]
            Self::Fiberhome => Capabilities {
                business: true,
                shell_commands: false,
            },
            #[cfg(feature = "device-realtek")]
            Self::Realtek => Capabilities {
                business: true,
                shell_commands: false,
            },
            _ => Capabilities {
                business: false,
                shell_commands: false,
            },
        }
    }
    pub fn status(self) -> BusinessPhase {
        match self {
            #[cfg(feature = "device-hl202")]
            Self::Hl202 => BusinessPhase::Configuring,
            #[cfg(feature = "device-hwtc")]
            Self::Hwtc(_) => BusinessPhase::Configuring,
            #[cfg(feature = "device-star")]
            Self::Star => BusinessPhase::Configuring,
            #[cfg(feature = "device-zte")]
            Self::Zte(_) => BusinessPhase::Configuring,
            Self::GenericBridge => BusinessPhase::Configuring,
            #[cfg(feature = "device-scty")]
            Self::Scty => BusinessPhase::Configuring,
            #[cfg(feature = "device-fiberhome")]
            Self::Fiberhome => BusinessPhase::Configuring,
            #[cfg(feature = "device-realtek")]
            Self::Realtek => BusinessPhase::Configuring,
            Self::Unavailable => BusinessPhase::Unsupported,
        }
    }
    pub fn verification_status(self) -> BusinessPhase {
        match self {
            #[cfg(feature = "device-hwtc")]
            Self::Hwtc(hwtc::Profile::V152) => BusinessPhase::WanBootstrapPending,
            #[cfg(feature = "device-hwtc")]
            Self::Hwtc(hwtc::Profile::K155 | hwtc::Profile::Standard) => BusinessPhase::Verifying,
            #[cfg(feature = "device-hl202")]
            Self::Hl202 => BusinessPhase::Verifying,
            #[cfg(feature = "device-star")]
            Self::Star => BusinessPhase::Verifying,
            #[cfg(feature = "device-zte")]
            Self::Zte(_) => BusinessPhase::Verifying,
            Self::GenericBridge => BusinessPhase::Verifying,
            #[cfg(feature = "device-scty")]
            Self::Scty => BusinessPhase::Verifying,
            #[cfg(feature = "device-fiberhome")]
            Self::Fiberhome => BusinessPhase::Verifying,
            #[cfg(feature = "device-realtek")]
            Self::Realtek => BusinessPhase::Verifying,
            Self::Unavailable => BusinessPhase::Verifying,
        }
    }
    pub fn configure(
        self,
        platform: crate::platform::context::PlatformContext,
        id: u8,
        token: std::sync::Arc<std::sync::atomic::AtomicBool>,
    ) -> std::io::Result<()> {
        crate::omci_auth::authenticate(platform, id, token.clone(), self.auth_layout())?;
        match self {
            #[cfg(feature = "device-hl202")]
            Self::Hl202 => {
                hl202::service::configure(platform, id, Some(0), true, true, true, Some(token))
            }
            #[cfg(feature = "device-hwtc")]
            Self::Hwtc(profile) => match profile {
                hwtc::Profile::K155 => hwtc::k155::service::configure(platform, id, Some(token)),
                hwtc::Profile::V152 => hwtc::v152::service::configure(platform, id, Some(token)),
                hwtc::Profile::Standard => hwtc::service::configure(platform, id, Some(token)),
            },
            #[cfg(feature = "device-star")]
            Self::Star => generic::service::configure(platform, id, Some(token)),
            #[cfg(feature = "device-zte")]
            Self::Zte(profile) => zte::configure(platform, profile, id, Some(token)),
            #[cfg(feature = "device-scty")]
            Self::Scty => scty::configure(platform, id, Some(token)),
            #[cfg(feature = "device-fiberhome")]
            Self::Fiberhome => fiberhome::configure(platform, id, Some(token)),
            #[cfg(feature = "device-realtek")]
            Self::Realtek => realtek::configure(platform, id, Some(token)),
            Self::GenericBridge => generic::service::configure(platform, id, Some(token)),
            _ => {
                let _ = (id, token);
                Err(std::io::Error::new(
                    std::io::ErrorKind::Unsupported,
                    self.status().to_string(),
                ))
            }
        }
    }
    pub(crate) fn auth_layout(self) -> crate::omci_auth::Layout {
        match self {
            #[cfg(feature = "device-hl202")]
            Self::Hl202 => hl202::AUTH_LAYOUT,
            _ => crate::omci_auth::STANDARD,
        }
    }
}
pub fn select(vendor: &str, identity: &crate::model::OnuIdentity) -> Adapter {
    select_with_serial(vendor, identity, None)
}

pub fn select_with_serial(
    vendor: &str,
    identity: &crate::model::OnuIdentity,
    serial: Option<&str>,
) -> Adapter {
    let equipment = identity.equipment_id.as_deref();
    if let Some(id) = serial.and_then(crate::provisioning::configured_omci_adapter) {
        if id != "auto" && !id.is_empty() {
            return select_explicit(&id, vendor, identity, serial);
        }
    }
    #[cfg(feature = "device-hl202")]
    if hl202::matches(vendor, equipment) {
        return Adapter::Hl202;
    }
    #[cfg(feature = "device-hwtc")]
    if vendor == "HWTC" {
        // Only the two verified Huawei models keep the fixed vendor topology.
        // Any other Huawei ONT (e.g. HG8010C) has a different MIB and goes
        // through the vendor-neutral generic bridge instead.
        match hwtc::profile(equipment) {
            profile @ (hwtc::Profile::K155 | hwtc::Profile::V152) => return Adapter::Hwtc(profile),
            hwtc::Profile::Standard => {}
        }
    }
    #[cfg(feature = "device-star")]
    if vendor == "STAR"
        || serial.is_some_and(|sn| sn.starts_with("STAR"))
        || equipment == Some("SG631Z-C")
    {
        return Adapter::Star;
    }
    #[cfg(feature = "device-zte")]
    if let Some(profile) = zte::profile(vendor, equipment, serial) {
        return Adapter::Zte(profile);
    }
    let _ = (vendor, equipment);
    #[cfg(feature = "device-scty")]
    if scty::matches(vendor, equipment) {
        return Adapter::Scty;
    }
    #[cfg(feature = "device-fiberhome")]
    if fiberhome::matches(vendor, equipment) {
        return Adapter::Fiberhome;
    }
    #[cfg(feature = "device-realtek")]
    if realtek::matches(vendor, equipment) {
        return Adapter::Realtek;
    }
    if serial.is_some_and(crate::provisioning::allows_generic_bridge)
        || std::env::var("FTTR_ALLOW_UNSUPPORTED_AS_BRIDGE")
            .is_ok_and(|v| v == "1" || v.eq_ignore_ascii_case("true"))
    {
        return Adapter::GenericBridge;
    }
    Adapter::Unavailable
}

/// Explicit adapter selection forces the requested compiled adapter without
/// rejecting mismatched equipment models; unknown or uncompiled IDs remain unavailable.
pub(crate) fn select_explicit(
    id: &str,
    _vendor: &str,
    identity: &crate::model::OnuIdentity,
    serial: Option<&str>,
) -> Adapter {
    let equipment = identity.equipment_id.as_deref();
    match id {
        "generic" => Adapter::GenericBridge,
        #[cfg(feature = "device-hl202")]
        "hl202" => Adapter::Hl202,
        #[cfg(feature = "device-hwtc")]
        "hwtc" => Adapter::Hwtc(hwtc::profile(equipment)),
        #[cfg(feature = "device-star")]
        "star" => Adapter::Star,
        #[cfg(feature = "device-zte")]
        "zte" => {
            Adapter::Zte(zte::profile(_vendor, equipment, serial).unwrap_or(zte::Profile::G1612))
        }
        #[cfg(feature = "device-scty")]
        "scty" => Adapter::Scty,
        #[cfg(feature = "device-fiberhome")]
        "fiberhome" => Adapter::Fiberhome,
        #[cfg(feature = "device-realtek")]
        "realtek" => Adapter::Realtek,
        _ => {
            let _ = (equipment, serial);
            Adapter::Unavailable
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn generic_bridge_associates_exact_ct_native_identity_without_enrolling_business() {
        assert!(matches_cwmp_adapter(
            "generic",
            "SCTY72ABCDEF",
            "TEWA-768G",
            "308618C8172ABCDEF",
        ));
        assert!(!matches_cwmp_adapter(
            "generic",
            "SCTY72ABCDE0",
            "TEWA-768G",
            "308618C8172ABCDEF",
        ));
        assert!(!matches_cwmp_adapter(
            "generic",
            "SCTY72ABCDEF",
            "TEWA-unknown",
            "308618C8172ABCDEF",
        ));
        assert!(!matches_cwmp_adapter(
            "unavailable",
            "SCTY72ABCDEF",
            "TEWA-768G",
            "308618C8172ABCDEF",
        ));
        assert_eq!(
            select(
                "SCTY",
                &crate::model::OnuIdentity {
                    equipment_id: Some("TEWA-768G".into()),
                    ..Default::default()
                }
            ),
            Adapter::Unavailable,
        );
    }

    #[test]
    #[cfg(not(feature = "mqtt"))]
    fn mqtt_disabled_commands_are_local_and_inventory_remains_available() {
        let _port = crate::MQTT_TEST_PORT.lock().unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:1883").unwrap();
        listener.set_nonblocking(true).unwrap();
        let credentials = CwmpCredentials::default();
        assert!(
            ensure_mqtt_management_wan("02AABBCCDDEE", &[1], 0, false, |_| true)
                .unwrap_err()
                .contains("unavailable")
        );
        assert!(
            read_authenticated_cwmp_settings("02AABBCCDDEE", "http://acs", &credentials)
                .unwrap_err()
                .contains("unavailable")
        );
        assert!(
            configure_authenticated_cwmp_settings("02AABBCCDDEE", "http://acs", &credentials)
                .unwrap_err()
                .contains("unavailable")
        );
        #[cfg(feature = "device-hl202")]
        {
            assert_eq!(
                hl202::command::execute(
                    "02AABBCCDDEE",
                    "fixture",
                    std::time::Duration::from_secs(1)
                )
                .unwrap_err()
                .kind(),
                std::io::ErrorKind::Unsupported
            );
            assert_eq!(
                hl202::command::dispatch("all", "fixture")
                    .unwrap_err()
                    .kind(),
                std::io::ErrorKind::Unsupported
            );
            let typed = hl202::api::Query::Run {
                request: hl202::request::Request {
                    schema_version: 1,
                    target: crate::mqtt_identity::Selection {
                        optical_sn: "H3CT12345678".into(),
                        onu_id: 2,
                        binding_generation: "fixture".into(),
                    },
                    action: hl202::request::Action::AccountsRead,
                },
            };
            let reply = hl202::api::control(&serde_json::to_string(&typed).unwrap(), &[]);
            assert_eq!(reply["error"]["code"], "mqtt_unavailable");
            let identity = crate::model::OnuIdentity {
                equipment_id: Some("HL202-DU-C".into()),
                ..Default::default()
            };
            let adapter = select("H3CT", &identity);
            assert!(adapter.capabilities().business);
            assert!(!adapter.capabilities().shell_commands);
            assert_eq!(adapter.verification_status(), BusinessPhase::Verifying);
        }
        assert!(Adapter::GenericBridge.capabilities().business);
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
    }
    #[test]
    fn selection_respects_vendor_model_rules_and_whole_device_features() {
        let mut identity = crate::model::OnuIdentity::default();
        assert_eq!(select("H3CT", &identity), Adapter::Unavailable);
        // A Huawei ONT without a verified model is not pushed into the fixed
        // vendor topology; selection falls through to the generic-bridge gate,
        // which needs the live serial number.
        assert_eq!(select("HWTC", &identity), Adapter::Unavailable);
        identity.equipment_id = Some("HL202-DU-C".into());
        assert_eq!(
            select("H3CT", &identity).capabilities().business,
            cfg!(feature = "device-hl202")
        );
        assert_eq!(select("OTHER", &identity), Adapter::Unavailable);
        identity.equipment_id = Some("K155-11".into());
        assert_eq!(
            select("HWTC", &identity).capabilities().business,
            cfg!(feature = "device-hwtc")
        );
        #[cfg(feature = "device-hwtc")]
        assert_eq!(
            select("HWTC", &identity),
            Adapter::Hwtc(hwtc::Profile::K155)
        );
        identity.equipment_id = Some("V152".into());
        assert_eq!(
            select("HWTC", &identity).capabilities().business,
            cfg!(feature = "device-hwtc")
        );
        assert_eq!(
            select("HWTC", &identity).status(),
            if cfg!(feature = "device-hwtc") {
                BusinessPhase::Configuring
            } else {
                BusinessPhase::Unsupported
            }
        );
        assert!(!select("HWTC", &identity).capabilities().shell_commands);
        #[cfg(feature = "device-hwtc")]
        {
            assert_eq!(
                select("HWTC", &identity),
                Adapter::Hwtc(hwtc::Profile::V152)
            );
            assert_eq!(
                select("HWTC", &identity).verification_status(),
                BusinessPhase::WanBootstrapPending
            );
            identity.equipment_id = Some("OTHER-HWTC".into());
            assert_eq!(select("HWTC", &identity), Adapter::Unavailable);
        }
        #[cfg(not(feature = "device-hwtc"))]
        assert_eq!(select("HWTC", &identity), Adapter::Unavailable);
    }

    #[test]
    fn scty_selection_is_model_scoped_and_does_not_claim_cwmp() {
        let identity = crate::model::OnuIdentity {
            equipment_id: Some("COMHEART TY-FS803G".into()),
            ..Default::default()
        };
        let adapter = select("SCTY", &identity);
        #[cfg(feature = "device-scty")]
        assert_eq!(adapter, Adapter::Scty);
        #[cfg(not(feature = "device-scty"))]
        assert_eq!(adapter, Adapter::Unavailable);
        assert_eq!(
            adapter.capabilities().business,
            cfg!(feature = "device-scty")
        );
        assert!(!adapter.capabilities().shell_commands);
        assert_eq!(select("OTHER", &identity), Adapter::Unavailable);
        let wrong_model = crate::model::OnuIdentity {
            equipment_id: Some("COMHEART OTHER".into()),
            ..Default::default()
        };
        assert_eq!(select("SCTY", &wrong_model), Adapter::Unavailable);
        assert_eq!(select("SCTY", &Default::default()), Adapter::Unavailable);
        // No observed Inform exists: do not invent an identity alias or contract.
        assert!(!matches_cwmp_adapter(
            "scty",
            "SCTY12345678",
            "COMHEART TY-FS803G",
            "SCTY12345678"
        ));
    }

    #[test]
    fn zte_f650a_selection_is_model_scoped_and_feature_gated() {
        let mut identity = crate::model::OnuIdentity {
            equipment_id: Some("ZXHN F650A(GPON ONU)".into()),
            ..Default::default()
        };
        #[cfg(feature = "device-zte")]
        assert_eq!(select("ZTEG", &identity), Adapter::Zte(zte::Profile::F650a));
        #[cfg(not(feature = "device-zte"))]
        assert_eq!(select("ZTEG", &identity), Adapter::Unavailable);
        assert_eq!(select("OTHER", &identity), Adapter::Unavailable);
        identity.equipment_id = Some("ZXHN F650A-OTHER".into());
        assert_eq!(select("ZTEG", &identity), Adapter::Unavailable);
        identity.equipment_id = Some("ZXHN G1612".into());
        #[cfg(feature = "device-zte")]
        assert_eq!(select("ZTEG", &identity), Adapter::Zte(zte::Profile::G1612));
        #[cfg(not(feature = "device-zte"))]
        assert_eq!(select("ZTEG", &identity), Adapter::Unavailable);
        assert!(!matches_cwmp_adapter(
            "zte",
            "ZTEG12345678",
            "F650A",
            "ZTEG12345678"
        ));
    }

    #[test]
    fn mqtt_connected_dispatch_reports_unsupported_identities() {
        assert!(configure_authenticated_cwmp_settings(
            "HWTC1234ABCD/fttr_data.tar.gz",
            "http://acs",
            &CwmpCredentials::default()
        )
        .is_err());
        assert!(configure_authenticated_cwmp_settings(
            "H3CT12345678",
            "http://acs",
            &CwmpCredentials::default()
        )
        .is_err());
    }

    #[test]
    fn explicit_omci_selection_forces_requested_adapter() {
        let identity = crate::model::OnuIdentity {
            equipment_id: Some("HG6371F-G-C".into()),
            ..Default::default()
        };
        assert_eq!(
            select_explicit("unknown", "H3CT", &identity, None),
            Adapter::Unavailable
        );
        #[cfg(feature = "device-hwtc")]
        assert_eq!(
            select_explicit("hwtc", "H3CT", &identity, None),
            Adapter::Hwtc(hwtc::Profile::Standard)
        );
        #[cfg(feature = "device-hl202")]
        assert_eq!(
            select_explicit("hl202", "HWTC", &identity, None),
            Adapter::Hl202
        );
        #[cfg(feature = "device-fiberhome")]
        assert_eq!(
            select_explicit("fiberhome", "FHTT", &identity, None),
            Adapter::Fiberhome
        );
        assert_eq!(
            select_explicit("generic", "HWTC", &identity, None),
            Adapter::GenericBridge
        );
    }

    #[test]
    fn generic_bridge_adapter_capabilities_and_selection() {
        if std::env::var_os("FTTR_TEST_GENERIC_BRIDGE_CHILD").is_some() {
            assert_eq!(
                select_with_serial("ZTEG", &crate::model::OnuIdentity::default(), None),
                Adapter::GenericBridge
            );
            return;
        }
        let adapter = Adapter::GenericBridge;
        assert!(adapter.capabilities().business);
        assert!(!adapter.capabilities().shell_commands);
        assert_eq!(adapter.status(), BusinessPhase::Configuring);
        assert_eq!(adapter.verification_status(), BusinessPhase::Verifying);

        let identity = crate::model::OnuIdentity::default();
        assert_eq!(
            select_with_serial("ZTEG", &identity, None),
            Adapter::Unavailable
        );

        // Do not change process-wide selection policy while other tests select adapters.
        assert!(std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "devices::tests::generic_bridge_adapter_capabilities_and_selection"
            ])
            .env("FTTR_TEST_GENERIC_BRIDGE_CHILD", "1")
            .env("FTTR_ALLOW_UNSUPPORTED_AS_BRIDGE", "1")
            .status()
            .unwrap()
            .success());
    }

    #[test]
    fn adapter_matches_cwmp_identity_rules() {
        #[cfg(feature = "device-hl202")]
        {
            assert!(matches_cwmp_adapter(
                "hl202",
                "H3CT12345678",
                "HL202-DU-C",
                "SN-H3CT12345678-01"
            ));
            assert!(!matches_cwmp_adapter(
                "hl202",
                "H3CT12345678",
                "OTHER-DEVICE",
                "SN-H3CT12345678-01"
            ));
            assert!(!matches_cwmp_adapter(
                "hl202",
                "H3CT12345678",
                "HL202-DU-C",
                "SN-WRONG-01"
            ));
        }
        #[cfg(feature = "device-hwtc")]
        {
            // HWTC in hex is 48575443
            assert!(matches_cwmp_adapter(
                "hwtc",
                "HWTC12345678",
                "K155-11",
                "4857544312345678"
            ));
            assert!(!matches_cwmp_adapter(
                "hwtc",
                "HWTC12345678",
                "K155-11",
                "WRONGSERIAL"
            ));
            assert!(!matches_cwmp_adapter(
                "hwtc",
                "OTHER12345678",
                "K155-11",
                "4857544312345678"
            ));
        }
        #[cfg(feature = "device-star")]
        {
            assert!(matches_cwmp_adapter(
                "star",
                "STAR1234ABCD",
                "SG631Z-C",
                "MODEL-STAR1234ABCD-01"
            ));
            assert!(!matches_cwmp_adapter(
                "star",
                "STAR1234ABCD",
                "OTHER-DEVICE",
                "MODEL-STAR1234ABCD-01"
            ));
            assert!(!matches_cwmp_adapter(
                "star",
                "STAR1234ABCD",
                "SG631Z-C",
                "MODEL-WRONG-01"
            ));
        }
        #[cfg(feature = "device-zte")]
        {
            assert!(matches_cwmp_adapter(
                "zte",
                "98661024BB88",
                "ZXHN G1612",
                "98661024BB88"
            ));
            assert!(!matches_cwmp_adapter(
                "zte",
                "98661024BB88",
                "OTHER-DEVICE",
                "98661024BB88"
            ));
            assert!(!matches_cwmp_adapter(
                "zte",
                "98661024BB88",
                "ZXHN G1612",
                "OTHER-SERIAL"
            ));
        }
        assert!(!matches_cwmp_adapter(
            "unknown",
            "TEST12345678",
            "CLASS",
            "SERIAL"
        ));
    }
}
pub fn supports_command_provisioning(adapter: &str) -> bool {
    cfg!(all(feature = "device-hl202", feature = "mqtt")) && adapter == "hl202"
}
pub(crate) fn mqtt_report_resources() -> serde_json::Value {
    #[cfg(feature = "device-hl202")]
    {
        hl202::telemetry::store::usage()
    }
    #[cfg(not(feature = "device-hl202"))]
    {
        serde_json::Value::Null
    }
}
