use super::super::fmcs as bosa;
use crate::pon_engine::ploam::PloamFrame;
use std::{
    io,
    os::fd::AsRawFd,
    sync::atomic::{AtomicU16, Ordering},
    time::Duration,
};

// Daemon-owned business allocations outlive a temporary OMCC diagnostic lease.
// Access is serialized by lock_onu; offline cleanup clears ownership before reuse.
static BUSINESS_ALLOCATIONS: AtomicU16 = AtomicU16::new(0);
static BUSINESS_GRANT_FLOORS: [AtomicU16; 16] = [const { AtomicU16::new(0) }; 16];
static MANAGEMENT_GRANT_FLOORS: [AtomicU16; 16] = [const { AtomicU16::new(60) }; 16];

pub fn forget_business_allocation(id: u8) {
    if id < 16 {
        BUSINESS_ALLOCATIONS.fetch_and(!(1u16 << id), Ordering::SeqCst);
        BUSINESS_GRANT_FLOORS[id as usize].store(0, Ordering::SeqCst);
        MANAGEMENT_GRANT_FLOORS[id as usize].store(60, Ordering::SeqCst);
    }
}

fn allocation_enabled_after_lease(owned: u16, id: u8) -> bool {
    owned & (1u16 << id) != 0
}

fn business_grant_control(id: u8) -> u32 {
    grant_control(id, &BUSINESS_GRANT_FLOORS)
}

// An active OMCC lease needs its own grant before business ownership exists:
// identity collection itself runs before the device adapter can be selected.
fn management_grant_control(id: u8) -> u32 {
    0x11000000 | u32::from(MANAGEMENT_GRANT_FLOORS[id as usize].load(Ordering::SeqCst))
}

fn grant_control(id: u8, floors: &[AtomicU16; 16]) -> u32 {
    let floor = if allocation_enabled_after_lease(BUSINESS_ALLOCATIONS.load(Ordering::SeqCst), id) {
        floors[id as usize].load(Ordering::SeqCst)
    } else {
        0
    };
    0x11000000 | u32::from(floor)
}
extern "C" {
    fn flock(fd: i32, operation: i32) -> i32;
}

