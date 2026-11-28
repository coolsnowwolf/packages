// Copyright 2026 Lazuli Kao / openwrt-fttr contributors
// SPDX-License-Identifier: Apache-2.0

use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_int, c_void};
use std::sync::Arc;

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

type FnUciAllocContext = unsafe extern "C" fn() -> *mut uci_context;
type FnUciFreeContext = unsafe extern "C" fn(ctx: *mut uci_context);
type FnUciSetConfdir = unsafe extern "C" fn(ctx: *mut uci_context, dir: *const c_char) -> c_int;
type FnUciSetSavedir = unsafe extern "C" fn(ctx: *mut uci_context, dir: *const c_char) -> c_int;
type FnUciLoad = unsafe extern "C" fn(
    ctx: *mut uci_context,
    name: *const c_char,
    package: *mut *mut uci_package,
) -> c_int;
type FnUciUnload = unsafe extern "C" fn(ctx: *mut uci_context, p: *mut uci_package) -> c_int;
type FnUciLookupPtr = unsafe extern "C" fn(
    ctx: *mut uci_context,
    ptr: *mut uci_ptr,
    str_: *mut c_char,
    extended: bool,
) -> c_int;
type FnUciSet = unsafe extern "C" fn(ctx: *mut uci_context, ptr: *mut uci_ptr) -> c_int;
type FnUciAddSection = unsafe extern "C" fn(
    ctx: *mut uci_context,
    p: *mut uci_package,
    type_: *const c_char,
    res: *mut *mut uci_section,
) -> c_int;
type FnUciDelete = unsafe extern "C" fn(ctx: *mut uci_context, ptr: *mut uci_ptr) -> c_int;
type FnUciAddList = unsafe extern "C" fn(ctx: *mut uci_context, ptr: *mut uci_ptr) -> c_int;
type FnUciDelList = unsafe extern "C" fn(ctx: *mut uci_context, ptr: *mut uci_ptr) -> c_int;
type FnUciRename = unsafe extern "C" fn(ctx: *mut uci_context, ptr: *mut uci_ptr) -> c_int;
type FnUciSave = unsafe extern "C" fn(ctx: *mut uci_context, p: *mut uci_package) -> c_int;
type FnUciCommit =
    unsafe extern "C" fn(ctx: *mut uci_context, p: *mut *mut uci_package, overwrite: bool) -> c_int;
type FnUciRevert = unsafe extern "C" fn(ctx: *mut uci_context, ptr: *mut uci_ptr) -> c_int;
type FnUciExport = unsafe extern "C" fn(
    ctx: *mut uci_context,
    stream: *mut libc::FILE,
    p: *mut uci_package,
    header: bool,
) -> c_int;
type FnUciGetErrorstr =
    unsafe extern "C" fn(ctx: *mut uci_context, dest: *mut *mut c_char, prefix: *const c_char);

#[derive(Clone)]
pub struct LibUci {
    inner: Arc<LibUciInner>,
}

struct LibUciInner {
    handle: *mut c_void,
    pub uci_alloc_context: FnUciAllocContext,
    pub uci_free_context: FnUciFreeContext,
    pub uci_set_confdir: FnUciSetConfdir,
    pub uci_set_savedir: FnUciSetSavedir,
    pub uci_load: FnUciLoad,
    pub uci_unload: FnUciUnload,
    pub uci_lookup_ptr: FnUciLookupPtr,
    pub uci_set: FnUciSet,
    pub uci_add_section: FnUciAddSection,
    pub uci_delete: FnUciDelete,
    pub uci_add_list: FnUciAddList,
    pub uci_del_list: FnUciDelList,
    pub uci_rename: FnUciRename,
    pub uci_save: FnUciSave,
    pub uci_commit: FnUciCommit,
    pub uci_revert: FnUciRevert,
    pub uci_export: FnUciExport,
    pub uci_get_errorstr: Option<FnUciGetErrorstr>,
}

unsafe impl Send for LibUciInner {}
unsafe impl Sync for LibUciInner {}

