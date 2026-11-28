//! Synthetic resources for protocol tests, independent of compiled platforms.
use crate::platform::resources::{OnuResources, Resources};
pub(super) const RESOURCES: Resources = Resources {
    onu_ids: &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15],
    management_interfaces: &[],
    capture_interface: None,
    downstream_interfaces: &[],
    allocations: &allocations(),
};
const fn allocations() -> [OnuResources; 16] {
    let mut result = [OnuResources {
        onu_id: 0,
        alloc_id: 0,
        gem_id: 0,
        interface: None,
    }; 16];
    let mut i = 0;
    while i < result.len() {
        result[i] = OnuResources {
            onu_id: i as u8,
            alloc_id: 300 + i as u16,
            gem_id: 500 + i as u16,
            interface: None,
        };
        i += 1;
    }
    result
}
