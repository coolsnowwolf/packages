//! Decode observations before applying vendor-independent management protection.
use serde_json::Value;
pub struct ObservedWan {
    pub services: Vec<String>,
    pub tagged: Option<bool>,
}
impl ObservedWan {
    pub fn protected(&self) -> bool {
        self.services.iter().any(|s| s == "TR069")
            || self.services.iter().any(|s| s == "INTERNET") && self.tagged != Some(true)
    }
}
pub fn decode<T: for<'a> std::ops::Index<&'a str, Output = Value>>(
    values: &[T],
    connection: &str,
) -> Vec<ObservedWan> {
    let Some((parent, _)) = connection
        .rsplit_once(".WANIPConnection.")
        .or_else(|| connection.rsplit_once(".WANPPPConnection."))
    else {
        return Vec::new();
    };
    let parent = format!("{parent}.");
    let read = |name: &str| {
        let matching: Vec<_> = values
            .iter()
            .filter(|v| v["name"] == name)
            .map(|v| v["value"].as_str())
            .collect();
        let first = matching.first().copied().flatten()?;
        matching
            .iter()
            .all(|value| *value == Some(first))
            .then(|| first.to_owned())
    };
    let mut result = Vec::new();
    for c in super::registry::available() {
        for row in values
            .iter()
            .filter(|v| v["name"] == c.wan.service.path(&parent, connection))
        {
            let Some(service) = row["value"].as_str() else {
                continue;
            };
            result.push(ObservedWan {
                services: super::codec::service_tokens(service)
                    .map(str::to_ascii_uppercase)
                    .collect(),
                tagged: c.wan.tagged(|name| {
                    let field = if name == c.wan.vlan.name {
                        c.wan.vlan
                    } else {
                        c.wan.enabled?
                    };
                    read(&field.path(&parent, connection))
                }),
            });
        }
    }
    result
}
pub fn protected<T: for<'a> std::ops::Index<&'a str, Output = Value>>(
    values: &[T],
    connection: &str,
) -> bool {
    decode(values, connection)
        .iter()
        .any(ObservedWan::protected)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn management_protection_spans_namespaces_and_conflicting_readback() {
        let connection =
            "InternetGatewayDevice.WANDevice.1.WANConnectionDevice.1.WANIPConnection.1.";
        let values = vec![
            json!({"name":format!("{connection}X_CU_ServiceList"),"value":"OTHER"}),
            json!({"name":format!("{connection}X_HW_SERVICELIST"),"value":"OTHER"}),
            json!({"name":format!("{connection}X_HW_SERVICELIST"),"value":"TR069_INTERNET"}),
        ];
        assert!(protected(&values, connection));
        assert!(!protected(
            &values,
            "InternetGatewayDevice.WANDevice.1.WANConnectionDevice.2.WANIPConnection.1."
        ));
        let values = vec![
            json!({"name":format!("{connection}X_HW_SERVICELIST"),"value":"INTERNET"}),
            json!({"name":format!("{connection}X_HW_VLAN"),"value":"66"}),
            json!({"name":format!("{connection}X_HW_VLAN"),"value":"0"}),
        ];
        assert!(protected(&values, connection));
        assert!(!protected(&values[..2], connection));
    }
}
