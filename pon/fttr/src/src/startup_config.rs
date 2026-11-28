//! Configuration fixed for the lifetime of the daemon.
use std::sync::OnceLock;

pub(crate) struct StartupConfig {
    pub(crate) acs_state_file: String,
    pub(crate) acs_username: String,
    pub(crate) acs_password: String,
    pub(crate) rx_snapshot: bool,
}

impl Default for StartupConfig {
    fn default() -> Self {
        Self {
            acs_state_file: String::new(),
            acs_username: String::new(),
            acs_password: String::new(),
            rx_snapshot: false,
        }
    }
}

static CONFIG: OnceLock<StartupConfig> = OnceLock::new();

/// Read the same UCI package used by provisioning before starting the runtime.
pub fn initialize() -> Result<(), &'static str> {
    let view = crate::config::load().map_err(|_| "UCI read failed")?;
    let config = from_view(&view)?;
    CONFIG.set(config).map_err(|_| "already initialized")
}

pub(crate) fn get() -> &'static StartupConfig {
    #[cfg(test)]
    return CONFIG.get_or_init(StartupConfig::default);
    #[cfg(not(test))]
    CONFIG.get().expect("startup configuration not initialized")
}

#[cfg(test)]
fn parse(text: &str) -> Result<StartupConfig, &'static str> {
    let view = crate::config::View::parse(text).map_err(|_| "invalid UCI syntax")?;
    from_view(&view)
}
fn from_view(view: &crate::config::View) -> Result<StartupConfig, &'static str> {
    let mut config = StartupConfig::default();
    let mut seen_sections = std::collections::BTreeSet::new();
    let mut seen_options = std::collections::BTreeSet::new();
    for item in &view.sections {
        let section = match item.name.as_deref() {
            Some("acs") => "acs",
            Some("fiberhome") => "fiberhome",
            _ => "",
        };
        if !section.is_empty() && !seen_sections.insert(section) {
            return Err("duplicate startup configuration section");
        }
        for option in &item.options {
            let key = option.key.as_str();
            let relevant = matches!(
                (section, key),
                ("acs", "state_file" | "username" | "password") | ("fiberhome", "rx_snapshot")
            );
            if !relevant {
                continue;
            }
            if option.is_list || !seen_options.insert((section, option.key.clone())) {
                return Err("startup settings require unique scalar options");
            }
            match (section, key) {
                ("acs", "state_file") => {
                    // Historical dump paths no longer select another writer.
                    // Only unresolved RPC metadata uses the shared tmp store.
                }
                ("acs", "username") => config.acs_username = option.value.clone(),
                ("acs", "password") => config.acs_password = option.value.clone(),
                ("fiberhome", "rx_snapshot") => {
                    config.rx_snapshot = match option.value.as_str() {
                        "1" | "true" => true,
                        "" | "0" | "false" => false,
                        _ => return Err("invalid fiberhome rx_snapshot"),
                    }
                }
                _ => unreachable!(),
            }
        }
    }
    config.acs_state_file = crate::provisioning::runtime_store::Record::Acs.path("");
    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::parse;

    #[test]
    fn packaged_configuration_is_accepted() {
        let config = parse(include_str!("../../files/fttr.config")).unwrap();
        assert_eq!(
            config.acs_state_file,
            crate::provisioning::runtime_store::Record::Acs.path("")
        );
        assert!(config.acs_username.is_empty());
        assert!(!config.rx_snapshot);
    }

    #[test]
    fn defaults_and_explicit_empty_values() {
        let defaults = parse("").unwrap();
        assert_eq!(
            defaults.acs_state_file,
            crate::provisioning::runtime_store::Record::Acs.path("")
        );
        assert!(defaults.acs_username.is_empty());
        assert!(!defaults.rx_snapshot);
        let config = parse("config acs 'acs'\n option state_file ''\n option username ''\n option password 'a'\\''b'\n").unwrap();
        assert_eq!(
            config.acs_state_file,
            crate::provisioning::runtime_store::Record::Acs.path("")
        );
        assert_eq!(config.acs_password, "a'b");
    }

    #[test]
    fn snapshot_boolean_and_invalid_values() {
        for (value, expected) in [
            ("1", true),
            ("true", true),
            ("", false),
            ("0", false),
            ("false", false),
        ] {
            let text = format!("config fiberhome 'fiberhome'\n option rx_snapshot '{value}'\n");
            assert_eq!(parse(&text).unwrap().rx_snapshot, expected);
        }
        assert!(parse("config fiberhome 'fiberhome'\n option rx_snapshot 'yes'\n").is_err());
        assert!(parse("config acs 'acs'\n list username 'user'\n").is_err());
        assert!(
            parse("config acs 'acs'\n option username 'one'\n option username 'two'\n").is_err()
        );
        assert!(parse("config acs 'acs'\n option password 'unterminated\n").is_err());
    }
}
