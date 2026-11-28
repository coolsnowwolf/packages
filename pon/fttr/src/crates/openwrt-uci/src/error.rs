// Copyright 2026 Lazuli Kao / openwrt-fttr contributors
// SPDX-License-Identifier: Apache-2.0

use thiserror::Error;

#[derive(Error, Debug)]
pub enum UciError {
    #[error("Failed to load libuci dynamic library: {0}")]
    LoadLibrary(String),
    #[error("Failed to allocate UCI context")]
    AllocContext,
    #[error("Invalid parameter: {0}")]
    InvalidParameter(String),
    #[error("UCI package not found: {0}")]
    PackageNotFound(String),
    #[error("UCI section not found: {0}")]
    SectionNotFound(String),
    #[error("UCI option not found: {0}")]
    OptionNotFound(String),
    #[error("UCI parse error: {0}")]
    ParseError(String),
    #[error("UCI I/O error: {0}")]
    IoError(String),
    #[error("UCI backend error: {0}")]
    Backend(String),
    #[error("String contains null byte: {0}")]
    NullByte(#[from] std::ffi::NulError),
}
