//! HL202-only TCAPI execution, authorized by the per-device management coordinator.
use super::command;
use std::time::Duration;

const TELEMETRY_SUFFIX: &str = "/fttr_data.tar.gz";

pub fn mqtt_target(device_id: &str) -> Option<String> {
    command::normalize_mac(
        device_id
            .strip_suffix(TELEMETRY_SUFFIX)
            .unwrap_or(device_id),
    )
    .ok()
}

/// Read only the settings we manage; never return credentials or the full TCAPI dump.
pub fn read_authenticated_settings(
    device_id: &str,
    acs_url: &str,
    credentials: &crate::devices::CwmpCredentials,
) -> Result<bool, String> {
    crate::runtime::block_on(read_authenticated_settings_async(
        device_id,
        acs_url,
        credentials,
    ))
}
pub async fn read_authenticated_settings_async(
    device_id: &str,
    acs_url: &str,
    credentials: &crate::devices::CwmpCredentials,
) -> Result<bool, String> {
    let target = mqtt_target(device_id).ok_or("invalid MQTT command target")?;
    let transaction =
        command::Transaction::acquire(&target).map_err(|_| "device command transaction active")?;
    let script = format!(
        "if {}; then printf 'FTTR_CWMP_MATCH\\n'; else printf 'FTTR_CWMP_DIFF\\n'; fi",
        authenticated_tests(Some(acs_url), credentials).join(" && ")
    );
    let result = transaction
        .execute(&script, Duration::from_secs(30))
        .await
        .map_err(|_| "CWMP settings read transport failed")?;
    if !result.success() {
        return Err(format!(
            "CWMP settings read failed (status={}, exit={:?}); outcome unknown",
            result.status, result.exit_code
        ));
    }
    match result.output.as_deref().map(str::trim) {
        Some("FTTR_CWMP_MATCH") => Ok(true),
        Some("FTTR_CWMP_DIFF") => Ok(false),
        _ => Err("CWMP settings read returned no valid result".into()),
    }
}
pub fn configure_settings(
    device_id: &str,
    acs_url: &str,
    credentials: &crate::devices::CwmpCredentials,
) -> Result<(), String> {
    crate::runtime::block_on(configure_settings_async(device_id, acs_url, credentials))
}
pub async fn configure_settings_async(
    device_id: &str,
    acs_url: &str,
    credentials: &crate::devices::CwmpCredentials,
) -> Result<(), String> {
    let target = mqtt_target(device_id).ok_or("invalid MQTT command target")?;
    let transaction =
        command::Transaction::acquire(&target).map_err(|_| "device command transaction active")?;
    let script = authenticated_script(Some(acs_url), credentials);
    let result = transaction
        .execute(&script, Duration::from_secs(90))
        .await
        .map_err(|_| "CWMP configuration transport failed")?;
    if !result.success()
        || !result
            .output
            .as_deref()
            .is_some_and(|s| s.trim() == "HL202_CWMP_SETTINGS_VERIFIED")
    {
        return Err(format!(
            "CWMP configuration not verified (status={}, exit={:?}, out={:?}); readback required",
            result.status, result.exit_code, result.output
        ));
    }
    Ok(())
}

