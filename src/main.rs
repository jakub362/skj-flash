//! SKJ Flash - make bootable USB drives on Windows, Linux and macOS.
//!
//! Right now this is a CLI around the parts that matter: finding drives safely, and writing to
//! them. The Slint UI comes once the dangerous half is proven on real hardware, because a window
//! makes it harder to see what the code actually decided, not easier.
//!
//!     skj-flash list              drives it is willing to write to
//!     skj-flash list --all        every drive it can see, with warnings
//!     skj-flash selftest <dev>    prove writing works, then put the drive back as it was

mod disk;
mod write;

use std::io::Write as _;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = args.first().map(String::as_str).unwrap_or("list");
    let all = args.iter().any(|a| a == "--all" || a == "-a");

    match cmd {
        "list" => list(all),
        "selftest" => selftest(&args),
        "help" | "--help" | "-h" => help(),
        other => {
            eprintln!("Unknown command: {other}\n");
            help();
            std::process::exit(2);
        }
    }
}

fn help() {
    println!("SKJ Flash {}\n", env!("CARGO_PKG_VERSION"));
    println!("  skj-flash list            drives it is willing to write to");
    println!("  skj-flash list --all      every drive, including internal ones");
    println!("  skj-flash selftest <device> [MB]");
    println!("                            prove writing works, then restore the drive\n");
    println!("selftest backs up the first few MB, writes a pattern, reads it back to check it");
    println!("really landed, then writes the original bytes back and checks those too. It needs");
    println!("administrator, and if it finishes it leaves the drive exactly as it was.");
}

fn list(all: bool) {
    let drives = disk::list(all);

    if drives.is_empty() {
        // Say which of the three reasons it actually is, rather than guessing at the scariest.
        let seen = disk::count_seen();
        if seen == 0 {
            println!("No drives could be opened at all.");
            println!("On Windows that usually means SKJ Flash needs to run as administrator.");
        } else if all {
            println!("Nothing to offer: all {seen} drive(s) found are the disk the system runs from.");
            println!("That one is never writable, whatever flags you pass.");
        } else {
            println!("No removable drives found, out of {seen} drive(s) seen.");
            println!("Plug a USB stick in, or pass --all to include internal drives.");
        }
        return;
    }

    println!("{:<22} {:>10}  {}", "DEVICE", "SIZE", "WHAT IT IS");
    for d in &drives {
        let mut notes = Vec::new();
        if d.system {
            notes.push("SYSTEM DISK");
        }
        if !d.removable {
            notes.push("not removable");
        }
        if !d.external_bus {
            notes.push("internal bus");
        }
        if !d.mounts.is_empty() {
            notes.push("mounted");
        }
        let tail = if notes.is_empty() { String::new() } else { format!("  [{}]", notes.join(", ")) };
        println!("{:<22} {:>10}  {}{}", d.path, disk::human_size(d.size), d.describe(), tail);
    }

    if all {
        println!("\nDrives without [not removable] / [internal bus] are the ones SKJ Flash offers by default.");
        println!("The disk the system runs from is never offered, whatever flags are passed.");
    }
}

fn selftest(args: &[String]) {
    let Some(target) = args.get(1) else {
        eprintln!("Which drive? Run `skj-flash list` first, then pass the device path.");
        std::process::exit(2);
    };
    let mb: u64 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(8);

    if !write::is_elevated() {
        eprintln!("SKJ Flash needs to run as administrator to touch a raw disk.");
        std::process::exit(1);
    }

    // Only a device that came out of the safety rules can be chosen - never a typed path.
    let Some(dev) = disk::list(false).into_iter().find(|d| d.path.eq_ignore_ascii_case(target)) else {
        eprintln!("{target} is not one of the drives SKJ Flash is willing to write to.");
        eprintln!("Run `skj-flash list` to see which are.");
        std::process::exit(2);
    };

    println!("Self-test on {dev}");
    println!("{mb} MB, backed up first and restored afterwards.\n");

    let mut last = String::new();
    let res = write::self_test(&dev, mb * 1024 * 1024, |p| {
        let line = format!("{:>5.1}%  {:.1} MB/s", p.percent(), p.speed / 1_048_576.0);
        if line != last {
            print!("\r{line}   ");
            let _ = std::io::stdout().flush();
            last = line;
        }
    });
    println!();

    match res {
        Ok(r) => {
            println!("\n  wrote and read back   {}", if r.wrote_ok { "OK" } else { "MISMATCH" });
            println!("  original restored     {}", if r.restored_ok { "OK" } else { "MISMATCH" });
            println!(
                "  write {:.1} MB/s, read {:.1} MB/s",
                r.write_speed / 1_048_576.0,
                r.read_speed / 1_048_576.0
            );
            if r.wrote_ok && r.restored_ok {
                println!("\nThe drive is exactly as it was.");
            } else {
                println!("\nSomething did not match - the drive may need reformatting.");
                std::process::exit(1);
            }
        }
        Err(e) => {
            eprintln!("\n{e}");
            std::process::exit(1);
        }
    }
}
