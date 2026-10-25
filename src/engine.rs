use crate::devices::{self, FoundDevice};
use crate::profiles::{Action, Macro, MacroAction, MacroStep, Profile};
use crate::VIRTUAL_DEVICE_NAME;
use anyhow::{Context, Result};
use evdev::uinput::VirtualDevice;
use evdev::{
    AttributeSet, BusType, Device, EventSummary, EventType, InputEvent, InputId, KeyCode, MiscCode,
    RelativeAxisCode,
};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

/// Linux `KEY_MAX`.
const KEY_MAX: u16 = 0x2ff;
/// Linux `REL_MAX`.
const REL_MAX: u16 = 0x0f;

/// Lock without propagating poisoning: a panicked helper thread must not take
/// the whole input pipeline down with it.
fn lock_unpoison<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Create the single virtual input device every source node is funnelled into.
///
/// It advertises the full key range plus all relative axes so a mapping can
/// turn a mouse button into an arbitrary key, media key or combination.
pub fn create_virtual_device() -> Result<VirtualDevice> {
    let mut keys = AttributeSet::<KeyCode>::new();
    for i in 0..=KEY_MAX {
        keys.insert(KeyCode(i));
    }
    let mut axes = AttributeSet::<RelativeAxisCode>::new();
    for i in 0..=REL_MAX {
        axes.insert(RelativeAxisCode(i));
    }
    let mut misc = AttributeSet::<MiscCode>::new();
    misc.insert(MiscCode::MSC_SCAN);

    let device = evdev::uinput::VirtualDevice::builder()
        .context(
            "cannot open /dev/uinput — run ./install.sh (as root) to create the udev rule, \
             then log out and back in",
        )?
        .name(VIRTUAL_DEVICE_NAME)
        // Distinct from KENSINGTON_VID so discovery can never match our own
        // output and the engine cannot grab (and stall) itself.
        .input_id(InputId::new(BusType::BUS_VIRTUAL, 0x0001, 0x0001, 0x0001))
        .with_keys(&keys)?
        .with_relative_axes(&axes)?
        .with_msc(&misc)?
        .build()?;

    tracing::info!("created virtual device: {VIRTUAL_DEVICE_NAME}");
    Ok(device)
}

/// Everything a single device thread needs to transform events.
#[derive(Default)]
pub(crate) struct TransformState {
    /// Fractional remainders so sub-1.0 multipliers do not quantise motion
    /// into zero on every other event.
    x_acc: f64,
    y_acc: f64,
    wheel_acc: f64,
    hwheel_acc: f64,
    wheel_hi_acc: f64,
    hwheel_hi_acc: f64,

    /// Source button -> keys that button is currently holding on our behalf.
    held: HashMap<u16, Held>,
    /// Reference counts so shared modifiers are released only once.
    mod_refs: HashMap<KeyCode, u32>,
}

#[derive(Clone)]
struct Held {
    modifiers: Vec<KeyCode>,
    target: KeyCode,
}

impl TransformState {
    fn press(&mut self, source: u16, modifiers: &[KeyCode], target: KeyCode, out: &mut Vec<InputEvent>) {
        for m in modifiers {
            let refs = self.mod_refs.entry(*m).or_insert(0);
            *refs += 1;
            if *refs == 1 {
                out.push(key_event(*m, 1));
            }
        }
        out.push(key_event(target, 1));
        self.held.insert(
            source,
            Held {
                modifiers: modifiers.to_vec(),
                target,
            },
        );
    }

    fn release(&mut self, source: u16, out: &mut Vec<InputEvent>) {
        if let Some(held) = self.held.remove(&source) {
            out.push(key_event(held.target, 0));
            for m in held.modifiers.iter().rev() {
                if let Some(refs) = self.mod_refs.get_mut(m) {
                    *refs = refs.saturating_sub(1);
                    if *refs == 0 {
                        self.mod_refs.remove(m);
                        out.push(key_event(*m, 0));
                    }
                }
            }
        }
    }
}

fn key_event(code: KeyCode, value: i32) -> InputEvent {
    InputEvent::new_now(EventType::KEY.0, code.code(), value)
}

/// Multiply a relative delta, carrying the fraction so fractional speeds feel
/// smooth rather than steppy. Sign inversion is applied before accumulation.
fn scale(acc: &mut f64, value: i32, multiplier: f64, invert: bool) -> i32 {
    let mut delta = value as f64 * multiplier;
    if invert {
        delta = -delta;
    }
    *acc += delta;
    let whole = acc.trunc();
    *acc -= whole;
    whole as i32
}

