use crate::devices::{self, FoundDevice};
use crate::profiles::{Action, Chord, Macro, MacroAction, MacroStep, Profile};
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

/// Identifies one grabbed evdev node, so the same button code on two nodes
/// (or two trackballs) is tracked separately.
pub(crate) type NodeId = u32;

/// Which part of a press an output belongs to: the physical button itself, or
/// the own action of a chord button.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Slot {
    Physical,
    Own,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct Src {
    node: NodeId,
    code: u16,
    slot: Slot,
}

impl Src {
    fn physical(node: NodeId, code: u16) -> Self {
        Src { node, code, slot: Slot::Physical }
    }
    fn with(self, slot: Slot) -> Self {
        Src { slot, ..self }
    }
}

/// The chord currently switched on.
struct ActiveChord {
    src: Src,
    chord: Chord,
}

/// What a pressed source did, so its release can undo exactly that.
#[derive(Clone)]
enum Latched {
    Act(Action),
    /// The press switched a chord on.
    Chord,
}

/// Transform state shared by every device thread, so a chord held on one node
/// applies to buttons arriving on another (the TB900 exposes two nodes).
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

    /// Source -> keys that source is currently holding on our behalf.
    held: HashMap<Src, Held>,
    /// Reference counts so shared modifiers are released only once.
    mod_refs: HashMap<KeyCode, u32>,
    /// Action each pressed source resolved to, so its release always undoes
    /// exactly what its press did, whatever the chord or profile does since.
    latched: HashMap<Src, Latched>,
    active: Option<ActiveChord>,
    /// Profile the latched presses were made under.
    profile_seen: Option<Arc<Profile>>,
}

#[derive(Clone)]
struct Held {
    modifiers: Vec<KeyCode>,
    target: KeyCode,
}

