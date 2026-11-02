//! Finding drives, and deciding which ones are safe to offer.
//!
//! This is the part of SKJ Flash that can destroy somebody's computer, so it is deliberately
//! cautious: a drive is only offered when we are sure it is removable, and the disk holding
//! Windows is marked and never offered by default. `include_internal` exists for the person who
//! genuinely means it, and even then the UI has to make them confirm the model and size.
//!
//! Nothing in this module writes. Writing lives in `write.rs`, and it takes a `Device` that came
//! from here rather than a path typed by hand, so there is no way to aim at a disk we never saw.

use std::fmt;

/// A whole physical disk, not a partition - flashing replaces the partition table too.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Device {
    /// What to open to read or write it, e.g. `\\.\PhysicalDrive2` or `/dev/sdb`
    pub path: String,
    /// Shown to the user: "SanDisk Ultra USB 3.0"
    pub model: String,
    pub size: u64,
    /// The OS says this is removable media
    pub removable: bool,
    /// USB / SD, as opposed to an internal SATA or NVMe disk
    pub external_bus: bool,
    /// This disk holds the running operating system. Never offered, never writable.
    pub system: bool,
    /// Drive letters or mount points currently on it, for the confirmation screen
    pub mounts: Vec<String>,
}

impl Device {
    /// Only drives we are confident about are offered without the user asking for more.
    ///
    /// Both a removable flag and an external bus are required: a USB-attached SSD reports
    /// "not removable", and an internal card reader can report "removable" with nothing in it.
    /// Requiring both keeps the default list to things that really are memory sticks and SD cards.
    pub fn is_safe_default(&self) -> bool {
        !self.system && self.size > 0 && (self.removable && self.external_bus)
    }

    /// A short human description for the confirmation step: "SanDisk Ultra (29.3 GB) - D:, E:"
    pub fn describe(&self) -> String {
        let mut s = format!("{} ({})", self.model, human_size(self.size));
        if !self.mounts.is_empty() {
            s.push_str(" - ");
            s.push_str(&self.mounts.join(", "));
        }
        s
    }
}

impl fmt::Display for Device {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {}", self.path, self.describe())
    }
}

pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = bytes as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 {
        format!("{} {}", bytes, UNITS[0])
    } else {
        format!("{:.1} {}", v, UNITS[u])
    }
}

/// How many disks the OS let us look at, before any filtering. Lets the caller tell "nothing
/// is plugged in" apart from "we could not open anything", which usually means no administrator.
pub fn count_seen() -> usize {
    imp::list().len()
}

/// Every disk we can see. `include_internal` adds the ones that are not clearly removable;
/// the disk running the OS is always marked `system` so the UI can refuse it outright.
pub fn list(include_internal: bool) -> Vec<Device> {
    let mut v = imp::list();
    if !include_internal {
        v.retain(|d| d.is_safe_default());
    } else {
        // even then, never the one we are booted from
        v.retain(|d| !d.system && d.size > 0);
    }
    v.sort_by(|a, b| a.path.cmp(&b.path));
    v
}

#[cfg(windows)]
mod imp {
    use super::Device;
    use std::os::windows::ffi::OsStrExt;
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{CloseHandle, HANDLE, MAX_PATH};
    use windows::Win32::Storage::FileSystem::{
        CreateFileW, GetLogicalDrives, BusTypeSd, BusTypeUsb, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ,
        FILE_SHARE_WRITE, IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS, OPEN_EXISTING,
    };
    use windows::Win32::System::Ioctl::{
        PropertyStandardQuery, StorageDeviceProperty, DISK_GEOMETRY_EX, GET_LENGTH_INFORMATION,
        IOCTL_DISK_GET_DRIVE_GEOMETRY_EX, IOCTL_DISK_GET_LENGTH_INFO, IOCTL_STORAGE_QUERY_PROPERTY,
        STORAGE_DEVICE_DESCRIPTOR, STORAGE_PROPERTY_QUERY, VOLUME_DISK_EXTENTS,
    };
    use windows::Win32::System::IO::DeviceIoControl;

    fn w(s: &str) -> Vec<u16> {
        std::ffi::OsStr::new(s).encode_wide().chain(Some(0)).collect()
    }

    /// Open for *querying only* - no access rights at all, so this can never write and does not
    /// need administrator. Writing opens its own handle, later, deliberately.
    fn open_query(path: &str) -> Option<HANDLE> {
        let p = w(path);
        unsafe {
            CreateFileW(
                PCWSTR(p.as_ptr()),
                0,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                None,
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL,
                None,
            )
            .ok()
        }
    }