#[cfg(test)]
fn config_script(acs_url: Option<&str>) -> String {
    authenticated_script(acs_url, &crate::devices::CwmpCredentials::default())
}
fn authenticated_script(
    acs_url: Option<&str>,
    credentials: &crate::devices::CwmpCredentials,
) -> String {
    let mut commands = Vec::new();
    commands.push("changed=0".to_string());
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    commands.push(format!("date -s @{now} >/dev/null 2>&1 || true"));
    commands.push("if [ \"$(tcapi get DeviceInfo_DevParaDynamic mainGatewayOUI)\" != 000FE2 ]; then tcapi set DeviceInfo_DevParaDynamic mainGatewayOUI 000FE2 || exit 1; changed=1; fi".into());
    commands.push("if [ \"$(tcapi get DeviceInfo_DevParaDynamic mainGatewaySN)\" != 1C94685AF429 ]; then tcapi set DeviceInfo_DevParaDynamic mainGatewaySN 1C94685AF429 || exit 1; changed=1; fi".into());
    commands.push("if [ \"$(tcapi get Cwmp_Entry DevInfCfgFlag)\" != 1 ]; then tcapi set Cwmp_Entry DevInfCfgFlag 1 || exit 1; changed=1; fi".into());
    commands.push("case \"$(tcapi get Cwmp_Entry devregInform)\" in 0|1) true;; *) tcapi set Cwmp_Entry devregInform 1 || exit 1; changed=1;; esac".into());
    if let Some(acs_url) = acs_url {
        let host = acs_url
            .strip_prefix("http://")
            .or_else(|| acs_url.strip_prefix("https://"))
            .and_then(|s| s.split([':', '/']).next())
            .unwrap_or("192.168.46.1");

        commands.push(format!(
            "if [ \"$(tcapi get Cwmp_Entry acsUrl)\" != {0} ]; then tcapi set Cwmp_Entry acsUrl {0} || exit 1; changed=1; fi",
            shell_quote(acs_url)
        ));
        commands.push(
            "case \"$(tcapi get Cwmp_Entry SyncAcsUrl)\" in No) tcapi set Cwmp_Entry SyncAcsUrl Yes || exit 1;; esac".into(),
        );
        commands.push("case \"$(tcapi get CwmpRoute_Entry ifStatus0)\" in Up|old|new) true;; *) tcapi set CwmpRoute_Entry ifStatus0 Up || exit 1; changed=1;; esac".into());
        commands.push(format!("if [ \"$(tcapi get CwmpRoute_Entry ifGateway0)\" != {0} ]; then tcapi set CwmpRoute_Entry ifGateway0 {0} || exit 1; changed=1; fi", shell_quote(host)));
    }
    for (key, value) in [("Active", "Yes")] {
        commands.push(format!("if [ \"$(tcapi get Cwmp_Entry {key})\" != {value} ]; then tcapi set Cwmp_Entry {key} {value} || exit 1; changed=1; fi"));
    }
    // Also persist matching runtime values after a previous command with an unknown outcome.
    if !credentials.username.is_empty() {
        for (key, value) in [
            ("acsUserName", &credentials.username),
            ("acsPassword", &credentials.password),
        ] {
            commands.push(format!("if [ \"$(tcapi get Cwmp_Entry {key})\" != {0} ]; then tcapi set Cwmp_Entry {key} {0} || exit 1; fi",shell_quote(value)));
        }
    }
    commands.push(
        "tcapi commit Cwmp && tcapi commit CwmpRoute && tcapi commit DeviceInfo && tcapi save"
            .into(),
    );
    commands.push("printf 'HL202_CWMP_SETTINGS_VERIFIED\\n'".into());
    commands.join(" && ")
}
fn authenticated_tests(
    url: Option<&str>,
    credentials: &crate::devices::CwmpCredentials,
) -> Vec<String> {
    let mut tests = settings_tests(url);
    if !credentials.username.is_empty() {
        for (key, value) in [
            ("acsUserName", &credentials.username),
            ("acsPassword", &credentials.password),
        ] {
            tests.push(format!(
                "test \"$(tcapi get Cwmp_Entry {key})\" = {}",
                shell_quote(value)
            ));
        }
    }
    tests
}

