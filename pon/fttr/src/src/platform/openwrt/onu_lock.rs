use std::{
    fs::{File, OpenOptions},
    io,
    os::fd::AsRawFd,
};

/// Serialize workers and diagnostic processes. Never unlink flock files.
pub fn lock_onu(id: u8) -> io::Result<File> {
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(format!("/tmp/fttr-onu-{id}.lock"))?;
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(file)
}
