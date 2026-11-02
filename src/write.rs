//! Writing to a raw disk, and proving it worked.
//!
//! Everything here takes a `Device` that came from `disk::list`, never a path, so there is no way
//! to aim at a disk that was never inspected and never passed the safety rules.
//!
//! Windows will quietly refuse, or silently discard, writes to regions a mounted filesystem thinks
//! it owns. So before writing anything the volumes on the disk are locked and dismounted, and the
//! locks are held until the write finishes. That is what `VolumeLocks` is for, and why it releases
//! the handles on drop even if the write fails part way.

use crate::disk::Device;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::time::Instant;

/// How far a long write has got.
#[derive(Clone, Copy, Debug)]
pub struct Progress {
    pub done: u64,
    pub total: u64,
    /// bytes per second, averaged over the whole run so far
    pub speed: f64,
}

impl Progress {
    pub fn percent(&self) -> f64 {
        if self.total == 0 {
            0.0
        } else {
            self.done as f64 * 100.0 / self.total as f64
        }
    }
}

/// What a self-test found.
#[derive(Debug)]
pub struct SelfTest {
    pub bytes: u64,
    pub wrote_ok: bool,
    pub restored_ok: bool,
    pub write_speed: f64,
    pub read_speed: f64,
}

/// Disks want whole sectors. 1 MiB is a multiple of every sector size in use and is big enough
/// that the per-write overhead stops mattering.
const CHUNK: usize = 1024 * 1024;

/// Prove the write path works, and put the disk back as it was.
///
/// Reads the first `bytes`, writes a pattern over them, reads it back to check it really landed,
/// then writes the original bytes back and checks those too. Nothing is left behind if it
/// finishes; if it is interrupted partway the region holds the pattern, and the disk needs
/// reformatting - which is why this is offered as a test and not run by itself.
pub fn self_test(dev: &Device, bytes: u64, mut progress: impl FnMut(Progress)) -> Result<SelfTest, String> {
    if dev.system {
        return Err("Refusing: that is the disk the system runs from".into());
    }
    let bytes = bytes.min(dev.size).max(CHUNK as u64) & !(CHUNK as u64 - 1);
    if bytes == 0 {
        return Err("Nothing to test".into());
    }

    // 1. keep what is there now
    let original = read_at(dev, 0, bytes, |p| progress(p))?;

    // 2. a pattern that cannot appear by accident, and differs every run so a stale read
    //    cannot pass for a fresh write
    let seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0x5C_1F_1A_58);
    let pattern = make_pattern(bytes as usize, seed);

    // 3. write it, timing as we go
    let t0 = Instant::now();
    write_at(dev, 0, &pattern, |p| progress(p))?;
    let write_speed = bytes as f64 / t0.elapsed().as_secs_f64().max(0.000_001);

    // 4. read it back - this is the bit that actually proves anything
    let t1 = Instant::now();
    let readback = read_at(dev, 0, bytes, |p| progress(p))?;
    let read_speed = bytes as f64 / t1.elapsed().as_secs_f64().max(0.000_001);
    let wrote_ok = readback == pattern;

    // 5. put the original back whatever happened above, then check that too
    write_at(dev, 0, &original, |p| progress(p))?;
    let restored = read_at(dev, 0, bytes, |p| progress(p))?;
    let restored_ok = restored == original;

    Ok(SelfTest { bytes, wrote_ok, restored_ok, write_speed, read_speed })
}

/// Deterministic from the seed, and nothing like real filesystem data.
fn make_pattern(len: usize, seed: u64) -> Vec<u8> {
    let mut v = Vec::with_capacity(len);
    let mut x = seed | 1;
    while v.len() < len {
        // xorshift64: cheap, and good enough that a half-written region cannot look correct
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        v.extend_from_slice(&x.to_le_bytes());
    }
    v.truncate(len);
    v
}

fn read_at(dev: &Device, offset: u64, len: u64, mut progress: impl FnMut(Progress)) -> Result<Vec<u8>, String> {
    let mut f = imp::open_read(dev)?;
    f.seek(SeekFrom::Start(offset)).map_err(|e| format!("seek: {e}"))?;
    let mut out = vec![0u8; len as usize];
    let t0 = Instant::now();
    let mut done = 0u64;
    for part in out.chunks_mut(CHUNK) {
        f.read_exact(part).map_err(|e| format!("read at {done}: {e}"))?;
        done += part.len() as u64;
        progress(Progress { done, total: len, speed: done as f64 / t0.elapsed().as_secs_f64().max(0.000_001) });
    }
    Ok(out)
}