fn settings_tests(acs_url: Option<&str>) -> Vec<String> {
    let mut commands = Vec::new();
    commands.push("test \"$(tcapi get DeviceInfo_DevParaDynamic mainGatewayOUI)\" = 000FE2".into());
    commands.push(
        "test \"$(tcapi get DeviceInfo_DevParaDynamic mainGatewaySN)\" = 1C94685AF429".into(),
    );
    commands.push("test \"$(tcapi get Cwmp_Entry DevInfCfgFlag)\" = 1".into());
    commands.push(
        "case \"$(tcapi get Cwmp_Entry devregInform)\" in 0|1) true;; *) false;; esac".into(),
    );
    if let Some(acs_url) = acs_url {
        let host = acs_url
            .strip_prefix("http://")
            .or_else(|| acs_url.strip_prefix("https://"))
            .and_then(|s| s.split([':', '/']).next())
            .unwrap_or("192.168.46.1");

        commands.push(format!(
            "test \"$(tcapi get Cwmp_Entry acsUrl)\" = {}",
            shell_quote(acs_url)
        ));
        // Some stock profiles do not expose this optional vendor field. Verify it only
        // when its recognized Yes/No values establish that the field exists.
        commands.push(
            "case \"$(tcapi get Cwmp_Entry SyncAcsUrl)\" in No) false;; *) true;; esac".into(),
        );
        commands.push(
            "case \"$(tcapi get CwmpRoute_Entry ifStatus0)\" in Up|old|new) true;; *) false;; esac"
                .into(),
        );
        commands.push(format!(
            "test \"$(tcapi get CwmpRoute_Entry ifGateway0)\" = {}",
            shell_quote(host)
        ));
    }
    commands.push("test \"$(tcapi get Cwmp_Entry Active)\" = Yes".into());
    commands
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_actual_mqtt_mac_without_a_test_device_allowlist() {
        assert!(mqtt_target("02AABBCCDDEE").is_some());
        assert!(mqtt_target("02aabbccddee/fttr_data.tar.gz").is_some());
        assert!(mqtt_target("02:11:22:33:44:55").is_some());
        assert!(mqtt_target("HWTC3C6DE5B3/fttr_data.tar.gz").is_none());
        assert!(mqtt_target("H3CT12345678").is_none());
        assert!(mqtt_target("../02AABBCCDDEE").is_none());
    }

    #[test]
    fn shell_quote_preserves_single_quotes() {
        assert_eq!(
            shell_quote("http://acs.example/a'b"),
            "'http://acs.example/a'\"'\"'b'"
        );
    }

    #[test]
    fn config_script_sets_acs_url_without_owning_periodic_policy() {
        let script = config_script(Some("http://192.168.1.1:7547/cwmp"));
        println!("SCRIPT: [{script}]");
        assert!(script.contains("tcapi set Cwmp_Entry acsUrl 'http://192.168.1.1:7547/cwmp'"));
        assert!(script.contains("tcapi set Cwmp_Entry SyncAcsUrl Yes"));
        assert!(!script.contains("periodInterval"));
        assert!(!script.contains("periodActive"));
        assert!(script.contains("tcapi set DeviceInfo_DevParaDynamic mainGatewayOUI 000FE2"));
        assert!(script.contains("tcapi set DeviceInfo_DevParaDynamic mainGatewaySN 1C94685AF429"));
        assert!(script.contains("case \"$(tcapi get CwmpRoute_Entry ifStatus0)\" in Up|old|new) true;; *) tcapi set CwmpRoute_Entry ifStatus0 Up"));
        assert!(script.contains("tcapi set CwmpRoute_Entry ifGateway0 '192.168.1.1'"));
        assert!(script.contains(
            "if [ \"$(tcapi get Cwmp_Entry acsUrl)\" != 'http://192.168.1.1:7547/cwmp' ]"
        ));
        assert!(script.contains("case \"$(tcapi get Cwmp_Entry SyncAcsUrl)\" in No) tcapi set"));
        assert!(script.contains(
            "tcapi commit Cwmp && tcapi commit CwmpRoute && tcapi commit DeviceInfo && tcapi save"
        ));
        assert!(script.contains("printf 'HL202_CWMP_SETTINGS_VERIFIED\\n'"));

        let tests = settings_tests(Some("http://192.168.1.1:7547/cwmp")).join(" && ");
        assert!(tests.contains("tcapi get Cwmp_Entry acsUrl"));
        assert!(tests.contains("tcapi get Cwmp_Entry SyncAcsUrl)\" in No) false"));
        assert!(!tests.contains("periodInterval"));
    }

    #[test]
    fn authentication_values_are_quoted_and_verified_without_a_credentials_dump() {
        let credentials = crate::devices::CwmpCredentials {
            username: "test'user".into(),
            password: "a'b;$(false)".into(),
        };
        let script = authenticated_script(Some("http://192.0.2.1/cwmp"), &credentials);
        assert!(script.contains("tcapi set Cwmp_Entry acsUserName 'test'\"'\"'user'"));
        assert!(script.contains("tcapi set Cwmp_Entry acsPassword 'a'\"'\"'b;$(false)'"));
        assert!(script.contains("tcapi get Cwmp_Entry acsUserName"));
        assert!(script.contains("tcapi get Cwmp_Entry acsPassword"));
        assert!(!script.contains("tcapi show Cwmp"));
    }

    #[test]
    fn empty_credentials_leave_existing_device_values_untouched() {
        let script = authenticated_script(
            Some("http://192.0.2.1/cwmp"),
            &crate::devices::CwmpCredentials::default(),
        );
        assert!(!script.contains("tcapi set Cwmp_Entry acsUserName"));
        assert!(!script.contains("tcapi set Cwmp_Entry acsPassword"));
        assert!(!script.contains("tcapi get Cwmp_Entry acsUserName"));
        assert!(!script.contains("tcapi get Cwmp_Entry acsPassword"));
    }
    #[test]
    fn missing_optional_sync_field_does_not_prevent_url_commit_and_readback() {
        let mock = r#"
url=old; interval=43200; active=No; periodic=No; committed=0
oui=000FE2; sn=1C94685AF429; devinf=1; devreg=1; ifstatus=Up; gw=192.0.2.1
tcapi() {
    case "$1:$3" in
        get:acsUrl) printf '%s' "$url";;
        get:periodInterval) printf '%s' "$interval";;
        get:Active) printf '%s' "$active";;
        get:periodActive) printf '%s' "$periodic";;
        get:SyncAcsUrl) printf 'no node information'; return 1;;
        get:mainGatewayOUI) printf '%s' "$oui";;
        get:mainGatewaySN) printf '%s' "$sn";;
        get:DevInfCfgFlag) printf '%s' "$devinf";;
        get:devregInform) printf '%s' "$devreg";;
        get:ifStatus0) printf '%s' "$ifstatus";;
        get:ifGateway0) printf '%s' "$gw";;
        get:GateWay) printf '%s' "$gw";;
        set:acsUrl) url="$4";;
        set:periodInterval) interval="$4";;
        set:Active) active="$4";;
        set:periodActive) periodic="$4";;
        set:SyncAcsUrl) return 3;;
        set:mainGatewayOUI) oui="$4";;
        set:mainGatewaySN) sn="$4";;
        set:DevInfCfgFlag) devinf="$4";;
        set:devregInform) devreg="$4";;
        set:ifStatus0) ifstatus="$4";;
        set:ifGateway0) gw="$4";;
        set:GateWay) gw="$4";;
        commit:*) committed=1;;
        save:*) test "$committed" = 1;;
        *) return 4;;
    esac
}
"#;
        let script = format!(
            "{mock}\n{} && test \"$interval:$periodic\" = '43200:No'",
            config_script(Some("http://192.0.2.1:7547/cwmp"))
        );
        let output = std::process::Command::new("sh")
            .args(["-c", &script])
            .output()
            .unwrap();
        assert!(output.status.success(), "{:?}", output);
        assert_eq!(
            String::from_utf8(output.stdout).unwrap(),
            "HL202_CWMP_SETTINGS_VERIFIED\n"
        );
    }
}