impl Drop for LibUciInner {
    fn drop(&mut self) {
        if !self.handle.is_null() {
            unsafe {
                dlclose(self.handle);
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
            // Sort in descending order to prefer newer timestamps/versions
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

impl LibUci {
    pub fn load() -> Result<Self, String> {
        if let Ok(override_path) = std::env::var("LIBUCI_SO_PATH") {
            if let Ok(lib) = Self::load_from_path(&override_path) {
                return Ok(lib);
            }
        }

        let candidates = discover_candidate_libraries("libuci.so");
        let mut errors = Vec::new();
        for path in &candidates {
            match Self::load_from_path(path) {
                Ok(lib) => return Ok(lib),
                Err(err) => errors.push(format!("{path}: {err}")),
            }
        }

        Err(format!(
            "Failed to load OpenWrt libuci library from standard paths:\n{}",
            errors.join("\n")
        ))
    }

    pub fn load_from_path(path: &str) -> Result<Self, String> {
        let c_path = CString::new(path).map_err(|e| e.to_string())?;
        unsafe {
            let handle = dlopen(c_path.as_ptr(), RTLD_LAZY | RTLD_LOCAL);
            if handle.is_null() {
                let err = dlerror();
                let msg = if !err.is_null() {
                    CStr::from_ptr(err).to_string_lossy().into_owned()
                } else {
                    format!("Failed to open {path}")
                };
                return Err(msg);
            }

            macro_rules! resolve {
                ($sym:ident, $type:ident) => {{
                    let c_sym = CString::new(stringify!($sym)).unwrap();
                    let ptr = dlsym(handle, c_sym.as_ptr());
                    if ptr.is_null() {
                        dlclose(handle);
                        return Err(format!("Symbol '{}' missing in {}", stringify!($sym), path));
                    }
                    std::mem::transmute::<*mut c_void, $type>(ptr)
                }};
            }

            macro_rules! resolve_opt {
                ($sym:ident, $type:ident) => {{
                    let c_sym = CString::new(stringify!($sym)).unwrap();
                    let ptr = dlsym(handle, c_sym.as_ptr());
                    if ptr.is_null() {
                        None
                    } else {
                        Some(std::mem::transmute::<*mut c_void, $type>(ptr))
                    }
                }};
            }

            let inner = LibUciInner {
                handle,
                uci_alloc_context: resolve!(uci_alloc_context, FnUciAllocContext),
                uci_free_context: resolve!(uci_free_context, FnUciFreeContext),
                uci_set_confdir: resolve!(uci_set_confdir, FnUciSetConfdir),
                uci_set_savedir: resolve!(uci_set_savedir, FnUciSetSavedir),
                uci_load: resolve!(uci_load, FnUciLoad),
                uci_unload: resolve!(uci_unload, FnUciUnload),
                uci_lookup_ptr: resolve!(uci_lookup_ptr, FnUciLookupPtr),
                uci_set: resolve!(uci_set, FnUciSet),
                uci_add_section: resolve!(uci_add_section, FnUciAddSection),
                uci_delete: resolve!(uci_delete, FnUciDelete),
                uci_add_list: resolve!(uci_add_list, FnUciAddList),
                uci_del_list: resolve!(uci_del_list, FnUciDelList),
                uci_rename: resolve!(uci_rename, FnUciRename),
                uci_save: resolve!(uci_save, FnUciSave),
                uci_commit: resolve!(uci_commit, FnUciCommit),
                uci_revert: resolve!(uci_revert, FnUciRevert),
                uci_export: resolve!(uci_export, FnUciExport),
                uci_get_errorstr: resolve_opt!(uci_get_errorstr, FnUciGetErrorstr),
            };

            Ok(LibUci {
                inner: Arc::new(inner),
            })
        }
    }

    #[inline]
    pub unsafe fn uci_alloc_context(&self) -> *mut uci_context {
        (self.inner.uci_alloc_context)()
    }

    #[inline]
    pub unsafe fn uci_free_context(&self, ctx: *mut uci_context) {
        (self.inner.uci_free_context)(ctx)
    }

    #[inline]
    pub unsafe fn uci_set_confdir(&self, ctx: *mut uci_context, dir: *const c_char) -> c_int {
        (self.inner.uci_set_confdir)(ctx, dir)
    }

    #[inline]
    pub unsafe fn uci_set_savedir(&self, ctx: *mut uci_context, dir: *const c_char) -> c_int {
        (self.inner.uci_set_savedir)(ctx, dir)
    }

    #[inline]
    pub unsafe fn uci_load(
        &self,
        ctx: *mut uci_context,
        name: *const c_char,
        package: *mut *mut uci_package,
    ) -> c_int {
        (self.inner.uci_load)(ctx, name, package)
    }

    #[inline]
    pub unsafe fn uci_unload(&self, ctx: *mut uci_context, p: *mut uci_package) -> c_int {
        (self.inner.uci_unload)(ctx, p)
    }

    #[inline]
    pub unsafe fn uci_lookup_ptr(
        &self,
        ctx: *mut uci_context,
        ptr: *mut uci_ptr,
        str_: *mut c_char,
        extended: bool,
    ) -> c_int {
        (self.inner.uci_lookup_ptr)(ctx, ptr, str_, extended)
    }

    #[inline]
    pub unsafe fn uci_set(&self, ctx: *mut uci_context, ptr: *mut uci_ptr) -> c_int {
        (self.inner.uci_set)(ctx, ptr)
    }

    #[inline]
    pub unsafe fn uci_add_section(
        &self,
        ctx: *mut uci_context,
        p: *mut uci_package,
        type_: *const c_char,
        res: *mut *mut uci_section,
    ) -> c_int {
        (self.inner.uci_add_section)(ctx, p, type_, res)
    }

    #[inline]
    pub unsafe fn uci_delete(&self, ctx: *mut uci_context, ptr: *mut uci_ptr) -> c_int {
        (self.inner.uci_delete)(ctx, ptr)
    }

    #[inline]
    pub unsafe fn uci_add_list(&self, ctx: *mut uci_context, ptr: *mut uci_ptr) -> c_int {
        (self.inner.uci_add_list)(ctx, ptr)
    }

    #[inline]
    pub unsafe fn uci_del_list(&self, ctx: *mut uci_context, ptr: *mut uci_ptr) -> c_int {
        (self.inner.uci_del_list)(ctx, ptr)
    }

    #[inline]
    pub unsafe fn uci_rename(&self, ctx: *mut uci_context, ptr: *mut uci_ptr) -> c_int {
        (self.inner.uci_rename)(ctx, ptr)
    }

    #[inline]
    pub unsafe fn uci_save(&self, ctx: *mut uci_context, p: *mut uci_package) -> c_int {
        (self.inner.uci_save)(ctx, p)
    }

    #[inline]
    pub unsafe fn uci_commit(
        &self,
        ctx: *mut uci_context,
        p: *mut *mut uci_package,
        overwrite: bool,
    ) -> c_int {
        (self.inner.uci_commit)(ctx, p, overwrite)
    }

    #[inline]
    pub unsafe fn uci_revert(&self, ctx: *mut uci_context, ptr: *mut uci_ptr) -> c_int {
        (self.inner.uci_revert)(ctx, ptr)
    }

    #[inline]
    pub unsafe fn uci_export(
        &self,
        ctx: *mut uci_context,
        stream: *mut libc::FILE,
        p: *mut uci_package,
        header: bool,
    ) -> c_int {
        (self.inner.uci_export)(ctx, stream, p, header)
    }

    pub unsafe fn get_error_message(&self, ctx: *mut uci_context) -> String {
        if let Some(err_fn) = self.inner.uci_get_errorstr {
            let mut dest: *mut c_char = std::ptr::null_mut();
            let prefix = CString::new("uci").unwrap();
            err_fn(ctx, &mut dest, prefix.as_ptr());
            if !dest.is_null() {
                let msg = CStr::from_ptr(dest).to_string_lossy().into_owned();
                #[cfg(unix)]
                libc::free(dest as *mut c_void);
                return msg;
            }
        }
        "UCI operation failed".to_string()
    }
}