fn write_at(dev: &Device, offset: u64, data: &[u8], mut progress: impl FnMut(Progress)) -> Result<(), String> {
    // held until this function returns, so the filesystem cannot remount underneath the write
    let _locks = imp::lock_volumes(dev)?;
    let mut f = imp::open_write(dev)?;
    f.seek(SeekFrom::Start(offset)).map_err(|e| format!("seek: {e}"))?;
    let total = data.len() as u64;
    let t0 = Instant::now();
    let mut done = 0u64;
    for part in data.chunks(CHUNK) {
        f.write_all(part).map_err(|e| format!("write at {done}: {e}"))?;
        done += part.len() as u64;
        progress(Progress { done, total, speed: done as f64 / t0.elapsed().as_secs_f64().max(0.000_001) });
    }
    // without this the last chunks can still be in the OS cache when the stick is pulled
    f.flush().map_err(|e| format!("flush: {e}"))?;
    imp::sync(&f)?;
    Ok(())
}

/// Write a disk image over the whole device. Not reachable from the CLI yet.
#[allow(dead_code)]
pub fn write_image(dev: &Device, image: &Path, mut progress: impl FnMut(Progress)) -> Result<(), String> {
    if dev.system {
        return Err("Refusing: that is the disk the system runs from".into());
    }
    let md = std::fs::metadata(image).map_err(|e| format!("{}: {e}", image.display()))?;
    if md.len() > dev.size {
        return Err(format!(
            "The image is {} but the drive is only {}",
            crate::disk::human_size(md.len()),
            crate::disk::human_size(dev.size)
        ));
    }
    let mut src = std::fs::File::open(image).map_err(|e| format!("{}: {e}", image.display()))?;
    let _locks = imp::lock_volumes(dev)?;
    let mut dst = imp::open_write(dev)?;
    let total = md.len();
    let t0 = Instant::now();
    let mut done = 0u64;
    let mut buf = vec![0u8; CHUNK];
    loop {
        let n = src.read(&mut buf).map_err(|e| format!("read image: {e}"))?;
        if n == 0 {
            break;
        }
        // the last chunk is padded out: disks only take whole sectors
        let end = (n + 511) & !511;
        buf[n..end].fill(0);
        dst.write_all(&buf[..end]).map_err(|e| format!("write at {done}: {e}"))?;
        done += n as u64;
        progress(Progress { done, total, speed: done as f64 / t0.elapsed().as_secs_f64().max(0.000_001) });
    }
    dst.flush().map_err(|e| format!("flush: {e}"))?;
    imp::sync(&dst)?;
    Ok(())
}

#[cfg(windows)]
mod imp {
    use super::Device;
    use std::fs::File;
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::io::FromRawHandle;
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::Storage::FileSystem::{
        CreateFileW, FlushFileBuffers, FILE_FLAG_NO_BUFFERING, FILE_FLAG_WRITE_THROUGH,
        FILE_GENERIC_READ, FILE_GENERIC_WRITE, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
    };
    use windows::Win32::System::Ioctl::{FSCTL_DISMOUNT_VOLUME, FSCTL_LOCK_VOLUME, FSCTL_UNLOCK_VOLUME};
    use windows::Win32::System::IO::DeviceIoControl;

    fn w(s: &str) -> Vec<u16> {
        std::ffi::OsStr::new(s).encode_wide().chain(Some(0)).collect()
    }

    fn open(path: &str, access: u32, flags: windows::Win32::Storage::FileSystem::FILE_FLAGS_AND_ATTRIBUTES) -> Result<HANDLE, String> {
        let p = w(path);
        unsafe {
            CreateFileW(
                PCWSTR(p.as_ptr()),
                access,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                None,
                OPEN_EXISTING,
                flags,
                None,
            )
        }
        .map_err(|e| {
            if e.code().0 as u32 == 0x8007_0005 {
                format!("{path}: access denied - SKJ Flash has to run as administrator to touch a raw disk")
            } else {
                format!("{path}: {e}")
            }
        })
    }

    pub fn open_read(dev: &Device) -> Result<File, String> {
        let h = open(&dev.path, FILE_GENERIC_READ.0, Default::default())?;
        Ok(unsafe { File::from_raw_handle(h.0 as _) })
    }

    pub fn open_write(dev: &Device) -> Result<File, String> {
        // write-through so a pulled stick does not lose what we were told was written
        let h = open(
            &dev.path,
            FILE_GENERIC_READ.0 | FILE_GENERIC_WRITE.0,
            FILE_FLAG_WRITE_THROUGH,
        )?;
        Ok(unsafe { File::from_raw_handle(h.0 as _) })
    }

    pub fn sync(f: &File) -> Result<(), String> {
        use std::os::windows::io::AsRawHandle;
        unsafe { FlushFileBuffers(HANDLE(f.as_raw_handle() as _)) }.map_err(|e| format!("flush: {e}"))
    }

