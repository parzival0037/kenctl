//! Minimal, single-feature prototypes.
//!
//! These exist so each capability can be validated on real hardware before the
//! rest of the application is trusted. `--dry-run` reads events without
//! grabbing the device and prints what would be emitted, so the mapping logic
//! can be checked before `/dev/uinput` has been granted.

use crate::devices::{self, FoundDevice};
use crate::engine::{self, TransformState};
use crate::profiles::{Action, Profile};
use anyhow::{bail, Context, Result};
use evdev::{Device, EventSummary, EventType, InputEvent, KeyCode};
use std::path::Path;
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

fn pick_device(requested: Option<&Path>) -> Result<FoundDevice> {
    let found = devices::scan();
    if found.is_empty() {
        bail!(
            "no Kensington input node found — is the trackball connected over USB \
             and do you have read access to /dev/input/event*? (user must be in the `input` group)"
        );
    }
    if let Some(path) = requested {
        return found
            .into_iter()
            .find(|d| d.path == path)
            .with_context(|| format!("{} is not a detected Kensington node", path.display()));
    }
    Ok(found.into_iter().next().expect("non-empty"))
}

fn describe(event: &InputEvent, pid: u16) -> String {
    match event.destructure() {
        EventSummary::Key(_, code, value) => {
            let raw = format!("{code:?}");
            let name = devices::button_label(pid, &raw)
                .map(str::to_string)
                .unwrap_or(raw);
            format!(
                "{name} {}",
                match value {
                    1 => "down",
                    0 => "up",
                    _ => "repeat",
                }
            )
        }
        EventSummary::RelativeAxis(_, code, value) => format!("{code:?} {value:+}"),
        EventSummary::Misc(_, code, value) => format!("{code:?} {value}"),
        EventSummary::Synchronization(_, code, _) => format!("{code:?}"),
        _ => "?".to_string(),
    }
}

/// Read a device without grabbing it and print the events as they arrive.
///
/// `transform` is applied so the same code path used by the engine is the one
/// being observed. An identity profile makes this a plain event viewer.
fn preview(
    mut device: Device,
    path: &Path,
    pid: u16,
    profile: Profile,
    label: &str,
    all: bool,
    show_output: bool,
) -> Result<()> {
    let name = device.name().unwrap_or("?").to_string();
    println!("{label}");
    println!("  device : {}", path.display());
    println!("  name   : {name}");
    println!("  mode   : preview (device is NOT grabbed; original input still works)");
    println!("  stop   : Ctrl+C");
    println!();

    let mut state = TransformState::default();
    let mut out: Vec<InputEvent> = Vec::new();
    let mut sources: Vec<String> = Vec::new();

    let keep = |line: &String| {
        if line.starts_with("MSC_") || line.starts_with("SYN_") {
            return false;
        }
        all || (!line.starts_with("REL_X") && !line.starts_with("REL_Y"))
    };

    loop {
        for event in device.fetch_events()? {
            if event.event_type() == EventType::SYNCHRONIZATION {
                if !out.is_empty() {
                    if show_output {
                        let mut rendered: Vec<String> =
                            out.iter().map(|e| describe(e, pid)).collect();
                        rendered.retain(|line| keep(line));
                        if !rendered.is_empty() {
                            println!("{:<46} -> {}", sources.join(", "), rendered.join(", "));
                        }
                    } else if !sources.is_empty() {
                        println!("{}", sources.join(", "));
                    }
                }
                out.clear();
                sources.clear();
                continue;
            }

            // Only key and relative events are meaningful for mapping.
            if matches!(
                event.event_type(),
                EventType::KEY | EventType::RELATIVE
            ) {
                sources.push(describe(&event, pid));
            }
            engine::transform(event, &profile, &mut state, &mut out);
        }
    }
}

/// Run the real grab + uinput pipeline for a single profile.
fn run_engine(profile: Profile, label: &str) -> Result<()> {
    if devices::scan().is_empty() {
        bail!("no Kensington input node found — connect the trackball first");
    }

    let vdev = Arc::new(Mutex::new(engine::create_virtual_device()?));
    engine::set_current_vdev(vdev.clone());

    let slot = Arc::new(RwLock::new(Arc::new(profile)));
    let managed: Arc<Mutex<Vec<FoundDevice>>> = Arc::new(Mutex::new(Vec::new()));
    engine::spawn_supervisor(slot, vdev, managed);

    println!("{label}");
    println!("  mode   : active (device is grabbed and re-injected through uinput)");
    println!("  stop   : Ctrl+C (also releases the grab)");
    println!();
    println!("Try it now. If input stops working, press Ctrl+C to restore the device.");
    println!();

    loop {
        std::thread::sleep(Duration::from_secs(3600));
    }
}

fn parse_key(name: &str, what: &str) -> Result<KeyCode> {
    name.parse::<KeyCode>()
        .map_err(|_| anyhow::anyhow!("{what} `{name}` is not a valid Linux key/button name (e.g. BTN_SIDE, KEY_C)"))
}

/// Feature 1 prototype: remap one trackball button to a key (optionally held
/// with modifiers), or to another mouse button.
pub fn button(
    from: &str,
    to: &str,
    modifiers: &[String],
    device: Option<&Path>,
    dry_run: bool,
) -> Result<()> {
    parse_key(from, "source button")?;
    parse_key(to, "target")?;
    for m in modifiers {
        parse_key(m, "modifier")?;
    }

    let mut profile = Profile::default();
    profile.name = "prototype-button".into();
    profile.buttons.insert(
        from.to_string(),
        Action::Key {
            code: to.to_string(),
            modifiers: modifiers.to_vec(),
        },
    );

    let label = if modifiers.is_empty() {
        format!("Prototype: {from} -> {to}")
    } else {
        format!("Prototype: {from} -> {}+{to}", modifiers.join("+"))
    };

    if dry_run {
        let target = pick_device(device)?;
        let dev = Device::open(&target.path)
            .with_context(|| format!("opening {}", target.path.display()))?;
        return preview(dev, &target.path, target.product_id, profile, &label, false, true);
    }

    run_engine(profile, &label)
}

/// Feature 2 prototype: scale (and optionally invert) scroll deltas.
pub fn scroll(speed: f64, invert: bool, device: Option<&Path>, dry_run: bool) -> Result<()> {
    if !speed.is_finite() || speed <= 0.0 {
        bail!("scroll speed must be a positive number");
    }

    let mut profile = Profile::default();
    profile.name = "prototype-scroll".into();
    profile.scroll_speed = speed;
    profile.invert_scroll = invert;

    let label = format!(
        "Prototype: scroll speed {speed:.2}x{}",
        if invert { ", inverted" } else { "" }
    );

    if dry_run {
        let target = pick_device(device)?;
        let dev = Device::open(&target.path)
            .with_context(|| format!("opening {}", target.path.display()))?;
        return preview(dev, &target.path, target.product_id, profile, &label, false, true);
    }

    run_engine(profile, &label)
}

/// Plain live event viewer used to discover the source button names for a
/// mapping (`BTN_SIDE`, `BTN_EXTRA`, ...).
pub fn listen(device: Option<&Path>, all: bool) -> Result<()> {
    let target = pick_device(device)?;
    let dev = Device::open(&target.path)
        .with_context(|| format!("opening {}", target.path.display()))?;
    preview(dev, &target.path, target.product_id, Profile::default(), "Live event viewer (identity)", all, false)
}