    /// Same as open_query but keeps the error, so the CLI can say why a drive was skipped.
    fn open_query_dbg(path: &str) -> Result<HANDLE, windows::core::Error> {
        let p = w(path);
        unsafe {
            CreateFileW(
                PCWSTR(p.as_ptr()),
                0,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                None,
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL,
                None,
            )
        }
    }

    fn device_info(h: HANDLE) -> Option<(String, bool, bool)> {
        let query = STORAGE_PROPERTY_QUERY {
            PropertyId: StorageDeviceProperty,
            QueryType: PropertyStandardQuery,
            AdditionalParameters: [0],
        };
        // one buffer for the descriptor and the strings it points into
        let mut buf = vec![0u8; 1024];
        let mut returned = 0u32;
        let ok = unsafe {
            DeviceIoControl(
                h,
                IOCTL_STORAGE_QUERY_PROPERTY,
                Some(&query as *const _ as *const core::ffi::c_void),
                std::mem::size_of::<STORAGE_PROPERTY_QUERY>() as u32,
                Some(buf.as_mut_ptr() as *mut core::ffi::c_void),
                buf.len() as u32,
                Some(&mut returned),
                None,
            )
        };
        if ok.is_err() || (returned as usize) < std::mem::size_of::<STORAGE_DEVICE_DESCRIPTOR>() {
            return None;
        }
        let d = unsafe { &*(buf.as_ptr() as *const STORAGE_DEVICE_DESCRIPTOR) };

        // the descriptor stores byte offsets into the same buffer, 0 meaning "not given"
        let at = |off: u32| -> String {
            if off == 0 || off as usize >= buf.len() {
                return String::new();
            }
            let bytes = &buf[off as usize..];
            let end = bytes.iter().position(|&c| c == 0).unwrap_or(0);
            String::from_utf8_lossy(&bytes[..end]).trim().to_string()
        };
        let vendor = at(d.VendorIdOffset);
        let product = at(d.ProductIdOffset);
        let model = match (vendor.is_empty(), product.is_empty()) {
            (true, true) => "Unknown drive".to_string(),
            (true, false) => product,
            (false, true) => vendor,
            (false, false) => format!("{} {}", vendor, product),
        };
        let external = d.BusType == BusTypeUsb || d.BusType == BusTypeSd;
        Some((model, d.RemovableMedia, external))
    }

    /// How big the disk is.
    ///
    /// Geometry first, because IOCTL_DISK_GET_LENGTH_INFO wants read access and these handles
    /// are opened with none at all - that is the whole point of them. GET_DRIVE_GEOMETRY_EX
    /// answers without it. The length call stays as a fallback for anything that dislikes
    /// geometry, and returns 0 only when neither works.
    fn length(h: HANDLE) -> u64 {
        let mut gx = DISK_GEOMETRY_EX::default();
        let mut returned = 0u32;
        let ok = unsafe {
            DeviceIoControl(
                h,
                IOCTL_DISK_GET_DRIVE_GEOMETRY_EX,
                None,
                0,
                Some(&mut gx as *mut _ as *mut core::ffi::c_void),
                std::mem::size_of::<DISK_GEOMETRY_EX>() as u32,
                Some(&mut returned),
                None,
            )
        };
        if ok.is_ok() && gx.DiskSize > 0 {
            return gx.DiskSize as u64;
        }

        let mut li = GET_LENGTH_INFORMATION::default();
        let ok = unsafe {
            DeviceIoControl(
                h,
                IOCTL_DISK_GET_LENGTH_INFO,
                None,
                0,
                Some(&mut li as *mut _ as *mut core::ffi::c_void),
                std::mem::size_of::<GET_LENGTH_INFORMATION>() as u32,
                Some(&mut returned),
                None,
            )
        };
        if ok.is_ok() {
            li.Length as u64
        } else {
            0
        }
    }

    /// Which physical disk a volume like `\\.\C:` sits on. A volume can span several disks, so
    /// every extent counts - a striped volume touching a disk is enough to call that disk in use.
    fn disks_of_volume(letter: char) -> Vec<u32> {
        let path = format!(r"\\.\{}:", letter);
        let Some(h) = open_query(&path) else { return Vec::new() };
        let mut buf = vec![0u8; 1024];
        let mut returned = 0u32;
        let ok = unsafe {
            DeviceIoControl(
                h,
                IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS,
                None,
                0,
                Some(buf.as_mut_ptr() as *mut core::ffi::c_void),
                buf.len() as u32,
                Some(&mut returned),
                None,
            )
        };
        let mut out = Vec::new();
        if ok.is_ok() {
            let ext = unsafe { &*(buf.as_ptr() as *const VOLUME_DISK_EXTENTS) };
            let n = ext.NumberOfDiskExtents as usize;
            let first = ext.Extents.as_ptr();
            for i in 0..n {
                out.push(unsafe { (*first.add(i)).DiskNumber });
            }
        }
        unsafe { let _ = CloseHandle(h); }
        out
    }

