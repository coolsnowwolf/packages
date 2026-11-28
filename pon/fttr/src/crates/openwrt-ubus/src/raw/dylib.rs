// Copyright 2026 Lazuli Kao / openwrt-fttr contributors
// SPDX-License-Identifier: Apache-2.0

use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_int, c_uint, c_void};
use std::sync::{Arc, OnceLock};

use super::bindings::*;

#[cfg(unix)]
mod dlfcn {
    use std::os::raw::{c_char, c_int, c_void};
    pub const RTLD_LAZY: c_int = 1;
    pub const RTLD_LOCAL: c_int = 0;
    extern "C" {
        pub fn dlopen(filename: *const c_char, flags: c_int) -> *mut c_void;
        pub fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
        pub fn dlclose(handle: *mut c_void) -> c_int;
        pub fn dlerror() -> *mut c_char;
    }
}

#[cfg(not(unix))]
mod dlfcn {
    use std::os::raw::{c_char, c_int, c_void};
    pub const RTLD_LAZY: c_int = 1;
    pub const RTLD_LOCAL: c_int = 0;
    pub unsafe fn dlopen(_filename: *const c_char, _flags: c_int) -> *mut c_void {
        std::ptr::null_mut()
    }
    pub unsafe fn dlsym(_handle: *mut c_void, _symbol: *const c_char) -> *mut c_void {
        std::ptr::null_mut()
    }
    pub unsafe fn dlclose(_handle: *mut c_void) -> c_int {
        0
    }
    pub unsafe fn dlerror() -> *mut c_char {
        std::ptr::null_mut()
    }
}

use dlfcn::*;

type FnUbusConnect = unsafe extern "C" fn(path: *const c_char) -> *mut ubus_context;
type FnUbusFree = unsafe extern "C" fn(ctx: *mut ubus_context);
type FnUbusReconnect = unsafe extern "C" fn(ctx: *mut ubus_context, path: *const c_char) -> c_int;
type FnUbusAddObject = unsafe extern "C" fn(ctx: *mut ubus_context, obj: *mut ubus_object) -> c_int;
type FnUbusRemoveObject =
    unsafe extern "C" fn(ctx: *mut ubus_context, obj: *mut ubus_object) -> c_int;
type FnUbusLookupId =
    unsafe extern "C" fn(ctx: *mut ubus_context, path: *const c_char, id: *mut u32) -> c_int;
type FnUbusInvokeFd = unsafe extern "C" fn(
    ctx: *mut ubus_context,
    obj: u32,
    method: *const c_char,
    msg: *mut blob_attr,
    cb: Option<ubus_data_handler_t>,
    priv_: *mut c_void,
    timeout: c_int,
    fd: c_int,
) -> c_int;
type FnUbusSendReply = unsafe extern "C" fn(
    ctx: *mut ubus_context,
    req: *mut ubus_request_data,
    msg: *mut blob_attr,
) -> c_int;
type FnUbusSendEvent =
    unsafe extern "C" fn(ctx: *mut ubus_context, id: *const c_char, data: *mut blob_attr) -> c_int;
type FnUbusStrerror = unsafe extern "C" fn(error: c_int) -> *const c_char;

type FnBlobBufInit = unsafe extern "C" fn(buf: *mut blob_buf, id: c_int) -> c_int;
type FnBlobBufFree = unsafe extern "C" fn(buf: *mut blob_buf);
type FnBlobmsgFormatJsonWithCb = unsafe extern "C" fn(
    attr: *mut blob_attr,
    list: bool,
    cb: *mut c_void,
    priv_: *mut c_void,
    indent: c_int,
    custom_format: bool,
) -> *mut c_char;
type FnBlobmsgAddJsonFromString =
    unsafe extern "C" fn(b: *mut blob_buf, str_: *const c_char) -> bool;

type FnUloopInit = unsafe extern "C" fn() -> c_int;
type FnUloopRunTimeout = unsafe extern "C" fn(timeout: c_int) -> c_int;
type FnUloopDone = unsafe extern "C" fn();
type FnUloopFdAdd = unsafe extern "C" fn(fd: *mut uloop_fd, flags: c_uint) -> c_int;
type FnUloopFdDelete = unsafe extern "C" fn(fd: *mut uloop_fd) -> c_int;

