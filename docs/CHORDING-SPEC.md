# Chording — Functional Spec

## Summary

Any button can be made a **chord button**. While it is held, the other
buttons use the chord's mappings. When it is released, they go back to their
normal mappings. The chord button keeps its own normal action, which is
pressed and released along with the button, the same as any other mapping.

## Definitions

| Term | Meaning |
|---|---|
| Chord button | A button that has a chord. |
| Chord | The mappings used while the chord button is held. |
| Own action | The chord button's normal mapping (`buttons` entry, or `default` if it has none). |
| Base mapping | A button's normal mapping from the profile. |
| Chorded button | A button that has an entry in the active chord. |

## Configuration

Chords are stored in the profile under `chords`, keyed by the chord button.
The chord button's own action stays in `buttons`, like any other mapping.

| Field | Required | Description |
|---|---|---|
| `buttons` | yes | Map of button → action used while the chord is held. An action can be `default`, `none`, `button`, `key` (with modifiers), or `macro`. |
| `passthrough` | no | Default `true`. While the chord is held, buttons with no chord entry use their base mapping. If `false`, they are blocked. |

## Examples

Both examples give copy / cut / paste on `BTN_SIDE` (top-right).

### Chord only

The own action is **Disable**, so holding the button only switches the chord
on. Each chorded button sends the full command, modifiers included.

```json
"buttons": {
  "BTN_SIDE": { "type": "none" }
},
"chords": {
  "BTN_SIDE": {
    "buttons": {
      "BTN_LEFT":   { "type": "key", "code": "KEY_C", "modifiers": ["KEY_LEFTCTRL"] },
      "BTN_MIDDLE": { "type": "key", "code": "KEY_X", "modifiers": ["KEY_LEFTCTRL"] },
      "BTN_RIGHT":  { "type": "key", "code": "KEY_V", "modifiers": ["KEY_LEFTCTRL"] }
    }
  }
}
```

- Holding `BTN_SIDE` sends nothing.
- `BTN_SIDE` + `BTN_LEFT` sends Ctrl+C.
- `BTN_SIDE` + `BTN_RIGHT` sends Ctrl+V.

### Chord + Ctrl

The own action is Ctrl, so holding the button switches the chord on and holds
Ctrl. Each chorded button sends only its own key, which combines with Ctrl.

```json
"buttons": {
  "BTN_SIDE": { "type": "key", "code": "KEY_LEFTCTRL" }
},
"chords": {
  "BTN_SIDE": {
    "buttons": {
      "BTN_LEFT":   { "type": "key", "code": "KEY_C" },
      "BTN_MIDDLE": { "type": "key", "code": "KEY_X" },
      "BTN_RIGHT":  { "type": "key", "code": "KEY_V" }
    }
  }
}
```

- Holding `BTN_SIDE` holds Ctrl for the whole time it is down.
- `BTN_SIDE` + `BTN_LEFT` sends Ctrl+C.
- `BTN_SIDE` + `BTN_RIGHT` sends Ctrl+V.
- While Ctrl is held, other input such as clicks and scrolling also gets Ctrl.

### Chord + Ctrl+Shift (terminal copy / paste)

The own action is `KEY_LEFTSHIFT` with the Ctrl modifier, so holding the
button holds Ctrl+Shift. Chorded buttons send plain C and V.

```json
"buttons": {
  "BTN_EXTRA": { "type": "key", "code": "KEY_LEFTSHIFT", "modifiers": ["KEY_LEFTCTRL"] }
},
"chords": {
  "BTN_EXTRA": {
    "buttons": {
      "BTN_LEFT":  { "type": "key", "code": "KEY_C" },
      "BTN_RIGHT": { "type": "key", "code": "KEY_V" }
    }
  }
}
```

- Holding `BTN_EXTRA` holds Ctrl+Shift.
- `BTN_EXTRA` + `BTN_LEFT` sends Ctrl+Shift+C.
- `BTN_EXTRA` + `BTN_RIGHT` sends Ctrl+Shift+V.

## Behaviour

### Chord button

1. **Press:** the chord switches on and the own action is pressed.
2. **Release:** the chord switches off and the own action is released.
3. A tap and a long hold behave the same way.

### Other buttons

| Situation | Result |
|---|---|
| Chord held, button has a chord entry | The chord action is used. |
| Chord held, no chord entry, `passthrough: true` | The base mapping is used. |
| Chord held, no chord entry, `passthrough: false` | The button is blocked. |
| No chord held | The base mapping is used. |

### Press/release pairing

- A button always releases the action it pressed. This holds even if a chord switches on or off while that button is still down.
- A button that was already held when the chord switched on keeps its base action until it is released.
- A chorded button that is still held after the chord switches off keeps its chord action until it is released.
- Modifiers that several actions share are released only when the last action using them is released.

### Pointer and scroll

Pointer motion and scrolling are not affected by chords. A modifier held by the own action applies to them, so Ctrl + scroll works.

### Multiple chord buttons

- A profile can have more than one chord button.
- Only one chord is active at a time. A chord button pressed while another chord is held acts as an ordinary button: it uses its entry in the active chord, or its own base mapping if passthrough applies. It does not switch on its own chord.

### Interruptions

When the profile is switched, the device is unplugged, or the daemon stops, every key, button and modifier being held is released and the chord switches off.

## Validation

When a profile is saved:

- A chord must not contain an entry for its own chord button. Such an entry is dropped.
- A chord entry with an empty key `code` is dropped.
- A chord entry with a `macro` action must refer to a macro that exists in the profile. Otherwise the save is rejected.

The UI shows a warning when `BTN_LEFT` is made a chord button.

## Web UI

- **Mapping rows:** each row has a **Chord** checkbox next to the action picker. A chord button's row shows a "Chord · N entries" badge.
- **Chord editor:** ticking Chord opens a panel with:
  - a note that the action above is pressed and released with the button, and that **Disable** makes the button only switch the chord
  - a **Passthrough** checkbox
  - a **While held** table with one row per device button, except the chord button. Each row uses the same action editor as the base mappings, and a row left as "Unchanged" means no chord entry.
- Removing a row also removes its chord. Changing a row's button moves the chord with it.

## CLI

`kenctl profiles` lists each chord button, its own action, and its chord entries.

## Compatibility

- Profiles without `chords` load and behave exactly as they do now.
- Chords are saved in the same `profiles.json` file.
