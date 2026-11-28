// Copyright 2026 Lazuli Kao / openwrt-fttr contributors
// SPDX-License-Identifier: Apache-2.0

use openwrt_ubus::Ubus;
use serde_json::{json, Value};

pub struct ServiceClient<'a> {
    ubus: &'a mut Ubus,
}

impl<'a> ServiceClient<'a> {
    pub fn new(ubus: &'a mut Ubus) -> Self {
        Self { ubus }
    }

    pub fn list(&mut self, name: &str) -> Result<Value, String> {
        let query = json!({ "name": name });
        self.ubus
            .call_typed("service", "list", &query, 3000)
            .map_err(|e| format!("service list failed: {e}"))
    }
}
