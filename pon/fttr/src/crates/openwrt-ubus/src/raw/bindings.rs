// Copyright 2026 Lazuli Kao / openwrt-fttr contributors
// SPDX-License-Identifier: Apache-2.0
//
// Pinned OpenWrt libubus and libubox C ABI definitions.
// Sourced from OpenWrt ubus.git commit da2d205a5cf3e877c45a2eec488218cbaf452e7d.

#![allow(non_camel_case_types)]
#![allow(non_snake_case)]
#![allow(dead_code)]

use std::os::raw::{c_char, c_int, c_uint, c_ulong, c_void};

pub const UBUS_STATUS_OK: c_int = 0;
pub const UBUS_STATUS_INVALID_COMMAND: c_int = 1;
pub const UBUS_STATUS_INVALID_ARGUMENT: c_int = 2;
pub const UBUS_STATUS_METHOD_NOT_FOUND: c_int = 3;
pub const UBUS_STATUS_NOT_FOUND: c_int = 4;
pub const UBUS_STATUS_NO_DATA: c_int = 5;
pub const UBUS_STATUS_PERMISSION_DENIED: c_int = 6;
pub const UBUS_STATUS_TIMEOUT: c_int = 7;
pub const UBUS_STATUS_NOT_SUPPORTED: c_int = 8;
pub const UBUS_STATUS_UNKNOWN_ERROR: c_int = 9;
pub const UBUS_STATUS_CONNECTION_FAILED: c_int = 10;

pub const BLOBMSG_TYPE_UNSPEC: c_int = 0;
pub const BLOBMSG_TYPE_ARRAY: c_int = 1;
pub const BLOBMSG_TYPE_TABLE: c_int = 2;
pub const BLOBMSG_TYPE_STRING: c_int = 3;
pub const BLOBMSG_TYPE_INT64: c_int = 4;
pub const BLOBMSG_TYPE_INT32: c_int = 5;
pub const BLOBMSG_TYPE_INT16: c_int = 6;
pub const BLOBMSG_TYPE_INT8: c_int = 7;
pub const BLOBMSG_TYPE_DOUBLE: c_int = 8;

#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct list_head {
    pub next: *mut list_head,
    pub prev: *mut list_head,
}

#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct avl_node {
    pub list: list_head,
    pub parent: *mut avl_node,
    pub left: *mut avl_node,
    pub right: *mut avl_node,
    pub key: *const c_void,
    pub balance: i8,
    pub leader: bool,
}

pub type avl_tree_comp =
    Option<unsafe extern "C" fn(k1: *const c_void, k2: *const c_void, ptr: *mut c_void) -> c_int>;

#[repr(C)]
#[derive(Copy, Clone)]
pub struct avl_tree {
    pub list_head: list_head,
    pub root: *mut avl_node,
    pub count: c_uint,
    pub allow_dups: bool,
    pub comp: avl_tree_comp,
    pub cmp_ptr: *mut c_void,
}

pub type uloop_fd_handler = Option<unsafe extern "C" fn(u: *mut uloop_fd, events: c_uint)>;

pub const ULOOP_READ: u8 = 1 << 0;
pub const ULOOP_WRITE: u8 = 1 << 1;
pub const ULOOP_EDGE_TRIGGER: u8 = 1 << 2;
pub const ULOOP_BLOCKING: u8 = 1 << 3;

#[repr(C)]
#[derive(Copy, Clone)]
pub struct uloop_fd {
    pub cb: uloop_fd_handler,
    pub fd: c_int,
    pub eof: bool,
    pub error: bool,
    pub registered: bool,
    pub flags: u8,
}

#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct blob_attr {
    pub id_len: u32,
}

#[repr(C)]
pub struct blob_buf {
    pub head: *mut blob_attr,
    pub grow: Option<unsafe extern "C" fn(buf: *mut blob_buf, minlen: c_int) -> bool>,
    pub buflen: c_int,
    pub buf: *mut c_void,
}

#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct blobmsg_policy {
    pub name: *const c_char,
    pub type_: c_int,
}

#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct ubus_acl_key {
    pub user: *const c_char,
    pub group: *const c_char,
    pub object: *const c_char,
}

#[repr(C)]
pub struct ubus_request_data {
    pub object: u32,
    pub peer: u32,
    pub seq: u16,
    pub acl: ubus_acl_key,
    pub deferred: bool,
    pub fd: c_int,
    pub req_fd: c_int,
}

pub type ubus_connection_lost_cb = Option<unsafe extern "C" fn(ctx: *mut ubus_context)>;

#[repr(C)]
pub struct uloop_timeout {
    pub list: list_head,
    pub pending: bool,
    pub _pad: [u8; 7],
    pub cb: Option<unsafe extern "C" fn(*mut uloop_timeout)>,
    pub time: [u8; 16],
}

#[repr(C)]
pub struct ubus_context {
    pub requests: list_head,
    pub objects: avl_tree,
    pub pending: list_head,
    pub sock: uloop_fd,
    pub pending_timer: uloop_timeout,
    pub local_id: u32,
    pub request_seq: u16,
    pub cancel_poll: bool,
    pub _pad1: u8,
    pub stack_depth: c_int,
    pub _pad2: [u8; 4],
    pub connection_lost: ubus_connection_lost_cb,
    pub monitor_cb: *mut c_void,
    pub msgbuf_hdr: *mut c_void,
    pub msgbuf_data: *mut c_void,
    pub msgbuf_data_len: u32,
}

#[repr(C)]
pub struct ubus_request {
    pub list: list_head,
    pub pending: list_head,
    pub status_code: c_int,
    pub status_msg: bool,
    pub blocked: bool,
    pub cancelled: bool,
    pub notify: bool,
    pub peer: u32,
    pub seq: u16,
    pub raw_data_cb: Option<ubus_data_handler_t>,
    pub data_cb: Option<ubus_data_handler_t>,
    pub fd_cb: *mut c_void,
    pub complete_cb: *mut c_void,
    pub fd: c_int,
    pub ctx: *mut ubus_context,
    pub priv_: *mut c_void,
}

pub type ubus_handler_t = unsafe extern "C" fn(
    ctx: *mut ubus_context,
    obj: *mut ubus_object,
    req: *mut ubus_request_data,
    method: *const c_char,
    msg: *mut blob_attr,
) -> c_int;

pub type ubus_state_handler_t = unsafe extern "C" fn(ctx: *mut ubus_context, obj: *mut ubus_object);

pub type ubus_data_handler_t =
    unsafe extern "C" fn(req: *mut ubus_request, type_: c_int, msg: *mut blob_attr);

#[repr(C)]
#[derive(Copy, Clone)]
pub struct ubus_method {
    pub name: *const c_char,
    pub handler: Option<ubus_handler_t>,
    pub mask: c_ulong,
    pub tags: c_ulong,
    pub policy: *const blobmsg_policy,
    pub n_policy: c_int,
}

#[repr(C)]
pub struct ubus_object_type {
    pub name: *const c_char,
    pub id: u32,
    pub methods: *const ubus_method,
    pub n_methods: c_int,
}

#[repr(C)]
pub struct ubus_object {
    pub avl: avl_node,
    pub name: *const c_char,
    pub id: u32,
    pub path: *const c_char,
    pub type_: *mut ubus_object_type,
    pub subscribe_cb: Option<ubus_state_handler_t>,
    pub has_subscribers: bool,
    pub methods: *const ubus_method,
    pub n_methods: c_int,
}
