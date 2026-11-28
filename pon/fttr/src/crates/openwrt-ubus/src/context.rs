// Copyright 2026 Lazuli Kao / openwrt-fttr contributors
// SPDX-License-Identifier: Apache-2.0

use std::ffi::{c_void, CString};
use std::os::raw::c_int;

use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::Value;

use crate::blobmsg::BlobBuffer;
use crate::error::UbusError;
use crate::raw::*;

pub struct Ubus {
    lib: LibUbus,
    ctx: *mut ubus_context,
}

unsafe impl Send for Ubus {}

struct InvokeState {
    lib: LibUbus,
    response: Option<Value>,
    error: Option<String>,
}

unsafe extern "C" fn invoke_callback(req: *mut ubus_request, _type: c_int, msg: *mut blob_attr) {
    if req.is_null() || msg.is_null() {
        return;
    }
    let priv_ = (*req).priv_;
    if priv_.is_null() {
        return;
    }
    let state = &mut *(priv_ as *mut InvokeState);
    match state.lib.blobmsg_format_json(msg, true) {
        Some(json_str) => match serde_json::from_str::<Value>(&json_str) {
            Ok(parsed) => {
                state.response = Some(parsed);
            }
            Err(e) => {
                state.error = Some(format!("failed to parse ubus JSON response: {e}"));
            }
        },
        None => {
            state.error = Some("blobmsg_format_json failed to format response".into());
        }
    }
}

impl Ubus {
    pub fn connect(socket_path: Option<&str>) -> Result<Self, UbusError> {
        let lib = LibUbus::load().map_err(UbusError::LoadLibrary)?;
        Self::connect_with_lib(lib, socket_path)
    }

    pub fn connect_with_lib(lib: LibUbus, socket_path: Option<&str>) -> Result<Self, UbusError> {
        let _lock = crate::raw::LIBUBUS_LOCK.lock().unwrap();
        let c_path = match socket_path {
            Some(p) => Some(CString::new(p)?),
            None => None,
        };
        let path_ptr = c_path
            .as_ref()
            .map(|c| c.as_ptr())
            .unwrap_or(std::ptr::null());

        unsafe {
            let ctx = lib.ubus_connect(path_ptr);
            if ctx.is_null() {
                return Err(UbusError::ConnectionFailed(
                    socket_path
                        .unwrap_or("default /var/run/ubus/ubus.sock")
                        .to_string(),
                ));
            }
            Ok(Self { lib, ctx })
        }
    }

    pub fn call(
        &mut self,
        object: &str,
        method: &str,
        msg: Option<&Value>,
        timeout_ms: i32,
    ) -> Result<Value, UbusError> {
        let _lock = crate::raw::LIBUBUS_LOCK.lock().unwrap();
        let c_obj = CString::new(object)?;
        let c_method = CString::new(method)?;

        let mut id = 0u32;
        unsafe {
            let ret = self.lib.ubus_lookup_id(self.ctx, c_obj.as_ptr(), &mut id);
            if ret != UBUS_STATUS_OK {
                return Err(UbusError::ObjectNotFound(format!(
                    "{} (error: {})",
                    object,
                    self.lib.strerror(ret)
                )));
            }

            let mut blob = match msg {
                Some(v) => {
                    let json_str = serde_json::to_string(v)
                        .map_err(|e| UbusError::Serialization(e.to_string()))?;
                    Some(BlobBuffer::from_json_str(self.lib.clone(), &json_str)?)
                }
                None => None,
            };

            let blob_head = blob
                .as_mut()
                .map(|b| b.head())
                .unwrap_or(std::ptr::null_mut());

            let mut state = InvokeState {
                lib: self.lib.clone(),
                response: None,
                error: None,
            };

            let ret = self.lib.ubus_invoke(
                self.ctx,
                id,
                c_method.as_ptr(),
                blob_head,
                Some(invoke_callback),
                &mut state as *mut _ as *mut c_void,
                timeout_ms,
            );

            if ret != UBUS_STATUS_OK {
                if ret == UBUS_STATUS_TIMEOUT {
                    return Err(UbusError::Timeout);
                } else if ret == UBUS_STATUS_NOT_FOUND {
                    return Err(UbusError::ObjectNotFound(object.to_string()));
                } else if ret == UBUS_STATUS_METHOD_NOT_FOUND {
                    return Err(UbusError::MethodNotFound(method.to_string()));
                } else {
                    return Err(UbusError::InvokeFailed(format!(
                        "Call {}.{} failed with: {}",
                        object,
                        method,
                        self.lib.strerror(ret)
                    )));
                }
            }

            if let Some(err) = state.error {
                return Err(UbusError::InvalidResponse(err));
            }

            Ok(state.response.unwrap_or(Value::Null))
        }
    }

    /// Type-safe typed RPC invocation: converts request to json and parses response directly into R
    pub fn call_typed<T, R>(
        &mut self,
        object: &str,
        method: &str,
        params: &T,
        timeout_ms: i32,
    ) -> Result<R, UbusError>
    where
        T: Serialize,
        R: DeserializeOwned,
    {
        let json_val =
            serde_json::to_value(params).map_err(|e| UbusError::Serialization(e.to_string()))?;
        let resp_val = self.call(object, method, Some(&json_val), timeout_ms)?;
        serde_json::from_value(resp_val).map_err(|e| UbusError::Serialization(e.to_string()))
    }

    /// Parameterless typed RPC invocation: directly decodes result into struct R
    pub fn call_empty<R>(
        &mut self,
        object: &str,
        method: &str,
        timeout_ms: i32,
    ) -> Result<R, UbusError>
    where
        R: DeserializeOwned,
    {
        let resp_val = self.call(object, method, None, timeout_ms)?;
        serde_json::from_value(resp_val).map_err(|e| UbusError::Serialization(e.to_string()))
    }
}

impl Drop for Ubus {
    fn drop(&mut self) {
        if !self.ctx.is_null() {
            let _lock = crate::raw::LIBUBUS_LOCK.lock().unwrap();
            unsafe {
                self.lib.ubus_free(self.ctx);
            }
        }
    }
}