impl TransformState {
    fn press(&mut self, source: Src, modifiers: &[KeyCode], target: KeyCode, out: &mut Vec<InputEvent>) {
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

    fn release(&mut self, source: Src, out: &mut Vec<InputEvent>) {
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

    /// The action a button press means right now, taking the active chord
    /// into account.
    fn resolve(&mut self, profile: &Profile, name: &str) -> Action {
        if let Some(active) = self.active.as_ref() {
            if let Some(action) = active.chord.buttons.get(name) {
                return action.clone();
            }
            if !active.chord.passthrough {
                return Action::None;
            }
        }
        profile.action_for(name)
    }

    /// Apply the press edge of `action` and return what to latch for release.
    fn apply_press(
        &mut self,
        src: Src,
        action: &Action,
        profile: &Profile,
        out: &mut Vec<InputEvent>,
    ) -> Action {
        let orig = KeyCode(src.code);
        match action {
            Action::Default => {
                out.push(key_event(orig, 1));
                Action::Default
            }
            Action::None => Action::None,
            Action::Button { code } => match code.parse::<KeyCode>() {
                Ok(t) => {
                    out.push(key_event(t, 1));
                    action.clone()
                }
                Err(_) => {
                    out.push(key_event(orig, 1));
                    Action::Default
                }
            },
            Action::Key { code, modifiers } => {
                let Ok(target) = code.parse::<KeyCode>() else {
                    out.push(key_event(orig, 1));
                    return Action::Default;
                };
                let mods: Vec<KeyCode> = modifiers
                    .iter()
                    .filter_map(|m| m.parse::<KeyCode>().ok())
                    .collect();
                self.press(src, &mods, target, out);
                action.clone()
            }
            Action::Macro { id } => {
                // Fire on press only; the release is swallowed so the macro
                // is not triggered twice.
                if let Some(m) = profile.macros.get(id) {
                    spawn_macro(m.clone());
                }
                Action::None
            }
        }
    }

    /// Undo a latched action press.
    fn apply_release(&mut self, src: Src, latched: Action, out: &mut Vec<InputEvent>) {
        match latched {
            Action::Default => out.push(key_event(KeyCode(src.code), 0)),
            Action::Button { code } => {
                if let Ok(t) = code.parse::<KeyCode>() {
                    out.push(key_event(t, 0));
                }
            }
            Action::Key { .. } => self.release(src, out),
            Action::None | Action::Macro { .. } => {}
        }
    }

    /// Switch `chord` on for the button `src` and press its own action.
    fn start_chord(&mut self, src: Src, chord: &Chord, own: Action, profile: &Profile, out: &mut Vec<InputEvent>) {
        let os = src.with(Slot::Own);
        let latched = self.apply_press(os, &own, profile, out);
        self.latched.insert(os, Latched::Act(latched));
        self.active = Some(ActiveChord { src, chord: chord.clone() });
    }

    /// The chord button was released: switch the chord off and release its
    /// own action.
    fn end_chord(&mut self, src: Src, out: &mut Vec<InputEvent>) {
        if self.active.as_ref().map(|a| a.src) == Some(src) {
            self.active = None;
        }
        let os = src.with(Slot::Own);
        if let Some(Latched::Act(own)) = self.latched.remove(&os) {
            self.apply_release(os, own, out);
        }
    }

    /// Release everything held for sources matching `which`.
    fn release_where(&mut self, which: impl Fn(&Src) -> bool, out: &mut Vec<InputEvent>) {
        if self.active.as_ref().is_some_and(|a| which(&a.src)) {
            self.active = None;
        }
        let sources: Vec<Src> = self.latched.keys().copied().filter(|s| which(s)).collect();
        for src in sources {
            if let Some(Latched::Act(action)) = self.latched.remove(&src) {
                self.apply_release(src, action, out);
            }
        }
    }

    /// Release everything (profile switched).
    pub(crate) fn release_all(&mut self, out: &mut Vec<InputEvent>) {
        self.release_where(|_| true, out);
    }

    /// Release everything a node was holding (node unplugged).
    pub(crate) fn release_node(&mut self, node: NodeId, out: &mut Vec<InputEvent>) {
        self.release_where(|s| s.node == node, out);
    }

    /// Release everything if the active profile changed since the last event.
    pub(crate) fn note_profile(&mut self, profile: &Arc<Profile>, out: &mut Vec<InputEvent>) {
        match &self.profile_seen {
            Some(seen) if Arc::ptr_eq(seen, profile) => {}
            Some(_) => {
                self.release_all(out);
                self.profile_seen = Some(profile.clone());
            }
            None => self.profile_seen = Some(profile.clone()),
        }
    }

    fn release_latched(&mut self, src: Src, latched: Latched, out: &mut Vec<InputEvent>) {
        match latched {
            Latched::Act(action) => self.apply_release(src, action, out),
            Latched::Chord => self.end_chord(src, out),
        }
    }

    fn key(&mut self, node: NodeId, code: KeyCode, value: i32, profile: &Profile, out: &mut Vec<InputEvent>) {
        let src = Src::physical(node, code.code());
        match value {
            1 => {
                if let Some(stale) = self.latched.remove(&src) {
                    self.release_latched(src, stale, out);
                }
                let name = format!("{code:?}");
                // One chord at a time: a chord button pressed while another
                // chord is active acts as an ordinary button.
                let latched = match profile.chord_for(&name) {
                    Some(chord) if self.active.is_none() => {
                        self.start_chord(src, chord, profile.action_for(&name), profile, out);
                        Latched::Chord
                    }
                    _ => {
                        let action = self.resolve(profile, &name);
                        Latched::Act(self.apply_press(src, &action, profile, out))
                    }
                };
                self.latched.insert(src, latched);
            }
            0 => {
                if let Some(latched) = self.latched.remove(&src) {
                    self.release_latched(src, latched, out);
                }
            }
            // 2 = key repeat from the source. Our virtual keys own their own
            // repeat, so only plain button passthrough forwards it.
            _ => match self.latched.get(&src) {
                Some(Latched::Act(Action::Default)) => out.push(key_event(code, value)),
                Some(Latched::Act(Action::Button { code: t })) => {
                    if let Ok(t) = t.parse::<KeyCode>() {
                        out.push(key_event(t, value));
                    }
                }
                _ => {}
            },
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
    transform_from(0, event, profile, state, out)
}

/// [`transform`] for an event read from grabbed node `node`.
pub(crate) fn transform_from(
    node: NodeId,
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

        EventSummary::Key(_, code, value) => state.key(node, code, value, profile, out),

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
    node: NodeId,
    profile_slot: &Arc<RwLock<Arc<Profile>>>,
    vdev: &Arc<Mutex<VirtualDevice>>,
    state: &Arc<Mutex<TransformState>>,
) -> Result<()> {
    let mut dev = Device::open(path).with_context(|| format!("opening {}", path.display()))?;
    dev.grab()
        .with_context(|| format!("grabbing {}", path.display()))?;
    let name = dev.name().unwrap_or("?").to_string();
    tracing::info!("grabbed {} ({name})", path.display());

    let mut out: Vec<InputEvent> = Vec::new();

    let result = (|| -> Result<()> {
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

                // Read the profile once per event: the copy is cheap (Arc
                // clone) and keeps edits from the web UI visible without a
                // restart.
                let profile = profile_slot
                    .read()
                    .unwrap_or_else(|e| e.into_inner())
                    .clone();
                let mut st = lock_unpoison(state);
                st.note_profile(&profile, &mut out);
                transform_from(node, event, &profile, &mut st, &mut out);
            }
        }
    })();

    // Unplugged: let go of anything this node was holding.
    out.clear();
    lock_unpoison(state).release_node(node, &mut out);
    if !out.is_empty() {
        let _ = lock_unpoison(vdev).emit(&out);
    }
    result
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
        let state = Arc::new(Mutex::new(TransformState::default()));
        let mut next_node: NodeId = 1;

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
                let state = state.clone();
                let node = next_node;
                next_node = next_node.wrapping_add(1);
                running.insert(path.clone());
                std::thread::spawn(move || {
                    if let Err(e) = device_loop(&path, node, &slot, &vdev, &state) {
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

    // ------------------------------------------------------------ chords

    fn key_action(code: &str, mods: &[&str]) -> Action {
        Action::Key { code: code.into(), modifiers: mods.iter().map(|m| m.to_string()).collect() }
    }

    fn copy_paste() -> std::collections::BTreeMap<String, Action> {
        [
            ("BTN_LEFT".to_string(), key_action("KEY_C", &[])),
            ("BTN_RIGHT".to_string(), key_action("KEY_V", &[])),
        ]
        .into_iter()
        .collect()
    }

    /// BTN_SIDE as a chord button with own action `own` and `chord`.
    fn chord_profile(own: Option<Action>, chord: Chord) -> Profile {
        Profile {
            buttons: own.into_iter().map(|c| ("BTN_SIDE".to_string(), c)).collect(),
            chords: [("BTN_SIDE".to_string(), chord)].into_iter().collect(),
            ..Profile::default()
        }
    }

    /// The "chord + Ctrl" copy/paste setup.
    fn hold_ctrl() -> Profile {
        chord_profile(
            Some(key_action("KEY_LEFTCTRL", &[])),
            Chord { buttons: copy_paste(), ..Chord::default() },
        )
    }

    /// Feed one key edge from `node` and return (code, value) pairs.
    fn edge(node: NodeId, code: KeyCode, value: i32, p: &Profile, st: &mut TransformState) -> Vec<(u16, i32)> {
        let mut out = Vec::new();
        transform_from(node, InputEvent::new_now(EventType::KEY.0, code.code(), value), p, st, &mut out);
        out.iter().map(|e| (e.code(), e.value())).collect()
    }

    const CTRL: u16 = KeyCode::KEY_LEFTCTRL.0;
    const C: u16 = KeyCode::KEY_C.0;
    const V: u16 = KeyCode::KEY_V.0;
    const SIDE: u16 = KeyCode::BTN_SIDE.0;

    #[test]
    fn disabled_own_action_chord_sends_full_command_from_chorded_button() {
        let p = chord_profile(
            Some(Action::None),
            Chord {
                buttons: [("BTN_LEFT".to_string(), key_action("KEY_C", &["KEY_LEFTCTRL"]))].into_iter().collect(),
                ..Chord::default()
            },
        );
        let mut st = TransformState::default();
        assert!(edge(0, KeyCode::BTN_SIDE, 1, &p, &mut st).is_empty(), "holding sends nothing");
        assert_eq!(edge(0, KeyCode::BTN_LEFT, 1, &p, &mut st), vec![(CTRL, 1), (C, 1)]);
        assert_eq!(edge(0, KeyCode::BTN_LEFT, 0, &p, &mut st), vec![(C, 0), (CTRL, 0)]);
        assert!(edge(0, KeyCode::BTN_SIDE, 0, &p, &mut st).is_empty());
        assert!(st.active.is_none() && st.latched.is_empty() && st.mod_refs.is_empty());
    }

    #[test]
    fn own_action_is_held_for_as_long_as_the_chord_button() {
        let p = hold_ctrl();
        let mut st = TransformState::default();
        assert_eq!(edge(0, KeyCode::BTN_SIDE, 1, &p, &mut st), vec![(CTRL, 1)]);
        assert_eq!(edge(0, KeyCode::BTN_RIGHT, 1, &p, &mut st), vec![(V, 1)]);
        assert_eq!(edge(0, KeyCode::BTN_RIGHT, 0, &p, &mut st), vec![(V, 0)]);
        assert_eq!(edge(0, KeyCode::BTN_SIDE, 0, &p, &mut st), vec![(CTRL, 0)]);
        assert!(st.held.is_empty() && st.latched.is_empty());
    }

    #[test]
    fn tapping_and_holding_behave_the_same() {
        // A quick press/release with nothing else pressed is just press + release.
        let p = chord_profile(None, Chord { buttons: copy_paste(), ..Chord::default() });
        let mut st = TransformState::default();
        assert_eq!(edge(0, KeyCode::BTN_SIDE, 1, &p, &mut st), vec![(SIDE, 1)]);
        assert_eq!(edge(0, KeyCode::BTN_SIDE, 0, &p, &mut st), vec![(SIDE, 0)]);
    }

    #[test]
    fn passthrough_controls_unmapped_buttons() {
        let mid = KeyCode::BTN_MIDDLE.0;
        for (passthrough, expected) in [(true, vec![(mid, 1)]), (false, vec![])] {
            let p = chord_profile(Some(Action::None), Chord { passthrough, buttons: copy_paste() });
            let mut st = TransformState::default();
            edge(0, KeyCode::BTN_SIDE, 1, &p, &mut st);
            assert_eq!(edge(0, KeyCode::BTN_MIDDLE, 1, &p, &mut st), expected);
        }
    }

    #[test]
    fn chorded_button_releases_its_chord_action_after_chord_ends() {
        let p = hold_ctrl();
        let mut st = TransformState::default();
        edge(0, KeyCode::BTN_SIDE, 1, &p, &mut st);
        edge(0, KeyCode::BTN_LEFT, 1, &p, &mut st);
        assert_eq!(edge(0, KeyCode::BTN_SIDE, 0, &p, &mut st), vec![(CTRL, 0)]);
        assert_eq!(edge(0, KeyCode::BTN_LEFT, 0, &p, &mut st), vec![(C, 0)], "C, not BTN_LEFT");
    }

    #[test]
    fn button_held_before_chord_keeps_base_action() {
        let p = chord_profile(Some(Action::None), Chord { buttons: copy_paste(), ..Chord::default() });
        let left = KeyCode::BTN_LEFT.0;
        let mut st = TransformState::default();
        assert_eq!(edge(0, KeyCode::BTN_LEFT, 1, &p, &mut st), vec![(left, 1)]);
        edge(0, KeyCode::BTN_SIDE, 1, &p, &mut st);
        assert_eq!(edge(0, KeyCode::BTN_LEFT, 0, &p, &mut st), vec![(left, 0)]);
    }

    #[test]
    fn second_chord_button_acts_as_ordinary_button() {
        let mut p = chord_profile(Some(Action::None), Chord { buttons: copy_paste(), ..Chord::default() });
        p.chords.insert("BTN_EXTRA".into(), Chord::default());
        p.buttons.insert("BTN_EXTRA".into(), key_action("KEY_LEFTALT", &[]));
        let alt = KeyCode::KEY_LEFTALT.0;
        let mut st = TransformState::default();
        edge(0, KeyCode::BTN_SIDE, 1, &p, &mut st);
        assert_eq!(edge(0, KeyCode::BTN_EXTRA, 1, &p, &mut st), vec![(alt, 1)], "base action, no new chord");
        assert_eq!(st.active.as_ref().unwrap().src.code, SIDE);
        assert_eq!(edge(0, KeyCode::BTN_LEFT, 1, &p, &mut st), vec![(C, 1)], "first chord still active");
        assert_eq!(edge(0, KeyCode::BTN_EXTRA, 0, &p, &mut st), vec![(alt, 0)]);
    }

    #[test]
    fn chord_on_one_node_applies_to_buttons_on_another() {
        let p = chord_profile(Some(Action::None), Chord { buttons: copy_paste(), ..Chord::default() });
        let mut st = TransformState::default();
        edge(1, KeyCode::BTN_SIDE, 1, &p, &mut st);
        assert_eq!(edge(2, KeyCode::BTN_LEFT, 1, &p, &mut st), vec![(C, 1)]);
    }

    #[test]
    fn profile_switch_releases_everything() {
        let p = Arc::new(hold_ctrl());
        let mut st = TransformState::default();
        let mut out = Vec::new();
        st.note_profile(&p, &mut out);
        edge(0, KeyCode::BTN_SIDE, 1, &p, &mut st);
        edge(0, KeyCode::BTN_LEFT, 1, &p, &mut st);

        st.note_profile(&Arc::new(Profile::default()), &mut out);
        let codes: Vec<(u16, i32)> = out.iter().map(|e| (e.code(), e.value())).collect();
        assert!(codes.contains(&(C, 0)) && codes.contains(&(CTRL, 0)));
        assert!(st.active.is_none() && st.latched.is_empty() && st.held.is_empty());
    }

    #[test]
    fn unplugging_a_node_releases_only_its_presses() {
        let p = hold_ctrl();
        let mut st = TransformState::default();
        edge(1, KeyCode::BTN_SIDE, 1, &p, &mut st);
        edge(2, KeyCode::BTN_LEFT, 1, &p, &mut st);
        let mut out = Vec::new();
        st.release_node(1, &mut out);
        assert_eq!(out.iter().map(|e| (e.code(), e.value())).collect::<Vec<_>>(), vec![(CTRL, 0)]);
        assert!(st.active.is_none());
        assert_eq!(edge(2, KeyCode::BTN_LEFT, 0, &p, &mut st), vec![(C, 0)]);
    }

    #[test]
    fn modifier_on_own_action_gives_terminal_copy_paste() {
        let mut p = chord_profile(
            Some(key_action("KEY_LEFTSHIFT", &["KEY_LEFTCTRL"])),
            Chord { buttons: copy_paste(), ..Chord::default() },
        );
        p.sanitize();
        let shift = KeyCode::KEY_LEFTSHIFT.0;
        let mut st = TransformState::default();
        assert_eq!(edge(0, KeyCode::BTN_SIDE, 1, &p, &mut st), vec![(CTRL, 1), (shift, 1)]);
        assert_eq!(edge(0, KeyCode::BTN_LEFT, 1, &p, &mut st), vec![(C, 1)], "Ctrl+Shift+C");
        assert_eq!(edge(0, KeyCode::BTN_LEFT, 0, &p, &mut st), vec![(C, 0)]);
        assert_eq!(edge(0, KeyCode::BTN_RIGHT, 1, &p, &mut st), vec![(V, 1)], "Ctrl+Shift+V");
        assert_eq!(edge(0, KeyCode::BTN_RIGHT, 0, &p, &mut st), vec![(V, 0)]);
        assert_eq!(edge(0, KeyCode::BTN_SIDE, 0, &p, &mut st), vec![(shift, 0), (CTRL, 0)]);
    }
}
