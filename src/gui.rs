//! The window. All the deciding lives in `disk` and `write`; this only shows it and reports back.
//!
//! Long jobs run on a worker thread and report through `invoke_from_event_loop`, so the window
//! keeps drawing while a drive is being written - a frozen window during a disk write is how
//! people end up pulling the stick.

use crate::disk::{self, Device};
use crate::multiboot;
use crate::write;
use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel, Weak};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

slint::include_modules!();

/// Idle / working / finished / failed, matching `S.state` in the .slint.
#[allow(dead_code)]
const IDLE: i32 = 0;
const WORKING: i32 = 1;
const DONE: i32 = 2;
const FAILED: i32 = 3;

pub fn run() -> Result<(), slint::PlatformError> {
    let ui = AppWindow::new()?;
    let devices: Rc<std::cell::RefCell<Vec<Device>>> = Rc::new(std::cell::RefCell::new(Vec::new()));
    let cancel = Arc::new(AtomicBool::new(false));

    ui.global::<S>().set_elevated(write::is_elevated());
    refresh(&ui, &devices);

    {
        let (w, d) = (ui.as_weak(), devices.clone());
        ui.global::<S>().on_refresh(move || {
            if let Some(ui) = w.upgrade() {
                refresh(&ui, &d);
            }
        });
    }

    {
        let w = ui.as_weak();
        ui.global::<S>().on_pick_image(move || {
            // rfd opens the system's own dialog, so it looks like every other file picker
            if let Some(p) = rfd::FileDialog::new()
                .add_filter("Disk images", &["iso", "img", "bin", "raw"])
                .add_filter("Every file", &["*"])
                .pick_file()
            {
                if let Some(ui) = w.upgrade() {
                    ui.global::<S>().set_image_path(SharedString::from(p.display().to_string()));
                }
            }
        });
    }

    {
        let (w, d, c) = (ui.as_weak(), devices.clone(), cancel.clone());
        ui.global::<S>().on_selftest(move || {
            let Some(dev) = selected(&w, &d) else { return };
            c.store(false, Ordering::SeqCst);
            start(&w, format!("Self-testing {}…", dev.model));
            let (w2, c2) = (w.clone(), c.clone());
            std::thread::spawn(move || {
                let r = write::self_test(&dev, 8 * 1024 * 1024, |p| {
                    report(&w2, (p.percent() / 100.0) as f32, None);
                    if c2.load(Ordering::SeqCst) {
                        // the write loop checks nothing yet; cancelling only stops the next job
                    }
                });
                let (ok, msg) = match r {
                    Ok(r) if r.wrote_ok && r.restored_ok => (
                        true,
                        format!(
                            "Self-test passed. Wrote {:.1} MB/s, read {:.1} MB/s. The drive is exactly as it was.",
                            r.write_speed / 1_048_576.0,
                            r.read_speed / 1_048_576.0
                        ),
                    ),
                    Ok(r) => (
                        false,
                        format!(
                            "Self-test failed: wrote {}, restored {}. The drive may need reformatting.",
                            if r.wrote_ok { "OK" } else { "MISMATCH" },
                            if r.restored_ok { "OK" } else { "MISMATCH" }
                        ),
                    ),
                    Err(e) => (false, e),
                };
                finish(&w2, ok, msg);
            });
        });
    }

    {
        let (w, d, c) = (ui.as_weak(), devices.clone(), cancel.clone());
        ui.global::<S>().on_start_write(move || {
            let Some(dev) = selected(&w, &d) else { return };
            let Some(ui) = w.upgrade() else { return };
            let image = ui.global::<S>().get_image_path().to_string();
            if image.is_empty() {
                return;
            }
            c.store(false, Ordering::SeqCst);
            start(&w, format!("Writing to {}…", dev.model));
            let w2 = w.clone();
            std::thread::spawn(move || {
                let path = std::path::PathBuf::from(&image);
                let r = write::write_image(&dev, &path, |p| {
                    report(
                        &w2,
                        (p.percent() / 100.0) as f32,
                        Some(format!(
                            "{} of {}  ·  {:.1} MB/s",
                            disk::human_size(p.done),
                            disk::human_size(p.total),
                            p.speed / 1_048_576.0
                        )),
                    );
                });
                match r {
                    Ok(()) => finish(&w2, true, "Done. The drive is ready.".into()),
                    Err(e) => finish(&w2, false, e),
                }
            });
        });
    }

    {
        let c = cancel.clone();
        ui.global::<S>().on_cancel(move || c.store(true, Ordering::SeqCst));
    }

    {
        let (w, d) = (ui.as_weak(), devices.clone());
        ui.global::<S>().on_multiboot(move || {
            let Some(dev) = selected(&w, &d) else { return };
            start(&w, format!("Preparing {} for multiboot…", dev.model));
            let w2 = w.clone();
            std::thread::spawn(move || {
                let w3 = w2.clone();
                let r = multiboot::prepare(&dev, move |m| {
                    report(&w3, 0.5, Some(m.to_string()));
                });
                match r {
                    Ok(()) => finish(
                        &w2,
                        true,
                        format!(
                            "Ready. Copy .iso files onto the {} drive. {}",
                            multiboot::DATA_LABEL,
                            multiboot::BOOTLOADER_NOTE.replace('\n', " ")
                        ),
                    ),
                    Err(e) => finish(&w2, false, e),
                }
            });
        });
    }

    {
        let w = ui.as_weak();
        ui.global::<S>().on_elevate(move || match write::relaunch_elevated() {
            // the elevated copy takes over; this one has nothing left to do
            Ok(()) => { let _ = slint::quit_event_loop(); }
            Err(e) => {
                if let Some(ui) = w.upgrade() {
                    let g = ui.global::<S>();
                    g.set_state(FAILED);
                    g.set_status(SharedString::from(e));
                }
            }
        });
    }

    ui.run()
}