#[derive(Clone)]
pub struct LibUbus {
    inner: Arc<LibUbusInner>,
}

struct LibUbusInner {
    handle_ubus: *mut c_void,
    handle_ubox: *mut c_void,
    handle_json: *mut c_void,

    pub ubus_connect: FnUbusConnect,
    pub ubus_free: FnUbusFree,
    pub ubus_reconnect: FnUbusReconnect,
    pub ubus_add_object: FnUbusAddObject,
    pub ubus_remove_object: FnUbusRemoveObject,
    pub ubus_lookup_id: FnUbusLookupId,
    pub ubus_invoke_fd: FnUbusInvokeFd,
    pub ubus_send_reply: FnUbusSendReply,
    pub ubus_send_event: FnUbusSendEvent,
    pub ubus_strerror: Option<FnUbusStrerror>,

    pub blob_buf_init: FnBlobBufInit,
    pub blob_buf_free: FnBlobBufFree,
    pub blobmsg_format_json_with_cb: Option<FnBlobmsgFormatJsonWithCb>,
    pub blobmsg_add_json_from_string: Option<FnBlobmsgAddJsonFromString>,

    pub uloop_init: FnUloopInit,
    pub uloop_run_timeout: FnUloopRunTimeout,
    pub uloop_done: FnUloopDone,
    pub uloop_fd_add: FnUloopFdAdd,
    pub uloop_fd_delete: FnUloopFdDelete,
    pub uloop_cancelled: *mut bool,
}

unsafe impl Send for LibUbusInner {}
unsafe impl Sync for LibUbusInner {}

impl Drop for LibUbusInner {
    fn drop(&mut self) {
        unsafe {
            if !self.handle_json.is_null() {
                dlclose(self.handle_json);
            }
            if !self.handle_ubox.is_null() {
                dlclose(self.handle_ubox);
            }
            if !self.handle_ubus.is_null() {
                dlclose(self.handle_ubus);
            }
        }
    }
}

fn discover_candidate_libraries(prefix: &str) -> Vec<String> {
    let mut candidates = Vec::new();
    let search_dirs = ["/lib", "/usr/lib"];

    for dir in search_dirs {
        if let Ok(entries) = std::fs::read_dir(dir) {
            let mut matches = Vec::new();
            for entry in entries.flatten() {
                if let Ok(name) = entry.file_name().into_string() {
                    if name.starts_with(prefix) {
                        matches.push(format!("{dir}/{name}"));
                    }
                }
            }
            matches.sort();
            matches.reverse();
            candidates.extend(matches);
        }
    }

    candidates.push(format!("/lib/{prefix}"));
    candidates.push(format!("/usr/lib/{prefix}"));
    candidates.push(prefix.to_string());
    candidates
}

unsafe fn load_first_working(prefix: &str, env_var: &str) -> Result<(*mut c_void, String), String> {
    if let Ok(path) = std::env::var(env_var) {
        let c_path = CString::new(path.as_bytes()).map_err(|e| e.to_string())?;
        let handle = dlopen(c_path.as_ptr(), RTLD_LAZY | RTLD_LOCAL);
        if !handle.is_null() {
            return Ok((handle, path));
        }
    }

    let candidates = discover_candidate_libraries(prefix);
    let mut errs = Vec::new();
    for p in &candidates {
        let c_path = CString::new(p.as_bytes()).unwrap();
        let handle = dlopen(c_path.as_ptr(), RTLD_LAZY | RTLD_LOCAL);
        if !handle.is_null() {
            return Ok((handle, p.clone()));
        }
        let err = dlerror();
        let msg = if !err.is_null() {
            CStr::from_ptr(err).to_string_lossy().into_owned()
        } else {
            "load failed".to_string()
        };
        errs.push(format!("{p}: {msg}"));
    }
    Err(errs.join("\n"))
}

struct DlHandle(*mut c_void);

impl DlHandle {
    fn new(ptr: *mut c_void) -> Self {
        Self(ptr)
    }

    fn raw(&self) -> *mut c_void {
        self.0
    }

    fn into_raw(mut self) -> *mut c_void {
        let p = self.0;
        self.0 = std::ptr::null_mut();
        p
    }
}

