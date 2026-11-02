# SKJ Flash

Make bootable USB drives on Windows, Linux and macOS. Part of the
[SKJ App Pack](https://github.com/jakub362/skj-files).

Rufus is Windows-only, and deeply so — its disk access, partitioning and filesystem creation are
all Windows APIs. Making that work on Linux and macOS is a rewrite rather than a port, so SKJ Flash
is written fresh in Rust, which also means it shares the SKJ App Pack's UI, build and release setup.

**Status: early.** Drive detection and raw writing work on Windows and are verified on real
hardware. There is no GUI yet, Linux and macOS are not implemented, and Ventoy-style multiboot and
Windows To Go are still to come.

## Using it

Raw disk access needs administrator (Windows) or root (Linux, macOS).

```
skj-flash list                        drives it is willing to write to
skj-flash list --all                  every drive it can see, with warnings
skj-flash selftest <device> [MB]      prove writing works, then restore the drive
skj-flash write <device> <image.iso>  write an image, ERASING the drive
```

`selftest` is the safe way to check a drive and the program before trusting either: it backs up the
first few MB, writes a pattern over them, reads it back to confirm it really landed, then writes
the original bytes back and checks those too. If it finishes, the drive is exactly as it was.

## Not hurting you

A tool that writes to raw block devices can destroy a computer, so the rules are deliberate:

- **A drive is offered only when it is both removable and on a USB or SD bus.** Either alone is not
  enough — a USB-attached SSD reports "not removable", and an empty internal card reader reports
  "removable". Requiring both keeps the default list to things that really are memory sticks.
- **The disk the system runs from is never offered**, whatever flags are passed. It is found through
  the volume holding the Windows directory, not by guessing at drive numbers.
- **Detection opens its handles with no access rights at all**, so that half of the program cannot
  write and does not need administrator.
- **Writing only accepts a device that came back from detection**, never a path typed by hand, so a
  typo cannot reach a disk that was never inspected.
- **`write` makes you type the drive's size back** rather than answering a yes/no prompt, because a
  yes/no is too easy to answer without reading.

## Still to do

- Slint GUI in the SKJ theme
- Linux and macOS drive detection and writing
- Ventoy-style multiboot: one stick, drop ISOs on, boot any of them
- Windows To Go
- Verify-after-write for full images (selftest already verifies)

## Licence

GPL-3.0-only, matching the rest of the SKJ App Pack.