    /// Locks and dismounts every volume on the disk, and lets go again on drop.
    ///
    /// Windows refuses writes to a region a mounted filesystem owns, so without this a write
    /// either fails or lands nowhere useful. Dismounting also stops Explorer writing its own
    /// files into the volume halfway through.
    pub struct VolumeLocks(Vec<HANDLE>);

    impl Drop for VolumeLocks {
        fn drop(&mut self) {
            for h in self.0.drain(..) {
                unsafe {
                    let mut ret = 0u32;
                    let _ = DeviceIoControl(h, FSCTL_UNLOCK_VOLUME, None, 0, None, 0, Some(&mut ret), None);
                    let _ = CloseHandle(h);
                }
            }
        }
    }

    pub fn lock_volumes(dev: &Device) -> Result<VolumeLocks, String> {
        let mut held = Vec::new();
        for m in &dev.mounts {
            let letter = m.trim_end_matches(':');
            let path = format!(r"\\.\{}:", letter);
            let h = match open(&path, FILE_GENERIC_READ.0 | FILE_GENERIC_WRITE.0, Default::default()) {
                Ok(h) => h,
                Err(e) => {
                    drop(VolumeLocks(held));
                    return Err(format!("Could not open {m} to lock it: {e}"));
                }
            };
            let mut ret = 0u32;
            let locked = unsafe { DeviceIoControl(h, FSCTL_LOCK_VOLUME, None, 0, None, 0, Some(&mut ret), None) };
            if locked.is_err() {
                unsafe { let _ = CloseHandle(h); }
                drop(VolumeLocks(held));
                return Err(format!(
                    "Could not lock {m}: something else is using it. Close any window or program open on that drive and try again."
                ));
            }
            let _ = unsafe { DeviceIoControl(h, FSCTL_DISMOUNT_VOLUME, None, 0, None, 0, Some(&mut ret), None) };
            held.push(h);
        }
        Ok(VolumeLocks(held))
    }

    /// Raw disk access needs administrator; saying so up front beats a confusing error later.
    pub fn is_elevated() -> bool {
        use windows::Win32::Security::{GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY};
        use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
        unsafe {
            let mut token = HANDLE::default();
            if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).is_err() {
                return false;
            }
            let mut el = TOKEN_ELEVATION::default();
            let mut sz = 0u32;
            let ok = GetTokenInformation(
                token,
                TokenElevation,
                Some(&mut el as *mut _ as *mut core::ffi::c_void),
                std::mem::size_of::<TOKEN_ELEVATION>() as u32,
                &mut sz,
            );
            let _ = CloseHandle(token);
            ok.is_ok() && el.TokenIsElevated != 0
        }
    }
}

#[cfg(not(windows))]
mod imp {
    use super::Device;
    use std::fs::File;

    pub struct VolumeLocks;

    pub fn open_read(_dev: &Device) -> Result<File, String> {
        Err("Writing is only implemented on Windows so far".into())
    }
    pub fn open_write(_dev: &Device) -> Result<File, String> {
        Err("Writing is only implemented on Windows so far".into())
    }
    pub fn lock_volumes(_dev: &Device) -> Result<VolumeLocks, String> {
        Err("Writing is only implemented on Windows so far".into())
    }
    pub fn sync(_f: &File) -> Result<(), String> {
        Ok(())
    }
    pub fn is_elevated() -> bool {
        // root on unix; checked properly when the unix writer lands
        false
    }
}

pub use imp::is_elevated;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pattern_is_the_length_asked_for() {
        assert_eq!(make_pattern(1000, 42).len(), 1000);
        assert_eq!(make_pattern(1024 * 1024, 1).len(), 1024 * 1024);
    }

    #[test]
    fn different_runs_write_different_bytes() {
        // so reading a stale copy of a previous test cannot pass for a fresh write
        assert_ne!(make_pattern(4096, 1), make_pattern(4096, 2));
    }

    #[test]
    fn the_same_seed_repeats() {
        assert_eq!(make_pattern(4096, 7), make_pattern(4096, 7));
    }

    #[test]
    fn a_pattern_does_not_look_like_a_blank_disk() {
        let p = make_pattern(65536, 99);
        assert!(p.iter().any(|&b| b != 0));
        assert!(p.iter().any(|&b| b != 0xFF));
    }

    #[test]
    fn progress_reports_a_percentage() {
        let p = Progress { done: 50, total: 200, speed: 0.0 };
        assert!((p.percent() - 25.0).abs() < f64::EPSILON);
        let empty = Progress { done: 0, total: 0, speed: 0.0 };
        assert_eq!(empty.percent(), 0.0);
    }
}
