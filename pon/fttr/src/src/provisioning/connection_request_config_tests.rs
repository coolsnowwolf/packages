use super::parse;

const DEVICE: &str = "config template 'home'\nconfig group 'home_group'\n option template 'home'\nconfig device 'child'\n option optical_sn 'ZTEG12345678'\n option omci_adapter 'generic'\n option bootstrap_adapter 'none'\n option provisioning_adapter 'none'\n option group 'home_group'\n";

#[test]
fn connection_request_is_enabled_when_options_are_absent() {
    let config = parse(DEVICE).unwrap();

    let request = &config.devices[0].connection_request;

    assert!(request.enabled);
    assert!(request.username.is_empty());
    assert!(request.password.is_empty());
}

#[test]
fn connection_request_rejects_ports_outside_the_tcp_range() {
    for port in ["0", "65536", "-1", "abc"] {
        let text = format!("{DEVICE} option connection_request_port '{port}'\n");
        let result = parse(&text);
        assert!(result.is_err(), "accepted {port}");
    }
}

#[test]
fn connection_request_preserves_explicit_credentials_through_template_expansion() {
    let text = format!(
        "{DEVICE}{}",
        r#" option connection_request '1'
 option connection_request_username 'cr user'
 option connection_request_password 'p\word "quoted"'\''s'
"#
    );

    let config = parse(&text).unwrap();

    let request = &config.devices[0].connection_request;
    assert!(request.enabled);
    assert_eq!(request.username, "cr user");
    assert_eq!(request.password, "p\\word \"quoted\"'s");
}

#[test]
fn connection_request_toggle_changes_revision_without_changing_credentials() {
    let text = format!("{DEVICE} option connection_request '0'\n option connection_request_username 'existing user'\n option connection_request_password 'existing secret'\n");
    let disabled = parse(&text).unwrap();

    let enabled = parse(&text.replace("connection_request '0'", "connection_request '1'")).unwrap();

    assert_ne!(disabled.revision, enabled.revision);
    assert!(enabled.devices[0].connection_request.enabled);
    assert_eq!(
        enabled.devices[0].connection_request.username,
        disabled.devices[0].connection_request.username
    );
    assert_eq!(
        enabled.devices[0].connection_request.password,
        disabled.devices[0].connection_request.password
    );
}

#[test]
fn connection_request_rejects_an_invalid_opt_in_boolean() {
    let text = format!("{DEVICE} option connection_request 'sometimes'\n");

    let result = parse(&text);

    assert!(result.is_err());
}

#[test]
#[cfg_attr(
    not(all(feature = "device-hl202", any(feature = "acs", feature = "mqtt"))),
    ignore = "requires HL202 and a provisioning transport"
)]
fn selecting_a_business_contract_does_not_select_connection_request_credentials() {
    let text = DEVICE.replace(
        "option provisioning_adapter 'none'",
        if cfg!(feature = "acs") {
            "option provisioning_adapter 'hl202-cu'"
        } else {
            "option provisioning_adapter 'hl202-mqtt'"
        },
    );
    let text = if cfg!(feature = "acs") {
        text.replace(
            "option bootstrap_adapter 'none'",
            "option bootstrap_adapter 'manual-acs'",
        )
    } else {
        text
    };
    let config = parse(&text).unwrap();

    let request = &config.devices[0].connection_request;
    assert!(request.enabled);
    assert!(request.defaults);
    assert!(request.username.is_empty());
    assert!(request.password.is_empty());
    assert_eq!(request.port.get(), 7547);
}
