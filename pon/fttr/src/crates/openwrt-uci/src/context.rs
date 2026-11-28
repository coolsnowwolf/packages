// Copyright 2026 Lazuli Kao / openwrt-fttr contributors
// SPDX-License-Identifier: Apache-2.0

use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_int};
use std::path::Path;
use std::str::FromStr;

use crate::error::UciError;
use crate::raw::*;

#[derive(Debug, Clone)]
pub struct UciSectionInfo {
    pub name: String,
    pub section_type: String,
    pub anonymous: bool,
}

pub struct Uci {
    lib: LibUci,
    ctx: *mut uci_context,
}

unsafe impl Send for Uci {}

impl Uci {
    pub fn new() -> Result<Self, UciError> {
        let lib = LibUci::load().map_err(UciError::LoadLibrary)?;
        Self::new_with_lib(lib)
    }

    pub fn new_with_lib(lib: LibUci) -> Result<Self, UciError> {
        unsafe {
            let ctx = lib.uci_alloc_context();
            if ctx.is_null() {
                return Err(UciError::AllocContext);
            }
            Ok(Self { lib, ctx })
        }
    }

    fn get_last_error(&self, ret: c_int, fallback: &str) -> UciError {
        let msg = unsafe { self.lib.get_error_message(self.ctx) };
        let detail = if msg.is_empty() {
            fallback.to_string()
        } else {
            msg
        };
        match ret {
            UCI_ERR_NOTFOUND => UciError::PackageNotFound(detail),
            UCI_ERR_PARSE => UciError::ParseError(detail),
            UCI_ERR_IO => UciError::IoError(detail),
            UCI_ERR_INVAL => UciError::InvalidParameter(detail),
            _ => UciError::Backend(detail),
        }
    }

    pub fn set_confdir<P: AsRef<Path>>(&mut self, path: P) -> Result<(), UciError> {
        let c_path = CString::new(path.as_ref().to_string_lossy().as_bytes())?;
        unsafe {
            let ret = self.lib.uci_set_confdir(self.ctx, c_path.as_ptr());
            if ret != UCI_OK {
                return Err(self.get_last_error(ret, "uci_set_confdir failed"));
            }
        }
        Ok(())
    }

    pub fn set_savedir<P: AsRef<Path>>(&mut self, path: P) -> Result<(), UciError> {
        let c_path = CString::new(path.as_ref().to_string_lossy().as_bytes())?;
        unsafe {
            let ret = self.lib.uci_set_savedir(self.ctx, c_path.as_ptr());
            if ret != UCI_OK {
                return Err(self.get_last_error(ret, "uci_set_savedir failed"));
            }
        }
        Ok(())
    }

    pub fn get(
        &mut self,
        package: &str,
        section: &str,
        option: &str,
    ) -> Result<Option<String>, UciError> {
        let path_str = format!("{package}.{section}.{option}");
        let mut buf = path_str.into_bytes();
        buf.push(0); // Null terminator for in-place tokenization

        let mut ptr = uci_ptr::default();

        unsafe {
            let ret =
                self.lib
                    .uci_lookup_ptr(self.ctx, &mut ptr, buf.as_mut_ptr() as *mut c_char, true);
            if ret != UCI_OK {
                if ret == UCI_ERR_NOTFOUND {
                    return Ok(None);
                }
                return Err(self.get_last_error(ret, "uci_lookup_ptr failed"));
            }
            if ptr.o.is_null() || (ptr.flags & UCI_LOOKUP_COMPLETE == 0) {
                return Ok(None);
            }

            let opt = &*ptr.o;
            match opt.type_ {
                uci_option_type::UCI_TYPE_STRING => {
                    let str_ptr = opt.v.string;
                    if str_ptr.is_null() {
                        Ok(None)
                    } else {
                        Ok(Some(CStr::from_ptr(str_ptr).to_string_lossy().into_owned()))
                    }
                }
                uci_option_type::UCI_TYPE_LIST => {
                    let mut values = Vec::new();
                    let head = &opt.v.list;
                    let mut curr = head.next;
                    while curr != (head as *const uci_list as *mut uci_list) && !curr.is_null() {
                        let elem = curr as *mut uci_element;
                        if !(*elem).name.is_null() {
                            values
                                .push(CStr::from_ptr((*elem).name).to_string_lossy().into_owned());
                        }
                        curr = (*curr).next;
                    }
                    Ok(Some(values.join(" ")))
                }
            }
        }
    }