impl Drop for DlHandle {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                dlclose(self.0);
            }
        }
    }
}

impl LibUbus {
    pub fn load() -> Result<Self, String> {
        // Keep the libraries and their process-global uloop policy alive for the
        // process lifetime. Concurrent clients must never rewrite that policy
        // while another thread is already running uloop. Paths (and failures)
        // are consequently fixed by the first load attempt.
        static LIBRARY: OnceLock<Result<LibUbus, String>> = OnceLock::new();
        LIBRARY.get_or_init(Self::load_once).clone()
    }

    fn load_once() -> Result<Self, String> {
        unsafe {
            let (h_ubox, _ubox_path) = load_first_working("libubox.so", "LIBUBOX_SO_PATH")
                .map_err(|e| format!("Failed to load libubox:\n{e}"))?;
            let handle_ubox = DlHandle::new(h_ubox);

            let (h_ubus, _ubus_path) = load_first_working("libubus.so", "LIBUBUS_SO_PATH")
                .map_err(|e| format!("Failed to load libubus:\n{e}"))?;
            let handle_ubus = DlHandle::new(h_ubus);

            let h_json = load_first_working("libblobmsg_json.so", "LIBBLOBMSG_JSON_SO_PATH")
                .map(|(h, _)| h)
                .unwrap_or(std::ptr::null_mut());
            let handle_json = DlHandle::new(h_json);

            macro_rules! resolve {
                ($handle:expr, $sym:ident, $type:ident) => {{
                    let c_sym = CString::new(stringify!($sym)).unwrap();
                    let ptr = dlsym($handle, c_sym.as_ptr());
                    if ptr.is_null() {
                        return Err(format!("Symbol '{}' missing in library", stringify!($sym)));
                    }
                    std::mem::transmute::<*mut c_void, $type>(ptr)
                }};
            }

            macro_rules! resolve_opt {
                ($handle:expr, $sym:ident, $type:ident) => {{
                    if $handle.is_null() {
                        None
                    } else {
                        let c_sym = CString::new(stringify!($sym)).unwrap();
                        let ptr = dlsym($handle, c_sym.as_ptr());
                        if ptr.is_null() {
                            None
                        } else {
                            Some(std::mem::transmute::<*mut c_void, $type>(ptr))
                        }
                    }
                }};
            }

            let handle_sigchld = {
                let c_sym = CString::new("uloop_handle_sigchld").unwrap();
                let ptr = dlsym(handle_ubox.raw(), c_sym.as_ptr()) as *mut bool;
                if ptr.is_null() {
                    return Err("Symbol 'uloop_handle_sigchld' missing in library".into());
                }
                ptr
            };

            let inner = LibUbusInner {
                ubus_connect: resolve!(handle_ubus.raw(), ubus_connect, FnUbusConnect),
                ubus_free: resolve!(handle_ubus.raw(), ubus_free, FnUbusFree),
                ubus_reconnect: resolve!(handle_ubus.raw(), ubus_reconnect, FnUbusReconnect),
                ubus_add_object: resolve!(handle_ubus.raw(), ubus_add_object, FnUbusAddObject),
                ubus_remove_object: resolve!(
                    handle_ubus.raw(),
                    ubus_remove_object,
                    FnUbusRemoveObject
                ),
                ubus_lookup_id: resolve!(handle_ubus.raw(), ubus_lookup_id, FnUbusLookupId),
                ubus_invoke_fd: resolve!(handle_ubus.raw(), ubus_invoke_fd, FnUbusInvokeFd),
                ubus_send_reply: resolve!(handle_ubus.raw(), ubus_send_reply, FnUbusSendReply),
                ubus_send_event: resolve!(handle_ubus.raw(), ubus_send_event, FnUbusSendEvent),
                ubus_strerror: resolve_opt!(handle_ubus.raw(), ubus_strerror, FnUbusStrerror),

                blob_buf_init: resolve!(handle_ubox.raw(), blob_buf_init, FnBlobBufInit),
                blob_buf_free: resolve!(handle_ubox.raw(), blob_buf_free, FnBlobBufFree),
                blobmsg_format_json_with_cb: resolve_opt!(
                    handle_json.raw(),
                    blobmsg_format_json_with_cb,
                    FnBlobmsgFormatJsonWithCb
                ),
                blobmsg_add_json_from_string: resolve_opt!(
                    handle_json.raw(),
                    blobmsg_add_json_from_string,
                    FnBlobmsgAddJsonFromString
                ),

                uloop_init: resolve!(handle_ubox.raw(), uloop_init, FnUloopInit),
                uloop_run_timeout: resolve!(
                    handle_ubox.raw(),
                    uloop_run_timeout,
                    FnUloopRunTimeout
                ),
                uloop_done: resolve!(handle_ubox.raw(), uloop_done, FnUloopDone),
                uloop_fd_add: resolve!(handle_ubox.raw(), uloop_fd_add, FnUloopFdAdd),
                uloop_fd_delete: resolve!(handle_ubox.raw(), uloop_fd_delete, FnUloopFdDelete),
                uloop_cancelled: {
                    let c_sym = CString::new("uloop_cancelled").unwrap();
                    dlsym(handle_ubox.raw(), c_sym.as_ptr()) as *mut bool
                },

                handle_ubus: handle_ubus.into_raw(),
                handle_ubox: handle_ubox.into_raw(),
                handle_json: handle_json.into_raw(),
            };

            // Rust Child owns all subprocess exit statuses in this process.
            // uloop's default SIGCHLD handler calls waitpid(-1), including for
            // children it does not track. Disable it before any caller can
            // initialize/run uloop; do not mask ECHILD in command executors.
            *handle_sigchld = false;

            Ok(LibUbus {
                inner: Arc::new(inner),
            })
        }
    }

