//! SKJ Flash - make bootable USB drives on Windows, Linux and macOS.
//!
//! Right now this is a CLI around the part that matters: finding drives safely. The Slint UI
//! comes once the dangerous half is proven on real hardware, because a window makes it harder
//! to see what the code actually decided, not easier.
//!
//!     skj-flash list          drives it is willing to write to
//!     skj-flash list --all    every drive it can see, including internal ones, with warnings

mod disk;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = args.first().map(String::as_str).unwrap_or("list");
    let all = args.iter().any(|a| a == "--all" || a == "-a");

    match cmd {
        "list" => list(all),
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
    println!("  skj-flash list         drives it is willing to write to");
    println!("  skj-flash list --all   every drive, including internal ones\n");
    println!("Nothing here writes to a disk yet.");
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