    /// Drive letters, and the physical disk each one lives on.
    fn letters() -> Vec<(char, Vec<u32>)> {
        let mask = unsafe { GetLogicalDrives() };
        let mut out = Vec::new();
        for i in 0..26u32 {
            if mask & (1 << i) == 0 {
                continue;
            }
            let c = (b'A' + i as u8) as char;
            let disks = disks_of_volume(c);
            if !disks.is_empty() {
                out.push((c, disks));
            }
        }
        out
    }

    /// The disk Windows itself is running from.
    fn system_disk() -> Option<u32> {
        let mut buf = [0u16; MAX_PATH as usize];
        let n = unsafe { windows::Win32::System::SystemInformation::GetWindowsDirectoryW(Some(&mut buf)) };
        if n == 0 {
            return None;
        }
        let dir = String::from_utf16_lossy(&buf[..n as usize]);
        let c = dir.chars().next()?;
        disks_of_volume(c).first().copied()
    }

    pub fn list() -> Vec<Device> {
        let debug = std::env::var_os("SKJ_FLASH_DEBUG").is_some();
        let sys = system_disk();
        let lets = letters();
        let mut out = Vec::new();
        // 32 is well past any desktop machine; the loop stops caring once a disk is absent
        for n in 0..32u32 {
            let path = format!(r"\\.\PhysicalDrive{}", n);
            let h = match open_query_dbg(&path) {
                Ok(h) => h,
                Err(e) => {
                    if debug && n < 4 {
                        eprintln!("[skj-flash] open {path}: {e}");
                    }
                    continue;
                }
            };
            let info = device_info(h);
            let size = length(h);
            unsafe { let _ = CloseHandle(h); }
            let Some((model, removable, external_bus)) = info else {
                if debug {
                    eprintln!("[skj-flash] {path}: opened, but the device query failed");
                }
                continue;
            };
            let mounts: Vec<String> = lets
                .iter()
                .filter(|(_, disks)| disks.contains(&n))
                .map(|(c, _)| format!("{}:", c))
                .collect();
            let d = Device {
                path,
                model,
                size,
                removable,
                external_bus,
                system: sys == Some(n),
                mounts,
            };
            if debug {
                eprintln!(
                    "[skj-flash] {} | {} | size={} removable={} external={} system={} mounts={:?}",
                    d.path, d.model, d.size, d.removable, d.external_bus, d.system, d.mounts
                );
            }
            out.push(d);
        }
        out
    }
}

#[cfg(not(windows))]
mod imp {
    use super::Device;

    // Linux and macOS come next; until then the list is simply empty rather than wrong, because
    // guessing at device paths is exactly how a tool like this destroys somebody's disk.
    pub fn list() -> Vec<Device> {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dev(removable: bool, external: bool, system: bool, size: u64) -> Device {
        Device {
            path: r"\\.\PhysicalDrive9".into(),
            model: "Test".into(),
            size,
            removable,
            external_bus: external,
            system,
            mounts: vec![],
        }
    }

    #[test]
    fn only_real_removable_drives_are_offered() {
        // a memory stick: removable and on USB
        assert!(dev(true, true, false, 1 << 30).is_safe_default());
        // an internal SSD
        assert!(!dev(false, false, false, 1 << 40).is_safe_default());
        // a USB-attached SSD reports "not removable" - not offered by default, on purpose
        assert!(!dev(false, true, false, 1 << 40).is_safe_default());
        // an empty internal card reader slot
        assert!(!dev(true, false, false, 0).is_safe_default());
    }

    #[test]
    fn the_system_disk_is_never_offered() {
        // even if it somehow looked removable and external
        assert!(!dev(true, true, true, 1 << 40).is_safe_default());
    }

    #[test]
    fn sizes_read_the_way_people_expect() {
        assert_eq!(human_size(0), "0 B");
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(1024), "1.0 KB");
        assert_eq!(human_size(1536), "1.5 KB");
        assert_eq!(human_size(32 * 1024 * 1024 * 1024), "32.0 GB");
    }

    #[test]
    fn describe_mentions_what_would_be_lost() {
        let mut d = dev(true, true, false, 1024 * 1024 * 1024);
        d.model = "SanDisk Ultra".into();
        d.mounts = vec!["D:".into(), "E:".into()];
        assert_eq!(d.describe(), "SanDisk Ultra (1.0 GB) - D:, E:");
    }
}
