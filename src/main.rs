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
mod iso;
mod multiboot;
mod gui;
mod write;

use std::io::Write as _;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    // No arguments means the window; the CLI is there for scripting and for testing the
    // dangerous half without a UI in the way.
    if args.is_empty() {
        if let Err(e) = gui::run() {
            eprintln!("Could not open the window: {e}");
            std::process::exit(1);
        }
        return;
    }
    let cmd = args.first().map(String::as_str).unwrap_or("list");
    let all = args.iter().any(|a| a == "--all" || a == "-a");

    match cmd {
        "list" => list(all),
        "selftest" => selftest(&args),
        "write" => write_iso(&args),
        "multiboot" => multiboot_cmd(&args),
        "inspect" => inspect_cmd(&args),
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
    println!("                            prove writing works, then restore the drive");
    println!("  skj-flash write <device> <image.iso>");
    println!("                            write an image, ERASING everything on the drive");
    println!("  skj-flash multiboot <device>");
    println!("                            lay the drive out for many ISOs, ERASING it\n");
    println!("multiboot makes a drive you copy .iso files onto and pick from at boot, instead");
    println!("of one image filling the whole stick.\n");
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
            println!("  tested over           {}", disk::human_size(r.bytes));
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

fn write_iso(args: &[String]) {
    let (Some(target), Some(image)) = (args.get(1), args.get(2)) else {
        eprintln!("Usage: skj-flash write <device> <image.iso>");
        std::process::exit(2);
    };
    let image = std::path::Path::new(image);
    if !image.is_file() {
        eprintln!("{} is not a file", image.display());
        std::process::exit(2);
    }

    if !write::is_elevated() {
        eprintln!("SKJ Flash needs to run as administrator to touch a raw disk.");
        std::process::exit(1);
    }

    let Some(dev) = disk::list(false).into_iter().find(|d| d.path.eq_ignore_ascii_case(target)) else {
        eprintln!("{target} is not one of the drives SKJ Flash is willing to write to.");
        eprintln!("Run `skj-flash list` to see which are.");
        std::process::exit(2);
    };

    // This one really does erase the drive, so it says so and makes the person type the size
    // back. A y/n is too easy to answer without reading; repeating "58.6 GB" is not.
    // Writing a plain CD image byte for byte copies it perfectly and boots nothing. Say so
    // before the write, not after.
    match iso::inspect(image) {
        Ok(info) => {
            if let Some(w) = info.warning() {
                println!("
  !  {w}
");
            }
        }
        Err(e) => println!("
  !  Could not read the image to check it: {e}
"),
    }

    let size = disk::human_size(dev.size);
    println!("About to ERASE this drive and write {}:\n", image.display());
    println!("    {dev}\n");
    if !dev.mounts.is_empty() {
        println!("  Everything on {} will be lost.\n", dev.mounts.join(", "));
    }
    print!("Type the drive's size exactly ({size}) to go ahead, or anything else to stop: ");
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    if std::io::stdin().read_line(&mut line).is_err() || line.trim() != size {
        println!("Stopped. Nothing was written.");
        return;
    }

    println!();
    let mut last = String::new();
    let res = write::write_image(&dev, image, |p| {
        let line = format!(
            "{:>5.1}%  {} of {}  {:.1} MB/s",
            p.percent(),
            disk::human_size(p.done),
            disk::human_size(p.total),
            p.speed / 1_048_576.0
        );
        if line != last {
            print!("\r{line}   ");
            let _ = std::io::stdout().flush();
            last = line;
        }
    });
    println!();

    match res {
        Ok(()) => println!("\nDone. {} written to {}.", image.display(), dev.path),
        Err(e) => {
            eprintln!("\n{e}");
            std::process::exit(1);
        }
    }
}

fn multiboot_cmd(args: &[String]) {
    let Some(target) = args.get(1) else {
        eprintln!("Usage: skj-flash multiboot <device>");
        std::process::exit(2);
    };
    if !write::is_elevated() {
        eprintln!("SKJ Flash needs to run as administrator to repartition a drive.");
        std::process::exit(1);
    }
    let Some(dev) = disk::list(false).into_iter().find(|d| d.path.eq_ignore_ascii_case(target)) else {
        eprintln!("{target} is not one of the drives SKJ Flash is willing to write to.");
        std::process::exit(2);
    };

    let size = disk::human_size(dev.size);
    println!("About to ERASE this drive and lay it out for multiboot:
");
    println!("    {dev}
");
    print!("Type the drive's size exactly ({size}) to go ahead, or anything else to stop: ");
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    if std::io::stdin().read_line(&mut line).is_err() || line.trim() != size {
        println!("Stopped. Nothing was written.");
        return;
    }

    println!();
    match multiboot::prepare(&dev, |m| println!("  {m}")) {
        Ok(()) => {
            println!("
Copy .iso files onto the {} drive and they appear in the boot menu.", multiboot::DATA_LABEL);
            println!("
{}", multiboot::BOOTLOADER_NOTE);
        }
        Err(e) => {
            eprintln!("
{e}");
            std::process::exit(1);
        }
    }
}

fn inspect_cmd(args: &[String]) {
    let Some(image) = args.get(1) else {
        eprintln!("Usage: skj-flash inspect <image.iso>");
        std::process::exit(2);
    };
    match iso::inspect(std::path::Path::new(image)) {
        Ok(i) => {
            println!("{image}");
            println!("  size       {}", disk::human_size(i.size));
            println!("  boots as   {:?}{}", i.boot, if i.gpt { " (+GPT)" } else { "" });
            match i.warning() {
                None => println!("
  Writing this to a drive will boot."),
                Some(w) => println!("
  {w}"),
            }
        }
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    }
}