fn selected(w: &Weak<AppWindow>, devices: &Rc<std::cell::RefCell<Vec<Device>>>) -> Option<Device> {
    let ui = w.upgrade()?;
    let i = ui.global::<S>().get_selected();
    if i < 0 {
        return None;
    }
    devices.borrow().get(i as usize).cloned()
}

fn refresh(ui: &AppWindow, devices: &Rc<std::cell::RefCell<Vec<Device>>>) {
    let found = disk::list(false);
    let rows: Vec<Drive> = found
        .iter()
        .map(|d| Drive {
            path: SharedString::from(d.path.clone()),
            label: SharedString::from(d.model.clone()),
            size: SharedString::from(disk::human_size(d.size)),
            mounts: SharedString::from(d.mounts.join(", ")),
            warn: !d.mounts.is_empty(),
        })
        .collect();
    let model = Rc::new(VecModel::from(rows));
    ui.global::<S>().set_drives(ModelRc::from(model.clone()));

    // keep the selection only if it still points at the same drive
    let g = ui.global::<S>();
    let old = g.get_selected();
    if old < 0 || old as usize >= model.row_count() {
        g.set_selected(if model.row_count() == 1 { 0 } else { -1 });
    }
    *devices.borrow_mut() = found;
    sync_confirm(ui, devices);
}

/// The text the confirmation makes the person type: the drive's own size.
fn sync_confirm(ui: &AppWindow, devices: &Rc<std::cell::RefCell<Vec<Device>>>) {
    let g = ui.global::<S>();
    let i = g.get_selected();
    let want = devices
        .borrow()
        .get(i.max(0) as usize)
        .map(|d| disk::human_size(d.size))
        .unwrap_or_default();
    g.set_confirm_want(SharedString::from(want));
}

fn start(w: &Weak<AppWindow>, status: String) {
    if let Some(ui) = w.upgrade() {
        let g = ui.global::<S>();
        g.set_state(WORKING);
        g.set_progress(0.0);
        g.set_status(SharedString::from(status));
        g.set_detail(SharedString::default());
    }
}

fn report(w: &Weak<AppWindow>, fraction: f32, detail: Option<String>) {
    let w = w.clone();
    let _ = slint::invoke_from_event_loop(move || {
        if let Some(ui) = w.upgrade() {
            let g = ui.global::<S>();
            g.set_progress(fraction);
            if let Some(d) = detail {
                g.set_detail(SharedString::from(d));
            }
        }
    });
}

fn finish(w: &Weak<AppWindow>, ok: bool, msg: String) {
    let w = w.clone();
    let _ = slint::invoke_from_event_loop(move || {
        if let Some(ui) = w.upgrade() {
            let g = ui.global::<S>();
            g.set_state(if ok { DONE } else { FAILED });
            g.set_progress(if ok { 1.0 } else { g.get_progress() });
            g.set_status(SharedString::from(msg));
            g.set_detail(SharedString::default());
        }
    });
}