    #[inline]
    pub unsafe fn ubus_connect(&self, path: *const c_char) -> *mut ubus_context {
        (self.inner.ubus_connect)(path)
    }

    #[inline]
    pub unsafe fn ubus_free(&self, ctx: *mut ubus_context) {
        (self.inner.ubus_free)(ctx)
    }

    #[inline]
    pub unsafe fn ubus_reconnect(&self, ctx: *mut ubus_context, path: *const c_char) -> c_int {
        (self.inner.ubus_reconnect)(ctx, path)
    }

    #[inline]
    pub unsafe fn ubus_add_object(&self, ctx: *mut ubus_context, obj: *mut ubus_object) -> c_int {
        (self.inner.ubus_add_object)(ctx, obj)
    }

    #[inline]
    pub unsafe fn ubus_remove_object(
        &self,
        ctx: *mut ubus_context,
        obj: *mut ubus_object,
    ) -> c_int {
        (self.inner.ubus_remove_object)(ctx, obj)
    }

    #[inline]
    pub unsafe fn ubus_lookup_id(
        &self,
        ctx: *mut ubus_context,
        path: *const c_char,
        id: *mut u32,
    ) -> c_int {
        (self.inner.ubus_lookup_id)(ctx, path, id)
    }

    #[inline]
    pub unsafe fn ubus_invoke(
        &self,
        ctx: *mut ubus_context,
        obj: u32,
        method: *const c_char,
        msg: *mut blob_attr,
        cb: Option<ubus_data_handler_t>,
        priv_: *mut c_void,
        timeout: c_int,
    ) -> c_int {
        (self.inner.ubus_invoke_fd)(ctx, obj, method, msg, cb, priv_, timeout, -1)
    }

    #[inline]
    pub unsafe fn ubus_send_reply(
        &self,
        ctx: *mut ubus_context,
        req: *mut ubus_request_data,
        msg: *mut blob_attr,
    ) -> c_int {
        (self.inner.ubus_send_reply)(ctx, req, msg)
    }

    #[inline]
    pub unsafe fn ubus_send_event(
        &self,
        ctx: *mut ubus_context,
        id: *const c_char,
        data: *mut blob_attr,
    ) -> c_int {
        (self.inner.ubus_send_event)(ctx, id, data)
    }

    #[inline]
    pub unsafe fn blob_buf_init(&self, buf: *mut blob_buf, id: c_int) -> c_int {
        (self.inner.blob_buf_init)(buf, id)
    }

    #[inline]
    pub unsafe fn blob_buf_free(&self, buf: *mut blob_buf) {
        (self.inner.blob_buf_free)(buf)
    }