/// Transform one source event into zero or more output events.
pub(crate) fn transform(
    event: InputEvent,
    profile: &Profile,
    state: &mut TransformState,
    out: &mut Vec<InputEvent>,
) {
    match event.destructure() {
        EventSummary::RelativeAxis(_, code, value) => {
            let (acc, multiplier, invert) = match code {
                RelativeAxisCode::REL_X => (&mut state.x_acc, profile.pointer_speed, profile.invert_pointer_x),
                RelativeAxisCode::REL_Y => (&mut state.y_acc, profile.pointer_speed, profile.invert_pointer_y),
                RelativeAxisCode::REL_WHEEL => (&mut state.wheel_acc, profile.scroll_speed, profile.invert_scroll),
                RelativeAxisCode::REL_WHEEL_HI_RES => {
                    (&mut state.wheel_hi_acc, profile.scroll_speed, profile.invert_scroll)
                }
                RelativeAxisCode::REL_HWHEEL => {
                    (&mut state.hwheel_acc, profile.scroll_speed, profile.invert_scroll)
                }
                RelativeAxisCode::REL_HWHEEL_HI_RES => {
                    (&mut state.hwheel_hi_acc, profile.scroll_speed, profile.invert_scroll)
                }
                // Anything else (e.g. dial on other models) passes through.
                other => {
                    out.push(InputEvent::new_now(EventType::RELATIVE.0, other.0, value));
                    return;
                }
            };
            let scaled = scale(acc, value, multiplier, invert);
            if scaled != 0 {
                out.push(InputEvent::new_now(EventType::RELATIVE.0, code.0, scaled));
            }
        }

        EventSummary::Key(_, code, value) => {
            let source_name = format!("{code:?}");
            match profile.action_for(&source_name) {
                Action::Default => out.push(event),
                Action::None => {}

                Action::Button { code: target } => match target.parse::<KeyCode>() {
                    Ok(t) => out.push(key_event(t, value)),
                    Err(_) => out.push(event),
                },

                Action::Key { code: target, modifiers } => {
                    let Ok(target) = target.parse::<KeyCode>() else {
                        out.push(event);
                        return;
                    };
                    let mods: Vec<KeyCode> = modifiers
                        .iter()
                        .filter_map(|m| m.parse::<KeyCode>().ok())
                        .collect();
                    match value {
                        1 => state.press(code.code(), &mods, target, out),
                        0 => state.release(code.code(), out),
                        // 2 = key repeat from the source: our virtual key owns
                        // its own repeat, so drop it.
                        _ => {}
                    }
                }

                Action::Macro { id } => {
                    // Fire on press only; the release is swallowed so the
                    // macro is not triggered twice.
                    if value == 1 {
                        if let Some(m) = profile.macros.get(&id) {
                            spawn_macro(m.clone());
                        }
                    }
                }
            }
        }

        EventSummary::Misc(..) => out.push(event),

        // SYN is handled by the caller; LEDs/switches/FF are not declared on
        // the virtual device so they are dropped rather than sent.
        _ => {}
    }
}

/// Turn one macro step into output events, tracking held keys/buttons.
///
/// `tap` emits press+release, `down` holds a key until a later `up`, which lets
/// recorded chords (Alt+Right, Ctrl+C, …) replay in the right order.
fn step_events(step: &MacroStep, held: &mut Vec<KeyCode>) -> Option<Vec<InputEvent>> {
    let code = step.code.parse::<KeyCode>().ok()?;
    Some(match step.action {
        MacroAction::Tap => vec![key_event(code, 1), key_event(code, 0)],
        MacroAction::Down => {
            if !held.contains(&code) {
                held.push(code);
            }
            vec![key_event(code, 1)]
        }
        MacroAction::Up => {
            held.retain(|c| *c != code);
            vec![key_event(code, 0)]
        }
    })
}

