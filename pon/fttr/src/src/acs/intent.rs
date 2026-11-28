//! Compact original write intent; values are never retained in plaintext.
use super::soap;
use crate::provisioning::runtime_store::SecretMacKey;
use serde::{Deserialize, Serialize};
use std::io;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(super) struct Intent {
    pub(super) parameter_key: String,
    pub(super) action: Action,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum Action {
    Set { parameters: Vec<Parameter> },
    Add { object: String },
    Delete { object: String },
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(super) struct Parameter {
    pub(super) name: String,
    pub(super) value_type: String,
    pub(super) value_mac: String,
}

impl Intent {
    pub(super) fn capture(args: &soap::WriteArgs<'_>) -> io::Result<Self> {
        let action = match args {
            soap::WriteArgs::Set(values) => {
                let key = SecretMacKey::load_or_create().map_err(io::Error::other)?;
                let parameters = values
                    .iter()
                    .map(|p| Parameter {
                        name: p.name.into(),
                        value_type: p.kind.into(),
                        value_mac: key.sign(p.name, p.value),
                    })
                    .collect();
                Action::Set { parameters }
            }
            soap::WriteArgs::Add(object) => Action::Add {
                object: (*object).into(),
            },
            soap::WriteArgs::Delete(object) => Action::Delete {
                object: (*object).into(),
            },
        };
        Ok(Self {
            parameter_key: super::token()?,
            action,
        })
    }

    pub(super) fn matches_method(&self, method: &str) -> bool {
        if self.parameter_key.len() != 32
            || !self.parameter_key.bytes().all(|b| b.is_ascii_hexdigit())
        {
            return false;
        }
        match (&self.action, method) {
            (Action::Set { parameters }, "SetParameterValues") => {
                !parameters.is_empty()
                    && parameters.len() <= soap::MAX_SET_PARAMETERS
                    && parameters.iter().enumerate().all(|(i, p)| {
                        !p.name.is_empty()
                            && soap::valid_path(&p.name)
                            && soap::valid_value_type(&p.value_type)
                            && p.value_mac.len() == 64
                            && p.value_mac.bytes().all(|b| b.is_ascii_hexdigit())
                            && !parameters[..i].iter().any(|other| other.name == p.name)
                    })
            }
            (Action::Add { object }, "AddObject") | (Action::Delete { object }, "DeleteObject") => {
                !object.is_empty() && soap::valid_path(object)
            }
            _ => false,
        }
    }

    /// Exercise lexical comparison before a recovery consumer is wired up.
    /// A matching key and values do not settle an operation or authorize replay.
    #[cfg(test)]
    fn observe_set(
        &self,
        key_name: &str,
        data: &crate::cwmp::response::Data,
    ) -> io::Result<Observation> {
        let Action::Set { parameters } = &self.action else {
            return Ok(Observation::Incomplete);
        };
        if parameters.is_empty()
            || parameters.len() > soap::MAX_SET_PARAMETERS
            || !matches!(
                key_name,
                "Device.ManagementServer.ParameterKey"
                    | "InternetGatewayDevice.ManagementServer.ParameterKey"
            )
            || parameters.iter().any(|p| p.name == key_name)
        {
            return Ok(Observation::Incomplete);
        }
        let Some(rows) = data.rows() else {
            return Ok(Observation::Incomplete);
        };
        if rows.len() != parameters.len() + 1 {
            return Ok(Observation::Incomplete);
        }
        let mut observed_key = None;
        let mut observed = [None; soap::MAX_SET_PARAMETERS];
        for row in rows {
            let crate::cwmp::response::Row::Value { name, value, .. } = row else {
                return Ok(Observation::Incomplete);
            };
            let (Some(name), Some(value)) = (name.as_str(), value.as_str()) else {
                return Ok(Observation::Incomplete);
            };
            if name == key_name {
                if observed_key.replace(value).is_some() {
                    return Ok(Observation::Incomplete);
                }
            } else if let Some(index) = parameters.iter().position(|p| p.name == name) {
                if observed[index].replace(value).is_some() {
                    return Ok(Observation::Incomplete);
                }
            } else {
                return Ok(Observation::Incomplete);
            }
        }
        // Distinct requested rows and the exact row count imply full coverage.
        if observed_key != Some(&self.parameter_key) {
            return Ok(Observation::Different);
        }
        let key = SecretMacKey::load().map_err(io::Error::other)?;
        for (parameter, value) in parameters.iter().zip(observed) {
            let Some(value) = value else {
                return Ok(Observation::Incomplete);
            };
            if key.sign(&parameter.name, value) != parameter.value_mac {
                return Ok(Observation::Different);
            }
        }
        Ok(Observation::Matching)
    }
}

#[cfg(test)]
#[derive(Debug, PartialEq, Eq)]
enum Observation {
    Incomplete,
    Different,
    Matching,
}

#[cfg(test)]
mod tests {
    use super::soap::WriteArgs;
    use super::Intent;
    use serde_json::json;

    #[test]
    fn captured_intents_always_pass_restart_validation() {
        for (method, args) in [
            ("AddObject", json!({"object": "Device.Test."})),
            ("DeleteObject", json!({"object": "Device.Test.1."})),
        ] {
            let intent =
                Intent::capture(&WriteArgs::parse(method, &args).unwrap().unwrap()).unwrap();
            assert!(intent.matches_method(method));
            let restored: Intent =
                serde_json::from_value(serde_json::to_value(intent).unwrap()).unwrap();
            assert!(restored.matches_method(method));
            assert!(WriteArgs::parse(method, &json!({"object": "Device.\nTest."})).is_err());
        }
    }

    #[test]
    fn separate_writes_have_distinct_bounded_parameter_keys() {
        for method in ["AddObject", "DeleteObject"] {
            let args = json!({"object": "Device.Test."});
            let write = WriteArgs::parse(method, &args).unwrap().unwrap();
            let first = Intent::capture(&write).unwrap();
            let second = Intent::capture(&write).unwrap();
            assert_ne!(first.parameter_key, second.parameter_key);
            for intent in [first, second] {
                let xml = write.request("urn:dslforum-org:cwmp-1-0", 1, &intent.parameter_key);
                assert!(xml.contains(&format!(
                    "<ParameterKey>{}</ParameterKey>",
                    intent.parameter_key
                )));
                assert_eq!(intent.parameter_key.len(), 32);
            }
        }
    }

    #[test]
    fn original_set_observation_rejects_missing_duplicate_and_wrong_key() {
        let name = "Device.Test.Value";
        let key_name = "Device.ManagementServer.ParameterKey";
        let args = json!({"parameters":[{"name":name,"value":"fixture-value"}]});
        let write = WriteArgs::parse("SetParameterValues", &args)
            .unwrap()
            .unwrap();
        let intent = Intent::capture(&write).unwrap();
        let row = |name: &str, value: &str| crate::cwmp::response::Row::Value {
            name: json!(name),
            value: json!(value),
            kind: serde_json::Value::Null,
        };
        let data = |rows| crate::cwmp::response::Data {
            parameters: Some(rows),
            ..Default::default()
        };
        let matching = data(vec![
            row(key_name, &intent.parameter_key),
            row(name, "fixture-value"),
        ]);
        assert_eq!(
            intent.observe_set(key_name, &matching).unwrap(),
            super::Observation::Matching
        );
        let wrong_key = data(vec![row(key_name, "other"), row(name, "fixture-value")]);
        assert_eq!(
            intent.observe_set(key_name, &wrong_key).unwrap(),
            super::Observation::Different
        );
        let duplicate = data(vec![
            row(key_name, &intent.parameter_key),
            row(key_name, &intent.parameter_key),
        ]);
        assert_eq!(
            intent.observe_set(key_name, &duplicate).unwrap(),
            super::Observation::Incomplete
        );
        let missing = data(vec![row(key_name, &intent.parameter_key)]);
        assert_eq!(
            intent.observe_set(key_name, &missing).unwrap(),
            super::Observation::Incomplete
        );
        let wrong_value = data(vec![
            row(key_name, &intent.parameter_key),
            row(name, "other"),
        ]);
        assert_eq!(
            intent.observe_set(key_name, &wrong_value).unwrap(),
            super::Observation::Different
        );
    }
}