    #[inline]
    pub unsafe fn blobmsg_format_json(&self, attr: *mut blob_attr, list: bool) -> Option<String> {
        if let Some(fmt_fn) = self.inner.blobmsg_format_json_with_cb {
            let res = fmt_fn(
                attr,
                list,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                -1,
                false,
            );
            if !res.is_null() {
                let s = CStr::from_ptr(res).to_string_lossy().into_owned();
                libc::free(res as *mut c_void);
                return Some(s);
            }
        }
        None
    }

    #[inline]
    pub unsafe fn blobmsg_add_json_from_string(
        &self,
        b: *mut blob_buf,
        str_: *const c_char,
    ) -> bool {
        if let Some(add_fn) = self.inner.blobmsg_add_json_from_string {
            add_fn(b, str_)
        } else {
            false
        }
    }

    pub unsafe fn strerror(&self, code: c_int) -> String {
        if let Some(err_fn) = self.inner.ubus_strerror {
            let res = err_fn(code);
            if !res.is_null() {
                return CStr::from_ptr(res).to_string_lossy().into_owned();
            }
        }
        format!("ubus error {code}")
    }

    #[inline]
    pub unsafe fn uloop_init(&self) -> c_int {
        (self.inner.uloop_init)()
    }

    #[inline]
    pub unsafe fn uloop_run(&self) -> c_int {
        (self.inner.uloop_run_timeout)(-1)
    }

    #[inline]
    pub unsafe fn uloop_end(&self) {
        if !self.inner.uloop_cancelled.is_null() {
            *self.inner.uloop_cancelled = true;
        }
    }

    #[inline]
    pub fn uloop_cancelled_ptr(&self) -> *mut bool {
        self.inner.uloop_cancelled
    }

    #[inline]
    pub unsafe fn uloop_done(&self) {
        (self.inner.uloop_done)()
    }

    #[inline]
    pub unsafe fn uloop_fd_add(&self, fd: *mut uloop_fd, flags: c_uint) -> c_int {
        (self.inner.uloop_fd_add)(fd, flags)
    }

    #[inline]
    pub unsafe fn uloop_fd_delete(&self, fd: *mut uloop_fd) -> c_int {
        (self.inner.uloop_fd_delete)(fd)
    }

    #[inline]
    pub unsafe fn ubus_add_uloop(&self, ctx: *mut ubus_context) -> c_int {
        let flags = (ULOOP_BLOCKING | ULOOP_READ) as c_uint;
        self.uloop_fd_add(&mut (*ctx).sock, flags)
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::process::{Command, Stdio};

    // uloop owns process-global signal handlers. Run only this test in a fresh
    // OpenWrt test process: --ignored --exact <test name> --test-threads=1.
    #[test]
    #[ignore = "requires OpenWrt shared libraries and an isolated test process"]
    fn uloop_preserves_rust_child_exit_status_and_output() {
        let lib = LibUbus::load().expect("load OpenWrt libraries");
        struct LoopGuard(LibUbus);
        impl Drop for LoopGuard {
            fn drop(&mut self) {
                unsafe { self.0.uloop_done() };
            }
        }

        for code in [0, 7] {
            assert_eq!(unsafe { lib.uloop_init() }, 0);
            let _guard = LoopGuard(lib.clone());
            // Loading another client while uloop is active must not rewrite its
            // global signal policy, nor unload its shared-library state.
            let _client = LibUbus::load().expect("load another client");
            let mut child = Command::new("/bin/sh")
                .args(["-c", &format!("printf rust-child-output; exit {code}")])
                .stdout(Stdio::piped())
                .spawn()
                .expect("spawn harmless child");
            // A bounded run performs only one event-loop iteration. SIGCHLD
            // delivered during polling is reaped on the following iteration.
            for _ in 0..4 {
                unsafe { (lib.inner.uloop_run_timeout)(50) };
            }
            let status = child
                .try_wait()
                .expect("uloop stole Rust child status")
                .expect("child should have exited during uloop run");
            assert_eq!(status.code(), Some(code));
            let output = child.wait_with_output().expect("capture child output");
            assert_eq!(output.status.code(), Some(code));
            assert_eq!(output.stdout, b"rust-child-output");
        }
    }
}
