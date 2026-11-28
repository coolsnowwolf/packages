// Copyright 2026 Lazuli Kao / openwrt-fttr contributors
// SPDX-License-Identifier: Apache-2.0

use thiserror::Error;

#[derive(Error, Debug)]
pub enum UbusError {
    #[error("Failed to load ubus dynamic library: {0}")]
    LoadLibrary(String),
    #[error("Failed to connect to ubus daemon: {0}")]
    ConnectionFailed(String),
    #[error("Ubus object not found: {0}")]
    ObjectNotFound(String),
    #[error("Ubus method not found: {0}")]
    MethodNotFound(String),
    #[error("Ubus call invocation failed: {0}")]
    InvokeFailed(String),
    #[error("Invalid ubus response data: {0}")]
    InvalidResponse(String),
    #[error("Ubus request timed out")]
    Timeout,
    #[error("Serialization error: {0}")]
    Serialization(String),
    #[error("Ubus backend error: {0}")]
    Backend(String),
    #[error("String contains null byte: {0}")]
    NullByte(#[from] std::ffi::NulError),
}