    /// Read a list option directly as a `Vec<String>` preserving element boundaries
    pub fn get_list(
        &mut self,
        package: &str,
        section: &str,
        option: &str,
    ) -> Result<Option<Vec<String>>, UciError> {
        let path_str = format!("{package}.{section}.{option}");
        let mut buf = path_str.into_bytes();
        buf.push(0);

        let mut ptr = uci_ptr::default();

        unsafe {
            let ret =
                self.lib
                    .uci_lookup_ptr(self.ctx, &mut ptr, buf.as_mut_ptr() as *mut c_char, true);
            if ret != UCI_OK {
                if ret == UCI_ERR_NOTFOUND {
                    return Ok(None);
                }
                return Err(self.get_last_error(ret, "uci_lookup_ptr failed"));
            }
            if ptr.o.is_null() || (ptr.flags & UCI_LOOKUP_COMPLETE == 0) {
                return Ok(None);
            }

            let opt = &*ptr.o;
            match opt.type_ {
                uci_option_type::UCI_TYPE_STRING => {
                    let str_ptr = opt.v.string;
                    if str_ptr.is_null() {
                        Ok(None)
                    } else {
                        Ok(Some(vec![CStr::from_ptr(str_ptr)
                            .to_string_lossy()
                            .into_owned()]))
                    }
                }
                uci_option_type::UCI_TYPE_LIST => {
                    let mut values = Vec::new();
                    let head = &opt.v.list;
                    let mut curr = head.next;
                    while curr != (head as *const uci_list as *mut uci_list) && !curr.is_null() {
                        let elem = curr as *mut uci_element;
                        if !(*elem).name.is_null() {
                            values
                                .push(CStr::from_ptr((*elem).name).to_string_lossy().into_owned());
                        }
                        curr = (*curr).next;
                    }
                    Ok(Some(values))
                }
            }
        }
    }

    /// Read an option and parse directly into a typed primitive (e.g. u32, bool, IpAddr)
    pub fn get_parsed<T: FromStr>(
        &mut self,
        package: &str,
        section: &str,
        option: &str,
    ) -> Result<Option<T>, UciError> {
        match self.get(package, section, option)? {
            Some(val) => val
                .parse::<T>()
                .map(Some)
                .map_err(|_| UciError::InvalidParameter(format!("Failed to parse value '{val}'"))),
            None => Ok(None),
        }
    }

    pub fn set(
        &mut self,
        package: &str,
        section: &str,
        option: &str,
        value: &str,
    ) -> Result<(), UciError> {
        let path_str = if option.is_empty() {
            format!("{package}.{section}={value}")
        } else {
            format!("{package}.{section}.{option}={value}")
        };
        let mut buf = path_str.into_bytes();
        buf.push(0);

        let mut ptr = uci_ptr::default();

        unsafe {
            let ret =
                self.lib
                    .uci_lookup_ptr(self.ctx, &mut ptr, buf.as_mut_ptr() as *mut c_char, true);
            if ret != UCI_OK {
                return Err(UciError::Backend(self.lib.get_error_message(self.ctx)));
            }

            let ret = self.lib.uci_set(self.ctx, &mut ptr);
            if ret != UCI_OK {
                return Err(UciError::Backend(self.lib.get_error_message(self.ctx)));
            }

            let ret = self.lib.uci_save(self.ctx, ptr.p);
            if ret != UCI_OK {
                return Err(UciError::Backend(self.lib.get_error_message(self.ctx)));
            }
        }
        Ok(())
    }

