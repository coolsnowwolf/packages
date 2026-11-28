//! Cache resolved evidence, not credentials or device values. Caller supplies a
//! registration/bootstrap epoch and a schema fingerprint from readonly discovery.
use super::{contract::Contract, identity::Identity, registry};

fn valid_serial(serial: &str) -> bool {
    if serial.len() != 12 || !serial.bytes().all(|b| b.is_ascii_alphanumeric()) {
        return false;
    }
    true
}
fn records() -> &'static std::sync::Mutex<std::collections::BTreeMap<String, serde_json::Value>> {
    static CACHE: std::sync::OnceLock<
        std::sync::Mutex<std::collections::BTreeMap<String, serde_json::Value>>,
    > = std::sync::OnceLock::new();
    CACHE.get_or_init(Default::default)
}
pub fn load_record(serial: &str) -> Option<serde_json::Value> {
    records().lock().ok()?.get(serial).cloned()
}
pub fn save_record(serial: &str, value: &serde_json::Value) -> Result<(), String> {
    if !valid_serial(serial) || serde_json::to_vec(value).map_err(|e| e.to_string())?.len() > 32768
    {
        return Err("invalid or oversized cache record".into());
    }
    let mut cache = records().lock().map_err(|_| "cache unavailable")?;
    if cache.len() >= 128 && !cache.contains_key(serial) {
        return Err("contract cache capacity reached".into());
    }
    cache.insert(serial.into(), value.clone());
    Ok(())
}
pub fn invalidate_record(serial: &str) {
    if let Ok(mut cache) = records().lock() {
        cache.remove(serial);
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Scope {
    pub identity: Identity,
    pub epoch: u64,
    pub schema: String,
    pub contract_version: u32,
}
pub struct CachedResolution {
    scope: Scope,
    contract: &'static str,
    invalidated: bool,
}
impl CachedResolution {
    pub fn new(scope: Scope, contract: &'static Contract) -> Self {
        Self {
            scope,
            contract: contract.id,
            invalidated: false,
        }
    }
    pub fn resolve(&self, scope: &Scope) -> Option<&'static Contract> {
        if self.invalidated
            || &self.scope != scope
            || scope.schema.is_empty()
            || scope.contract_version != registry::VERSION
        {
            return None;
        }
        registry::lookup(self.contract).ok()
    }
    /// Parameter faults and BOOTSTRAP events invalidate prior evidence.
    pub fn invalidate(&mut self) {
        self.invalidated = true;
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cache_never_crosses_identity_firmware_schema_or_bootstrap_epochs() {
        let mut scope = Scope {
            identity: Identity {
                optical_vendor: "HWTC".into(),
                optical_serial: "s1".into(),
                cwmp_serial: "c1".into(),
                product_class: "K155".into(),
                software_version: "v1".into(),
                hardware_version: "h1".into(),
            },
            epoch: 1,
            schema: "schema-1".into(),
            contract_version: registry::VERSION,
        };
        let mut cache = CachedResolution::new(scope.clone(), &super::super::vendor::hw::BASE);
        assert!(cache.resolve(&scope).is_some());
        scope.identity.software_version = "v2".into();
        assert!(cache.resolve(&scope).is_none());
        scope.identity.software_version = "v1".into();
        scope.epoch += 1;
        assert!(cache.resolve(&scope).is_none());
        scope.epoch = 1;
        scope.schema = "schema-2".into();
        assert!(cache.resolve(&scope).is_none());
        scope.schema = "schema-1".into();
        cache.invalidate();
        assert!(cache.resolve(&scope).is_none());
    }
}
