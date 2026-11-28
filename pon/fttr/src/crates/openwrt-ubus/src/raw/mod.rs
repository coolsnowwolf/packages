pub mod bindings;
pub mod dylib;

pub use bindings::*;
pub use dylib::*;

use std::sync::Mutex;

/// Global process-level lock protecting upstream libubus shared static buffers
/// (e.g. `static struct blob_buf b` in libubus-io.c) across threads.
pub static LIBUBUS_LOCK: Mutex<()> = Mutex::new(());