/// Project hardware serialization; a lease restores only its own bitmap bit.
pub fn update_onu_enable(id: u8, enabled: bool) -> io::Result<()> {
    if id > 15 {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "ONU slot"));
    }
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open("/tmp/fttr-hardware.lock")?;
    if unsafe { flock(lock.as_raw_fd(), 2) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let value = bosa::read_fpga_reg(0x01000008)?;
    bosa::write_fpga_reg(0x01000008, enable_bit(value, id, enabled))
}
fn enable_bit(value: u32, id: u8, enabled: bool) -> u32 {
    if enabled {
        value | (1u32 << id)
    } else {
        value & !(1u32 << id)
    }
}
pub struct ChannelLease {
    id: u8,
    previous: u16,
}
impl crate::platform::context::ChannelGuard for ChannelLease {}
impl ChannelLease {
    pub fn open(id: u8, channel: u16) -> io::Result<Self> {
        Ok(Self {
            id,
            previous: bosa::exchange_omci_channel(id, channel)?,
        })
    }
}
impl Drop for ChannelLease {
    fn drop(&mut self) {
        if let Err(e) = bosa::exchange_omci_channel(self.id, self.previous) {
            eprintln!("OMCI channel restore failed: {e}");
        }
    }
}
/// Reversible, per-ONU stock OMCC resource ownership (project platform object).
pub struct StockOmccLease {
    id: u8,
    saved: Vec<(u32, u32)>,
    disarmed: bool,
    business_grant_floor: u16,
    management_grant_floor: u16,
}
impl StockOmccLease {
    pub fn open(id: u8) -> io::Result<Self> {
        if id > 15 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "ONU has no verified hardware slot",
            ));
        }
        let mut saved = Vec::new();
        for addr in [
            0x01000008,
            0x0500009c + 16 * id as u32,
            0x050000a4 + 16 * id as u32,
        ] {
            saved.push((addr, bosa::read_fpga_reg(addr)?));
        }
        let lease = Self {
            id,
            saved,
            disarmed: false,
            business_grant_floor: (business_grant_control(id) & 0xffff) as u16,
            management_grant_floor: (management_grant_control(id) & 0xffff) as u16,
        };
        for (i, &(addr, old)) in lease.saved.iter().enumerate() {
            let value = match i {
                0 => old | (1 << id),
                2 => business_grant_control(id),
                _ => management_grant_control(id),
            };
            println!("STOCK_OMCC {addr:08x}: {old:08x}->{value:08x}");
            if i == 0 {
                update_onu_enable(id, true)?;
            } else {
                bosa::write_fpga_reg(addr, value)?;
            }
        }
        lease.configure_port(true)?;
        std::thread::sleep(Duration::from_millis(500));
        Ok(lease)
    }

    /// Set a bounded business grant floor on this lease's slot only.
    /// Field comparisons verified these low bits extend the business BWmap
    /// allocation even when the dynamic DBRu feedback is not usable. Larger
    /// unbounded values can starve OMCC and other ONUs; only the verified small
    /// range is accepted. Device policy chooses whether to request a floor.
    /// A failed configuration still restores the registers saved by this lease.
    pub fn set_business_grant_floor(&mut self, bytes: u16) -> io::Result<()> {
        self.set_grant_floor(0x050000a4, bytes)?;
        self.business_grant_floor = bytes;
        Ok(())
    }

    /// Keep a bounded OMCC allocation large enough for a baseline response.
    /// Device-specific policy must validate OMCI and business coexistence; this
    /// platform primitive does not infer policy from the ONU vendor.
    pub fn set_management_grant_floor(&mut self, bytes: u16) -> io::Result<()> {
        self.set_grant_floor(0x0500009c, bytes)?;
        self.management_grant_floor = bytes;
        Ok(())
    }

    fn set_grant_floor(&mut self, base: u32, bytes: u16) -> io::Result<()> {
        if !(1..=1300).contains(&bytes) {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "grant floor"));
        }
        let address = base + 16 * u32::from(self.id);
        let value = 0x11000000 | u32::from(bytes);
        bosa::write_fpga_reg(address, value)?;
        if bosa::read_fpga_reg(address)? != value {
            return Err(io::Error::other("grant floor readback failed"));
        }
        Ok(())
    }

    /// Disarm the lease teardown when transitioning to a persistent business service.
    /// This prevents dropping the Alloc-ID and FPGA hardware mappings when configuration succeeds.
    pub fn disarm(&mut self) {
        BUSINESS_GRANT_FLOORS[self.id as usize].store(self.business_grant_floor, Ordering::SeqCst);
        MANAGEMENT_GRANT_FLOORS[self.id as usize]
            .store(self.management_grant_floor, Ordering::SeqCst);
        BUSINESS_ALLOCATIONS.fetch_or(1u16 << self.id, Ordering::SeqCst);
        self.disarmed = true;
    }

    fn configure_port(&self, enable: bool) -> io::Result<()> {
        let port = 0x100u16 + self.id as u16;
        let mut p = [0; 10];
        p[0] = (port >> 4) as u8;
        p[1] = (port << 4) as u8;
        p[2] = if enable { 1 } else { 0xff };
        for _ in 0..3 {
            bosa::send_ploam_msg(&PloamFrame::new(self.id, 0x0a, p).to_bytes())?;
        }
        Ok(())
    }
}
impl Drop for StockOmccLease {
    fn drop(&mut self) {
        if self.disarmed {
            return;
        }
        let enabled =
            allocation_enabled_after_lease(BUSINESS_ALLOCATIONS.load(Ordering::SeqCst), self.id);
        if let Err(e) = self.configure_port(enabled) {
            eprintln!("OMCC allocation restore failed: {e}");
        }
        for &(addr, value) in self.saved.iter().rev() {
            let result = if addr == 0x01000008 {
                update_onu_enable(self.id, value & (1u32 << self.id) != 0)
            } else {
                bosa::write_fpga_reg(addr, value)
            };
            if let Err(e) = result {
                eprintln!("OMCC restore {addr:08x} failed: {e}");
            }
        }
    }
}
impl crate::platform::context::ManagementLease for StockOmccLease {
    fn set_business_grant_floor(&mut self, bytes: u16) -> io::Result<()> {
        StockOmccLease::set_business_grant_floor(self, bytes)
    }
    fn set_management_grant_floor(&mut self, bytes: u16) -> io::Result<()> {
        StockOmccLease::set_management_grant_floor(self, bytes)
    }
    fn disarm(&mut self) -> io::Result<()> {
        StockOmccLease::disarm(self);
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    #[test]
    fn temporary_diagnostics_preserve_only_owned_business_allocations() {
        use super::*;
        let mut service = StockOmccLease {
            id: 15,
            saved: Vec::new(),
            disarmed: false,
            business_grant_floor: 254,
            management_grant_floor: 60,
        };
        forget_business_allocation(15);
        assert!(!allocation_enabled_after_lease(
            BUSINESS_ALLOCATIONS.load(Ordering::SeqCst),
            15
        ));
        service.disarm();
        drop(service); // Persistent service must not run temporary teardown.
        assert_eq!(business_grant_control(15), 0x110000fe);
        assert_eq!(management_grant_control(15), 0x1100003c);
        assert!(allocation_enabled_after_lease(
            BUSINESS_ALLOCATIONS.load(Ordering::SeqCst),
            15
        ));
        forget_business_allocation(15);
        assert_eq!(business_grant_control(15), 0x11000000);
        // Initial identity reads still get the management floor, but cannot
        // create or retain a business allocation.
        assert_eq!(management_grant_control(15), 0x1100003c);
        assert!(!allocation_enabled_after_lease(
            BUSINESS_ALLOCATIONS.load(Ordering::SeqCst),
            15
        ));
    }
    #[test]
    fn disabling_one_slot_preserves_other_live_slots() {
        let both = super::enable_bit(super::enable_bit(0x80000000, 1, true), 2, true);
        assert_eq!(super::enable_bit(both, 1, false), 0x80000004);
        assert_eq!(super::enable_bit(both, 2, false), 0x80000002);
    }
}
