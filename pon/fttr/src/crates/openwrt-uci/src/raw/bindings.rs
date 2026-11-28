// Copyright 2026 Lazuli Kao / openwrt-fttr contributors
// SPDX-License-Identifier: Apache-2.0
//
// Pinned OpenWrt libuci C ABI definitions.
// Sourced from OpenWrt uci.git commit 74f6277aabffc943d026f406df57c22595134c42.

#![allow(non_camel_case_types)]
#![allow(non_snake_case)]
#![allow(dead_code)]

use std::os::raw::{c_char, c_int, c_void};

pub const UCI_OK: c_int = 0;
pub const UCI_ERR_MEM: c_int = 1;
pub const UCI_ERR_INVAL: c_int = 2;
pub const UCI_ERR_NOTFOUND: c_int = 3;
pub const UCI_ERR_IO: c_int = 4;
pub const UCI_ERR_PARSE: c_int = 5;
pub const UCI_ERR_DUPLICATE: c_int = 6;
pub const UCI_ERR_UNKNOWN: c_int = 7;

pub const UCI_LOOKUP_DONE: u32 = 1 << 0;
pub const UCI_LOOKUP_COMPLETE: u32 = 1 << 1;
pub const UCI_LOOKUP_EXTENDED: u32 = 1 << 2;

#[repr(C)]
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum uci_type {
    UCI_TYPE_UNSPEC = 0,
    UCI_TYPE_DELTA = 1,
    UCI_TYPE_PACKAGE = 2,
    UCI_TYPE_SECTION = 3,
    UCI_TYPE_OPTION = 4,
    UCI_TYPE_PATH = 5,
    UCI_TYPE_BACKEND = 6,
    UCI_TYPE_ITEM = 7,
    UCI_TYPE_HOOK = 8,
}

#[repr(C)]
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum uci_option_type {
    UCI_TYPE_STRING = 0,
    UCI_TYPE_LIST = 1,
}

#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct uci_list {
    pub next: *mut uci_list,
    pub prev: *mut uci_list,
}

#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct uci_element {
    pub list: uci_list,
    pub type_: uci_type,
    pub name: *mut c_char,
}

#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct uci_ptr {
    pub target: uci_type,
    pub flags: u32,
    pub p: *mut uci_package,
    pub s: *mut uci_section,
    pub o: *mut uci_option,
    pub last: *mut uci_element,
    pub package: *const c_char,
    pub section: *const c_char,
    pub option: *const c_char,
    pub value: *const c_char,
}

impl Default for uci_ptr {
    fn default() -> Self {
        Self {
            target: uci_type::UCI_TYPE_UNSPEC,
            flags: 0,
            p: std::ptr::null_mut(),
            s: std::ptr::null_mut(),
            o: std::ptr::null_mut(),
            last: std::ptr::null_mut(),
            package: std::ptr::null(),
            section: std::ptr::null(),
            option: std::ptr::null(),
            value: std::ptr::null(),
        }
    }
}

#[repr(C)]
pub struct uci_context {
    pub root: uci_list,
    pub pctx: *mut c_void,
    pub backend: *mut c_void,
    pub backends: uci_list,
    pub flags: c_int,
    pub confdir: *mut c_char,
    pub savedir: *mut c_char,
    pub searchpath: *const c_char,
    pub err: c_int,
    pub func: *const c_char,
}

#[repr(C)]
pub struct uci_package {
    pub e: uci_element,
    pub sections: uci_list,
    pub ctx: *mut uci_context,
    pub has_delta: bool,
    pub uses_conf2: bool,
    pub path: *mut c_char,
}

#[repr(C)]
pub struct uci_section {
    pub e: uci_element,
    pub options: uci_list,
    pub package: *mut uci_package,
    pub anonymous: bool,
    pub type_: *mut c_char,
}

#[repr(C)]
#[derive(Copy, Clone)]
pub union uci_option_value {
    pub list: uci_list,
    pub string: *mut c_char,
}

#[repr(C)]
pub struct uci_option {
    pub e: uci_element,
    pub section: *mut uci_section,
    pub type_: uci_option_type,
    pub v: uci_option_value,
}