fn spawn_macro(m: Macro) {
    std::thread::spawn(move || {
        let Some(vdev) = current_vdev() else {
            tracing::warn!("macro {} triggered before the virtual device existed", m.name);
            return;
        };

        // Keys/buttons held by `down` steps, so a recording that ends mid-chord
        // cannot leave Ctrl/Alt or a mouse button stuck.
        let mut held: Vec<KeyCode> = Vec::new();

        for step in &m.steps {
            let Some(events) = step_events(step, &mut held) else {
                tracing::warn!("macro {}: unknown key/button `{}`", m.name, step.code);
                continue;
            };

            {
                let mut guard = lock_unpoison(&vdev);
                if let Err(e) = guard.emit(&events) {
                    tracing::warn!("macro {}: emit failed: {e}", m.name);
                    break;
                }
            }

            if step.delay_ms > 0 {
                std::thread::sleep(Duration::from_millis(step.delay_ms));
            }
        }

        if !held.is_empty() {
            let events: Vec<InputEvent> = held.iter().rev().map(|c| key_event(*c, 0)).collect();
            let mut guard = lock_unpoison(&vdev);
            let _ = guard.emit(&events);
        }
    });
}

/// Process-wide handle to the virtual device, set once at startup so macro
/// worker threads (which are not part of the supervisor graph) can emit.
static VDEV: Mutex<Option<Arc<Mutex<VirtualDevice>>>> = Mutex::new(None);

pub fn set_current_vdev(vdev: Arc<Mutex<VirtualDevice>>) {
    *lock_unpoison(&VDEV) = Some(vdev);
}

fn current_vdev() -> Option<Arc<Mutex<VirtualDevice>>> {
    lock_unpoison(&VDEV).clone()
}

/// Blocking loop for one grabbed device. Returns when the device disappears.
fn device_loop(
    path: &PathBuf,
    profile_slot: &Arc<RwLock<Arc<Profile>>>,
    vdev: &Arc<Mutex<VirtualDevice>>,
) -> Result<()> {
    let mut dev = Device::open(path).with_context(|| format!("opening {}", path.display()))?;
    dev.grab()
        .with_context(|| format!("grabbing {}", path.display()))?;
    let name = dev.name().unwrap_or("?").to_string();
    tracing::info!("grabbed {} ({name})", path.display());

    let mut state = TransformState::default();
    let mut out: Vec<InputEvent> = Vec::new();

    loop {
        for event in dev.fetch_events()? {
            if event.event_type() == EventType::SYNCHRONIZATION {
                if !out.is_empty() {
                    let mut guard = lock_unpoison(vdev);
                    guard.emit(&out)?;
                    out.clear();
                }
                continue;
            }

            // Read the profile once per event: the copy is cheap (Arc clone)
            // and keeps edits from the web UI visible without a restart.
            let profile = profile_slot
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .clone();
            transform(event, &profile, &mut state, &mut out);
            drop(profile);
        }
    }
}

/// Dry-run mode: keep the device list in the UI fresh without ever opening a
/// grab or a virtual device, so the app stays usable before `/dev/uinput` has
/// been granted.
pub fn spawn_scanner(managed: Arc<Mutex<Vec<FoundDevice>>>) {
    std::thread::spawn(move || loop {
        *lock_unpoison(&managed) = devices::scan();
        std::thread::sleep(Duration::from_secs(1));
    });
}