    /// Ensure a section of the specified type exists (e.g. named section `network.fttr_tr069=interface`)
    pub fn ensure_section(
        &mut self,
        package: &str,
        section: &str,
        section_type: &str,
    ) -> Result<(), UciError> {
        self.set(package, section, "", section_type)
    }

    /// Add an item to a list option (`uci add_list package.section.option=value`)
    pub fn add_list(
        &mut self,
        package: &str,
        section: &str,
        option: &str,
        value: &str,
    ) -> Result<(), UciError> {
        let path_str = format!("{package}.{section}.{option}={value}");
        let mut buf = path_str.into_bytes();
        buf.push(0);

        let mut ptr = uci_ptr::default();

        unsafe {
            let ret =
                self.lib
                    .uci_lookup_ptr(self.ctx, &mut ptr, buf.as_mut_ptr() as *mut c_char, true);
            if ret != UCI_OK {
                return Err(UciError::Backend(self.lib.get_error_message(self.ctx)));
            }

            let ret = self.lib.uci_add_list(self.ctx, &mut ptr);
            if ret != UCI_OK {
                return Err(UciError::Backend(self.lib.get_error_message(self.ctx)));
            }

            let ret = self.lib.uci_save(self.ctx, ptr.p);
            if ret != UCI_OK {
                return Err(UciError::Backend(self.lib.get_error_message(self.ctx)));
            }
        }
        Ok(())
    }

    /// Replace all elements of a list option with the given slice
    pub fn replace_list(
        &mut self,
        package: &str,
        section: &str,
        option: &str,
        values: &[String],
    ) -> Result<(), UciError> {
        self.delete(package, section, Some(option))?;
        for v in values {
            self.add_list(package, section, option, v)?;
        }
        Ok(())
    }

    pub fn delete(
        &mut self,
        package: &str,
        section: &str,
        option: Option<&str>,
    ) -> Result<(), UciError> {
        let path_str = match option {
            Some(opt) => format!("{package}.{section}.{opt}"),
            None => format!("{package}.{section}"),
        };
        let mut buf = path_str.into_bytes();
        buf.push(0);

        let mut ptr = uci_ptr::default();

        unsafe {
            let ret =
                self.lib
                    .uci_lookup_ptr(self.ctx, &mut ptr, buf.as_mut_ptr() as *mut c_char, true);
            if ret != UCI_OK {
                if ret == UCI_ERR_NOTFOUND {
                    return Ok(()); // Already absent
                }
                return Err(UciError::Backend(self.lib.get_error_message(self.ctx)));
            }

            // Check if the target actually exists in the lookup result
            let exists = if option.is_some() {
                !ptr.o.is_null() && (ptr.flags & UCI_LOOKUP_COMPLETE != 0)
            } else {
                !ptr.s.is_null()
            };

            if !exists {
                return Ok(()); // Idempotent: target does not exist, nothing to delete
            }

            let ret = self.lib.uci_delete(self.ctx, &mut ptr);
            if ret != UCI_OK && ret != UCI_ERR_NOTFOUND {
                return Err(UciError::Backend(self.lib.get_error_message(self.ctx)));
            }

            if !ptr.p.is_null() {
                let ret = self.lib.uci_save(self.ctx, ptr.p);
                if ret != UCI_OK {
                    return Err(UciError::Backend(self.lib.get_error_message(self.ctx)));
                }
            }
        }
        Ok(())
    }

    pub fn commit(&mut self, package: &str) -> Result<(), UciError> {
        let mut ptr = uci_ptr::default();
        let mut buf = package.as_bytes().to_vec();
        buf.push(0);

        unsafe {
            let ret =
                self.lib
                    .uci_lookup_ptr(self.ctx, &mut ptr, buf.as_mut_ptr() as *mut c_char, true);

            let mut p = if ret == UCI_OK && !ptr.p.is_null() {
                ptr.p
            } else {
                let c_pkg = CString::new(package)?;
                let mut loaded: *mut uci_package = std::ptr::null_mut();
                let ret = self.lib.uci_load(self.ctx, c_pkg.as_ptr(), &mut loaded);
                if (ret != UCI_OK && ret != UCI_ERR_DUPLICATE) || loaded.is_null() {
                    return Err(self.get_last_error(ret, "uci_load failed"));
                }
                loaded
            };

            let ret = self.lib.uci_commit(self.ctx, &mut p, false);
            if ret != UCI_OK {
                return Err(self.get_last_error(ret, "uci_commit failed"));
            }
        }
        Ok(())
    }

