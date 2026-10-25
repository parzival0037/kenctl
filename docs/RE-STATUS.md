# KenCtl — Reverse Engineering Notes

Date: 2026-10-23
> KenCtl is a Linux reimplementation. “Konnect” below refers to Kensington’s
> original software, not to this project.
Device under test: **Kensington Expert Wireless Trackball (K72359WW)**, `047d:8018`
Connected over USB cable, enumerated at `1-8`.

## 1. What was verified vs. what was guessed

Earlier scaffolding in this tree contained HID command opcodes that were
**invented by analogy**, not derived from the device or from the vendor
binaries:

| Claimed command | "opcode" used | Status |
|---|---|---|
| set_pointer | `01 <speed> <accel> <sens>` | **unverified guess** |
| get_pointer | `81` | **unverified guess** |
| set_scroll | `10 <speed> <dir> <smooth>` | **unverified guess** |
| set_keybtn | `20 <mask:le> <action>` | **unverified guess** |
| get_battery | `F0` | **unverified guess** |

No capture, no disassembly and no device response backed any of these. The
set/get `0x10`/`0x90` nibble pattern was inferred purely from a set/get
convention, which is not evidence.

## 2. Ground truth from the HID report descriptors

Read from `/sys/bus/hid/devices/0003:047D:8018.000*/report_descriptor`.

### Interface 1 — vendor/config channel (`...000C`, `/dev/hidraw5`)

```
06 0a ff        Usage Page (Vendor 0xFF0A)
09 01           Usage (0x01)
a1 01           Collection (Application)
  09 02         Usage (0x02)
  a1 00         Collection (Physical)
    06 00 ff    Usage Page (Vendor 0xFF00)
    09 03 09 04 Usage (0x03, 0x04)
    15 80 25 7f Logical Min/Max (-128..127)
    35 00 46 ff 00  Physical Min/Max (0..255)
    75 08        Report Size (8)
    95 20        Report Count (32)
    81 02        Input  (Data,Var,Abs)   -> 32-byte INPUT report
    09 05 09 06  Usage (0x05, 0x06)
    ... 75 08 95 20
    91 02        Output (Data,Var,Abs)   -> 32-byte OUTPUT report
  c0 c0
```

Consequences:
- **No Report IDs** on this interface. Prepending `0x02` / `0x03` (as the old
  code did) injects a phantom report id that does not exist.
- **No Feature items at all.** The old code's `send_feature_report(0x03)` /
  `get_feature_report` path could not have worked — there is nothing to read.
- Exactly 32-byte input and 32-byte output reports.

### Interface 0 — mouse/input channel (`...000D`, `/dev/hidraw6`)

- Report ID 1: 5 buttons + padding + REL_WHEEL + REL_HWHEEL (consumer AC Pan)
- Report ID 2: X, Y (12-bit relative, -1999..2047)
- Report ID 3: consumer media keys (play/pause, scan next/prev, vol up/down)

Corresponding evdev nodes:
- `event7` — "Kensington Expert Wireless TB Mouse" (`REL X,Y,HWHEEL,WHEEL,
  WHEEL_HI_RES,HWHEEL_HI_RES`)
- `event8` — "Kensington Expert Wireless TB Consumer Control"

> Note: older comments in `src/device.rs` had hidraw5/hidraw6 the wrong way
> round. hidraw5 = config (iface 1), hidraw6 = mouse (iface 0).

### USB endpoints (iface 1)

- `0x81` interrupt **IN**, wMaxPacket 32, bInterval 6
- `0x01` interrupt **OUT**, wMaxPacket 32, bInterval 6

## 3. Live probe results

Scripts: `re/probe_config.py`, `re/size_matrix.py`.

| Experiment | Result |
|---|---|
| Passive read on `/dev/hidraw5` | opens, **0 unsolicited reports** (pure request/response) |
| Write 32 zero bytes | `EPROTO` |
| Write sizes 2,4,8,16,31,32,33,40,64 | `EPROTO` (size 1 -> `EINVAL`, i.e. hidraw length check) |
| Header byte sweep `0x00..0xFF` @ 32 bytes | **0/256 accepted** |
| Write to mouse iface `/dev/hidraw6` | `OK` (control path is healthy) |
| Kernel log | `hid-generic 0003:047D:8018.000C: pid ... passed too short report` |

### Interpretation

`hidraw` first calls `hid_hw_output_report()` (interrupt OUT). Because iface 1
*has* an interrupt OUT endpoint, `usbhid` sets up `urbout` and therefore never
falls back to a control `SET_REPORT`. The device rejects the interrupt OUT
transfer for **every** payload, so this is a transport-level rejection, not a
command-content rejection.

Access to a working write path therefore requires bypassing `hidraw`, i.e. a
control `SET_REPORT` issued directly over usbfs (`/dev/bus/usb/...`), which
needs root or a udev rule. **Hardware-level configuration is currently blocked
on that permission**, not on protocol knowledge.

## 4. Implication for the product

From the extracted frontend, Konnect requests macOS *input monitoring* and
*accessibility* permissions specifically to "monitor keyboard and mouse input
to support button remapping" and to "control your cursor". Combined with the
architecture notes (profiles/macros/app-specific settings are Konductor-side
software state), this says the majority of user-facing features are applied
**on the host**, not flashed into the device.

Therefore the Linux port is built as a **host-side input engine**:

| Feature | Mechanism on Linux |
|---|---|
| Button remapping | evdev `EVIOCGRAB` -> transform -> `uinput` |
| Macros / key combos | emitted through the same virtual device |
| Pointer speed | REL_X/REL_Y scaling with fractional carry |
| Scroll speed / direction | REL_WHEEL(+HI_RES)/REL_HWHEEL scaling + sign flip |
| Profiles | persisted JSON, hot-switchable |
| Hardware DPI / firmware | *pending* — needs usbfs access (see §3) |

This delivers the requested features without depending on the blocked
transport, and leaves the hardware channel as an additive capability.

## 5. Open items

- [ ] Obtain usbfs write access (install script provisions the udev rule).
- [ ] Retry control `SET_REPORT` against the vendor interface; if it answers,
      capture and decode the real command set.
- [ ] Static RE of `KensingtonKonductor-TB` (Mach-O, 62 MB) or the Windows
      Konductor from `msi/cab1` for the authoritative opcode table.
      Note: `wine` is not installed, so the Windows daemon cannot be run
      locally for a live capture.