/// Watch for hotplug and keep one worker per managed node alive.
///
/// Worker threads exit on their own when a device is unplugged (the read
/// fails), so this only needs to spawn newcomers and forget the departed.
pub fn spawn_supervisor(
    profile_slot: Arc<RwLock<Arc<Profile>>>,
    vdev: Arc<Mutex<VirtualDevice>>,
    managed: Arc<Mutex<Vec<FoundDevice>>>,
) {
    std::thread::spawn(move || {
        let (done_tx, done_rx) = mpsc::channel::<PathBuf>();
        let mut running: HashSet<PathBuf> = HashSet::new();

        loop {
            // Reap finished workers.
            while let Ok(path) = done_rx.try_recv() {
                running.remove(&path);
                tracing::info!("released {}", path.display());
            }

            let found = devices::scan();
            {
                let mut list = lock_unpoison(&managed);
                list.clear();
                for d in &found {
                    let mut d = d.clone();
                    d.managed = running.contains(&d.path);
                    list.push(d);
                }
            }

            for device in found {
                if running.contains(&device.path) {
                    continue;
                }
                let path = device.path.clone();
                let tx = done_tx.clone();
                let slot = profile_slot.clone();
                let vdev = vdev.clone();
                running.insert(path.clone());
                std::thread::spawn(move || {
                    if let Err(e) = device_loop(&path, &slot, &vdev) {
                        // ENODEV / ENOENT are normal on unplug.
                        tracing::debug!("{}: {e:#}", path.display());
                    }
                    let _ = tx.send(path);
                });
            }

            std::thread::sleep(Duration::from_secs(1));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_scale_is_lossless() {
        let mut acc = 0.0;
        for v in [3, -7, 1, 0, 12, -12] {
            assert_eq!(scale(&mut acc, v, 1.0, false), v);
        }
        assert_eq!(acc, 0.0);
    }

    #[test]
    fn fractional_scale_accumulates_instead_of_rounding_to_zero() {
        let mut acc = 0.0;
        let mut total = 0;
        for _ in 0..10 {
            total += scale(&mut acc, 1, 0.5, false);
        }
        assert_eq!(total, 5, "0.5x over ten single-pixel steps must total 5");
    }

    #[test]
    fn invert_flips_sign_without_losing_magnitude() {
        let mut acc = 0.0;
        assert_eq!(scale(&mut acc, 4, 2.0, true), -8);
    }

    fn rel_event(code: RelativeAxisCode, value: i32) -> InputEvent {
        InputEvent::new_now(EventType::RELATIVE.0, code.0, value)
    }

    #[test]
    fn scroll_speed_scales_legacy_and_hi_res_wheel_consistently() {
        let profile = Profile {
            scroll_speed: 2.0,
            ..Profile::default()
        };
        let mut st = TransformState::default();
        let mut out = Vec::new();

        transform(rel_event(RelativeAxisCode::REL_WHEEL, 1), &profile, &mut st, &mut out);
        transform(rel_event(RelativeAxisCode::REL_WHEEL_HI_RES, 120), &profile, &mut st, &mut out);

        assert_eq!(out.len(), 2);
        assert_eq!(out[0].value(), 2, "legacy notch x2");
        assert_eq!(out[1].value(), 240, "hi-res 120 x2");
    }

    #[test]
    fn invert_scroll_flips_both_wheel_axes() {
        let profile = Profile {
            invert_scroll: true,
            ..Profile::default()
        };
        let mut st = TransformState::default();
        let mut out = Vec::new();

        transform(rel_event(RelativeAxisCode::REL_WHEEL, 1), &profile, &mut st, &mut out);
        transform(rel_event(RelativeAxisCode::REL_WHEEL_HI_RES, -120), &profile, &mut st, &mut out);

        assert_eq!(out[0].value(), -1);
        assert_eq!(out[1].value(), 120);
    }

    #[test]
    fn pointer_motion_passes_through_untouched_at_1x() {
        let profile = Profile::default();
        let mut st = TransformState::default();
        let mut out = Vec::new();

        transform(rel_event(RelativeAxisCode::REL_X, 7), &profile, &mut st, &mut out);
        transform(rel_event(RelativeAxisCode::REL_Y, -3), &profile, &mut st, &mut out);

        assert_eq!(out.len(), 2);
        assert_eq!((out[0].code(), out[0].value()), (RelativeAxisCode::REL_X.0, 7));
        assert_eq!((out[1].code(), out[1].value()), (RelativeAxisCode::REL_Y.0, -3));
    }

    #[test]
    fn virtual_device_name_is_never_a_kensington_match() {
        assert!(!VIRTUAL_DEVICE_NAME.starts_with("Kensington"));
    }

    #[test]
    fn key_parse_roundtrips_known_names() {
        let parsed: KeyCode = "BTN_LEFT".parse().unwrap();
        assert_eq!(format!("{parsed:?}"), "BTN_LEFT");
        let key: KeyCode = "KEY_LEFTCTRL".parse().unwrap();
        assert_eq!(format!("{key:?}"), "KEY_LEFTCTRL");
    }

    #[test]
    fn button_remap_emits_target_key_and_releases_it() {
        let profile = Profile {
            buttons: [(
                "BTN_SIDE".to_string(),
                Action::Key {
                    code: "KEY_C".into(),
                    modifiers: vec!["KEY_LEFTCTRL".into()],
                },
            )]
            .into_iter()
            .collect(),
            ..Profile::default()
        };
        let mut st = TransformState::default();
        let mut out = Vec::new();

        let press = InputEvent::new_now(EventType::KEY.0, KeyCode::BTN_SIDE.code(), 1);
        transform(press, &profile, &mut st, &mut out);
        assert_eq!(out.len(), 2, "modifier down + key down");
        out.clear();

        let release = InputEvent::new_now(EventType::KEY.0, KeyCode::BTN_SIDE.code(), 0);
        transform(release, &profile, &mut st, &mut out);
        assert_eq!(out.len(), 2, "key up + modifier up");
        assert_eq!(out[0].value(), 0);
        assert_eq!(out[1].value(), 0);
        assert!(st.held.is_empty());
        assert!(st.mod_refs.is_empty());
    }

    #[test]
    fn none_action_swallows_both_edges() {
        let profile = Profile {
            buttons: [("BTN_EXTRA".to_string(), Action::None)].into_iter().collect(),
            ..Profile::default()
        };
        let mut st = TransformState::default();
        let mut out = Vec::new();
        for value in [1, 0] {
            transform(
                InputEvent::new_now(EventType::KEY.0, KeyCode::BTN_EXTRA.code(), value),
                &profile,
                &mut st,
                &mut out,
            );
        }
        assert!(out.is_empty());
    }

    #[test]
    fn default_action_forwards_unmapped_buttons() {
        let profile = Profile::default();
        let mut st = TransformState::default();
        let mut out = Vec::new();
        transform(
            InputEvent::new_now(EventType::KEY.0, KeyCode::BTN_LEFT.code(), 1),
            &profile,
            &mut st,
            &mut out,
        );
        assert_eq!(out.len(), 1);
    }

    #[test]
    fn shared_modifier_is_released_only_after_last_user() {
        let profile = Profile {
            buttons: [
                (
                    "BTN_SIDE".to_string(),
                    Action::Key { code: "KEY_C".into(), modifiers: vec!["KEY_LEFTCTRL".into()] },
                ),
                (
                    "BTN_EXTRA".to_string(),
                    Action::Key { code: "KEY_V".into(), modifiers: vec!["KEY_LEFTCTRL".into()] },
                ),
            ]
            .into_iter()
            .collect(),
            ..Profile::default()
        };
        let mut st = TransformState::default();
        let mut out = Vec::new();

        transform(InputEvent::new_now(EventType::KEY.0, KeyCode::BTN_SIDE.code(), 1), &profile, &mut st, &mut out);
        transform(InputEvent::new_now(EventType::KEY.0, KeyCode::BTN_EXTRA.code(), 1), &profile, &mut st, &mut out);
        out.clear();

        // First release must NOT drop the shared Ctrl.
        transform(InputEvent::new_now(EventType::KEY.0, KeyCode::BTN_SIDE.code(), 0), &profile, &mut st, &mut out);
        assert_eq!(out.len(), 1, "only the target key should go up");
        out.clear();

        transform(InputEvent::new_now(EventType::KEY.0, KeyCode::BTN_EXTRA.code(), 0), &profile, &mut st, &mut out);
        assert_eq!(out.len(), 2, "target up + modifier up");
        assert!(st.mod_refs.is_empty());
    }

    fn step(code: &str, action: MacroAction) -> MacroStep {
        MacroStep { code: code.into(), delay_ms: 0, action }
    }

    #[test]
    fn macro_tap_emits_press_then_release() {
        let mut held = Vec::new();
        let events = step_events(&step("KEY_F13", MacroAction::Tap), &mut held).unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!((events[0].code(), events[0].value()), (KeyCode::KEY_F13.code(), 1));
        assert_eq!((events[1].code(), events[1].value()), (KeyCode::KEY_F13.code(), 0));
        assert!(held.is_empty());
    }

    #[test]
    fn macro_down_and_up_replay_a_chord_in_order() {
        let mut held = Vec::new();

        let down = step_events(&step("KEY_LEFTALT", MacroAction::Down), &mut held).unwrap();
        assert_eq!(down.len(), 1);
        assert_eq!(down[0].value(), 1);
        assert_eq!(held, vec![KeyCode::KEY_LEFTALT]);

        let press = step_events(&step("KEY_RIGHT", MacroAction::Tap), &mut held).unwrap();
        assert_eq!(press[0].value(), 1);
        assert_eq!(press[1].value(), 0);

        let up = step_events(&step("KEY_LEFTALT", MacroAction::Up), &mut held).unwrap();
        assert_eq!(up.len(), 1);
        assert_eq!(up[0].value(), 0);
        assert!(held.is_empty(), "up must clear the held modifier");
    }

    #[test]
    fn macro_unknown_key_is_reported_instead_of_emitted() {
        let mut held = Vec::new();
        assert!(step_events(&step("KEY_NOT_REAL", MacroAction::Tap), &mut held).is_none());
    }
}