    pub fn revert(&mut self, package: &str) -> Result<(), UciError> {
        let mut buf = package.as_bytes().to_vec();
        buf.push(0);

        let mut ptr = uci_ptr::default();

        unsafe {
            let ret =
                self.lib
                    .uci_lookup_ptr(self.ctx, &mut ptr, buf.as_mut_ptr() as *mut c_char, true);
            if ret != UCI_OK {
                return Ok(());
            }

            let ret = self.lib.uci_revert(self.ctx, &mut ptr);
            if ret != UCI_OK {
                return Err(UciError::Backend(self.lib.get_error_message(self.ctx)));
            }
        }
        Ok(())
    }

    /// Export a loaded UCI package into canonical OpenWrt UCI text format.
    pub fn export_package(&mut self, package: &str) -> Result<String, UciError> {
        self.export_package_with_names(package, false)
    }

    /// Export with libuci's current IDs for anonymous sections (like `uci -n export`).
    pub fn export_package_with_names(
        &mut self,
        package: &str,
        names: bool,
    ) -> Result<String, UciError> {
        let mut ptr = uci_ptr::default();
        let mut buf = package.as_bytes().to_vec();
        buf.push(0);

        unsafe {
            let ret =
                self.lib
                    .uci_lookup_ptr(self.ctx, &mut ptr, buf.as_mut_ptr() as *mut c_char, true);

            let p = if ret == UCI_OK && !ptr.p.is_null() {
                ptr.p
            } else {
                let c_pkg = CString::new(package)?;
                let mut loaded: *mut uci_package = std::ptr::null_mut();
                let ret = self.lib.uci_load(self.ctx, c_pkg.as_ptr(), &mut loaded);
                if (ret != UCI_OK && ret != UCI_ERR_DUPLICATE) || loaded.is_null() {
                    return Err(self.get_last_error(ret, "uci_load failed"));
                }
                loaded
            };

            if p.is_null() {
                return Err(UciError::PackageNotFound(package.to_string()));
            }

            #[cfg(unix)]
            {
                let mut buf_ptr: *mut c_char = std::ptr::null_mut();
                let mut buf_size: libc::size_t = 0;
                let stream = libc::open_memstream(&mut buf_ptr, &mut buf_size);
                if stream.is_null() {
                    return Err(UciError::Backend("open_memstream failed".into()));
                }

                const UCI_FLAG_EXPORT_NAME: c_int = 1 << 2;
                let flags = (*self.ctx).flags;
                if names {
                    (*self.ctx).flags |= UCI_FLAG_EXPORT_NAME;
                }
                let ret = self.lib.uci_export(self.ctx, stream, p, true);
                (*self.ctx).flags = flags;
                libc::fclose(stream);

                if ret != UCI_OK {
                    if !buf_ptr.is_null() {
                        libc::free(buf_ptr as *mut std::ffi::c_void);
                    }
                    return Err(UciError::Backend(self.lib.get_error_message(self.ctx)));
                }

                if buf_ptr.is_null() {
                    return Ok(String::new());
                }

                let res = CStr::from_ptr(buf_ptr).to_string_lossy().into_owned();
                libc::free(buf_ptr as *mut std::ffi::c_void);
                Ok(res)
            }

            #[cfg(not(unix))]
            {
                Err(UciError::Backend(
                    "export_package not supported on non-unix target".into(),
                ))
            }
        }
    }
}

impl Drop for Uci {
    fn drop(&mut self) {
        if !self.ctx.is_null() {
            unsafe {
                self.lib.uci_free_context(self.ctx);
            }
        }
    }
}
