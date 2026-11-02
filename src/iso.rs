//! Looking inside a disk image before writing it.
//!
//! Writing an image to a USB stick byte for byte only produces something bootable if the image
//! was built to be written that way. A *hybrid* ISO carries an MBR and a partition table in its
//! very first sector, put there by `isohybrid` or `xorriso -isohybrid-mbr`, so firmware treats
//! the stick as a disk. Ubuntu, Fedora and Debian all ship hybrid images.
//!
//! A plain ISO 9660 image has none of that - its first 32 KB is the "system area" and is usually
//! all zeros. It boots from a CD through El Torito, which firmware only honours for optical
//! media. Copy it to a stick and you get a faithful copy of something that cannot boot.
//!
//! This is the difference between `dd` and Rufus: Rufus notices, and for those images extracts
//! the files onto a real partition and installs a bootloader instead. SKJ Flash does not do that
//! yet, so for now it says so rather than handing back a stick that silently does nothing.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

/// How an image expects to be booted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Boot {
    /// Has an MBR and partition table: writing it to a stick boots.
    Hybrid,
    /// El Torito only. Boots from a CD; a raw copy to USB will not.
    CdOnly,
    /// Neither - not a bootable image at all, as far as we can tell.
    None,
}

#[derive(Clone, Debug)]
pub struct Info {
    pub boot: Boot,
    /// A GPT as well as the MBR, which some firmware prefers
    pub gpt: bool,
    pub size: u64,
}

impl Info {
    /// Whether writing this image to a drive produces something that boots.
    pub fn writable_bootable(&self) -> bool {
        self.boot == Boot::Hybrid
    }

    /// What to tell the person before they wait for a write that cannot work.
    pub fn warning(&self) -> Option<String> {
        match self.boot {
            Boot::Hybrid => None,
            Boot::CdOnly => Some(
                "This image is a plain CD image: it has no partition table, so writing it to a \
                 drive copies it faithfully but the result will not boot. It boots from a CD, or \
                 from a multiboot drive, where GRUB loads it as a file."
                    .into(),
            ),
            Boot::None => Some(
                "This image has no boot record of any kind, so writing it will not produce a \
                 bootable drive."
                    .into(),
            ),
        }
    }
}

/// Read just enough of the image to tell how it boots. Cheap: two small reads.
pub fn inspect(path: &Path) -> Result<Info, String> {
    let mut f = File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let size = f.metadata().map_err(|e| format!("{}: {e}", path.display()))?.len();

    let mut mbr = [0u8; 512];
    f.read_exact(&mut mbr).map_err(|e| format!("{}: {e}", path.display()))?;

    // An MBR ends with 0x55 0xAA and has at least one partition entry with a non-zero type.
    let signature = mbr[510] == 0x55 && mbr[511] == 0xAA;
    let has_partition = (0..4).any(|i| mbr[0x1BE + i * 16 + 4] != 0);
    let hybrid = signature && has_partition;

    // A protective or real GPT announces itself at the start of sector 1.
    let mut gpt_hdr = [0u8; 8];
    let gpt = f.seek(SeekFrom::Start(512)).is_ok()
        && f.read_exact(&mut gpt_hdr).is_ok()
        && &gpt_hdr == b"EFI PART";

    // El Torito lives in the boot record volume descriptor, sector 17 of an ISO 9660 image.
    let mut vd = [0u8; 2048];
    let el_torito = f.seek(SeekFrom::Start(17 * 2048)).is_ok()
        && f.read_exact(&mut vd).is_ok()
        && vd.starts_with(&[0x00])
        && &vd[1..6] == b"CD001"
        && vd[7..30].starts_with(b"EL TORITO SPECIFICATION");

    let boot = if hybrid {
        Boot::Hybrid
    } else if el_torito {
        Boot::CdOnly
    } else {
        Boot::None
    };

    Ok(Info { boot, gpt, size })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn image(mbr: [u8; 512], el_torito: bool) -> (tempdir::Guard, std::path::PathBuf) {
        let g = tempdir::Guard::new();
        let p = g.dir.join("test.iso");
        let mut f = File::create(&p).unwrap();
        f.write_all(&mbr).unwrap();
        // pad out to sector 17, then the boot record volume descriptor
        let mut rest = vec![0u8; 17 * 2048 - 512];
        f.write_all(&rest).unwrap();
        rest.clear();
        let mut vd = vec![0u8; 2048];
        if el_torito {
            vd[0] = 0x00;
            vd[1..6].copy_from_slice(b"CD001");
            vd[6] = 0x01;
            vd[7..7 + 23].copy_from_slice(b"EL TORITO SPECIFICATION");
        }
        f.write_all(&vd).unwrap();
        (g, p)
    }

    fn hybrid_mbr() -> [u8; 512] {
        let mut m = [0u8; 512];
        m[510] = 0x55;
        m[511] = 0xAA;
        m[0x1BE + 4] = 0x17; // a partition type, any non-zero one
        m
    }

    #[test]
    fn a_hybrid_image_is_safe_to_write() {
        let (_g, p) = image(hybrid_mbr(), true);
        let i = inspect(&p).unwrap();
        assert_eq!(i.boot, Boot::Hybrid);
        assert!(i.writable_bootable());
        assert!(i.warning().is_none());
    }

    #[test]
    fn a_plain_cd_image_is_flagged() {
        // exactly the CorePure64 case: zeros in sector 0, El Torito further in
        let (_g, p) = image([0u8; 512], true);
        let i = inspect(&p).unwrap();
        assert_eq!(i.boot, Boot::CdOnly);
        assert!(!i.writable_bootable());
        assert!(i.warning().unwrap().contains("will not boot"));
    }

    #[test]
    fn a_signature_without_a_partition_is_not_hybrid() {
        // some images end sector 0 with 0x55AA but have no partition table; writing them
        // still does not boot, so the partition entry is what actually matters
        let mut m = [0u8; 512];
        m[510] = 0x55;
        m[511] = 0xAA;
        let (_g, p) = image(m, true);
        assert_eq!(inspect(&p).unwrap().boot, Boot::CdOnly);
    }

    #[test]
    fn something_that_is_not_an_image_at_all() {
        let (_g, p) = image([0u8; 512], false);
        let i = inspect(&p).unwrap();
        assert_eq!(i.boot, Boot::None);
        assert!(i.warning().unwrap().contains("no boot record"));
    }

    #[test]
    fn a_missing_file_is_an_error_not_a_panic() {
        assert!(inspect(Path::new("no-such-image-anywhere.iso")).is_err());
    }

    /// A scratch directory that cleans itself up, so the tests leave nothing behind.
    mod tempdir {
        pub struct Guard {
            pub dir: std::path::PathBuf,
        }
        impl Guard {
            pub fn new() -> Self {
                let dir = std::env::temp_dir().join(format!(
                    "skj-flash-test-{}-{:?}",
                    std::process::id(),
                    std::thread::current().id()
                ));
                std::fs::create_dir_all(&dir).unwrap();
                Guard { dir }
            }
        }
        impl Drop for Guard {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.dir);
            }
        }
    }
}
