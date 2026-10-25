use crate::{KENSINGTON_VID, VIRTUAL_DEVICE_NAME};
use serde::Serialize;
use std::path::PathBuf;

/// Known Kensington product ids -> model name.
///
/// Source: `supportTrackballs` extracted from the Konnect frontend bundle
/// (see `docs/trackball-architecture.md`).
pub const SUPPORTED_PIDS: &[(u16, &str)] = &[
    (0x8018, "Expert Mouse Wireless Trackball"),
    (0x8019, "Expert Mouse Wireless Trackball (JP)"),
    (0x8002, "Orbit Wireless Mobile Trackball 2.4G"),
    (0x2041, "SlimBlade Trackball"),
    (0x80bc, "SlimBlade Trackball (alt)"),
    (0x1020, "Expert Mouse Wired Trackball"),
    (0x2048, "Orbit Trackball with Scroll Ring"),
    (0x80a6, "Orbit Wireless Trackball with Scroll Ring"),
    (0x80a7, "Orbit Wireless Trackball with Scroll Ring (alt)"),
    (0x80a8, "Orbit Wireless Trackball with Scroll Ring (alt2)"),
    (0x807b, "Orbit Fusion Wireless Trackball"),
    (0x809d, "Pro Fit Ergo Vertical Wired Trackball"),
    (0x80c1, "Orbit Wireless Mobile Trackball 2.4G & BT"),
    (0x80c2, "Orbit Wireless Mobile Trackball 2.4G & BT (alt)"),
    (0x80c3, "Orbit Wireless Mobile Trackball 2.4G & BT (alt2)"),
    (0x80d4, "SlimBlade Pro Trackball"),
    (0x80d5, "SlimBlade Pro Trackball (alt)"),
    (0x80d6, "SlimBlade Pro Trackball (alt2)"),
    (0x80d7, "SlimBlade Pro Trackball (alt3)"),
    (0x80fa, "Pro Fit Ergo TB450 Trackball"),
    (0x80fb, "Pro Fit Ergo TB450 Trackball (alt)"),
    (0x80fc, "Pro Fit Ergo TB450 Trackball (alt2)"),
    (0x80fd, "Pro Fit Ergo TB550 Trackball"),
    (0x80fe, "Pro Fit Ergo TB550 Trackball (alt)"),
    (0x80ff, "Pro Fit Ergo TB550 Trackball (alt2)"),
    (0x8168, "Expert Mouse TB800 EQ Trackball"),
    (0x8169, "Expert Mouse TB800 EQ Trackball (alt)"),
    (0x816a, "Expert Mouse TB800 EQ Trackball (alt2)"),
    (0x8213, "Pro Fit Ergo TB675 Vertical Wireless Trackball"),
    (0x8214, "Pro Fit Ergo TB675 Vertical Wireless Trackball (alt)"),
    (0x8220, "Pro Fit Ergo TB675 Vertical Wireless Trackball (alt2)"),
    (0x8215, "Pro Fit Ergo TB675 Vertical Wired Trackball"),
    (0x823b, "Expert Mouse TB900 EQ Trackball"),
    (0x8156, "MY230 EQ Rechargeable Mouse"),
    (0x8158, "MY230 EQ Rechargeable Mouse (alt)"),
    (0xfa67, "Pro Fit Ergo Vertical Mouse"),
    (0x805b, "Pro Fit Ergo Wireless Mouse"),
    (0x0040, "Pro Fit Ergo Wireless Mouse (alt)"),
    (0x08a0, "Pro Fit Ergo Wired Mouse"),
    (0x8068, "Pro Fit Ergo Vertical Trackball"),
    (0x8112, "MY430 Ergo Wireless Mouse"),
    (0x8138, "MY430 Ergo Wireless Mouse (alt)"),
    (0x8188, "MY430 Ergo Wireless Mouse (alt2)"),
    (0x8143, "Pro Fit Ergo MY630 EQ Rechargeable Mouse"),
    (0x8145, "Pro Fit Ergo MY630 EQ Rechargeable Mouse (alt)"),
    (0x4102, "Pro Fit Left-Handed Ergo Wireless Mouse"),
    (0x8207, "MY435 EQ Rechargeable Mid-Size Mouse"),
    (0x8208, "MY435 EQ Rechargeable Mid-Size Mouse (alt)"),
    (0x8209, "MY435 EQ Wired Mid-Size Mouse"),
];

/// Physical button layout for models where the evdev names do not match the
/// printed positions. Order is clockwise from the top-left button.
pub fn model_buttons(pid: u16) -> Option<&'static [&'static str]> {
    match pid {
        // Expert Mouse Wireless / JP / Wired: four buttons around the ball.
        0x8018 | 0x8019 | 0x1020 => {
            Some(&["BTN_MIDDLE", "BTN_SIDE", "BTN_RIGHT", "BTN_LEFT"])
        }
        _ => None,
    }
}

/// User-facing name for a button on a known model.
///
/// This is display-only: profiles and the engine keep using the real evdev
/// code (`BTN_SIDE`, …) so existing mappings remain valid.
pub fn button_label(pid: u16, code: &str) -> Option<&'static str> {
    model_buttons(pid)?;
    match code {
        "BTN_MIDDLE" => Some("BTN_TOP_LEFT"),
        "BTN_SIDE" => Some("BTN_TOP_RIGHT"),
        "BTN_RIGHT" => Some("BTN_BOTTOM_RIGHT"),
        "BTN_LEFT" => Some("BTN_BOTTOM_LEFT"),
        _ => None,
    }
}

pub fn model_for(pid: u16) -> String {
    SUPPORTED_PIDS
        .iter()
        .find(|(p, _)| *p == pid)
        .map(|(_, n)| (*n).to_string())
        .unwrap_or_else(|| format!("Kensington device (0x{pid:04X})"))
}

#[derive(Debug, Clone, Serialize)]
pub struct FoundDevice {
    pub path: PathBuf,
    /// evdev node name, e.g. "Kensington Expert Wireless TB Mouse".
    pub name: String,
    pub product_id: u16,
    pub model: String,
    /// True once the engine has grabbed this node.
    #[serde(default)]
    pub managed: bool,
}

/// Discover every Kensington input node currently present.
///
/// Nodes whose name starts with [`VIRTUAL_DEVICE_NAME`] are skipped so the
/// engine can never re-capture its own output (which would deadlock the
/// desktop in an input loop).
pub fn scan() -> Vec<FoundDevice> {
    let mut found: Vec<FoundDevice> = Vec::new();

    for (path, dev) in evdev::enumerate() {
        let id = dev.input_id();
        if id.vendor() != KENSINGTON_VID {
            continue;
        }
        let name = dev.name().unwrap_or("").to_string();
        if name.starts_with(VIRTUAL_DEVICE_NAME) {
            continue;
        }
        let product_id = id.product();
        found.push(FoundDevice {
            path,
            model: model_for(product_id),
            name: if name.is_empty() {
                model_for(product_id)
            } else {
                name
            },
            product_id,
            managed: false,
        });
    }

    found.sort_by(|a, b| a.path.cmp(&b.path));
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_pid_maps_to_a_model() {
        assert_eq!(model_for(0x8018), "Expert Mouse Wireless Trackball");
    }

    #[test]
    fn unknown_pid_is_labelled_not_panicking() {
        assert!(model_for(0x1234).contains("0x1234"));
    }

    #[test]
    fn table_has_no_duplicate_pids() {
        let mut seen = std::collections::HashSet::new();
        for (pid, _) in SUPPORTED_PIDS {
            assert!(seen.insert(*pid), "duplicate pid 0x{pid:04X}");
        }
    }

    #[test]
    fn scan_never_returns_our_virtual_nodes() {
        for d in scan() {
            assert!(!d.name.starts_with(VIRTUAL_DEVICE_NAME));
        }
    }

    #[test]
    fn expert_mouse_buttons_use_physical_labels() {
        assert_eq!(button_label(0x8018, "BTN_MIDDLE"), Some("BTN_TOP_LEFT"));
        assert_eq!(button_label(0x8018, "BTN_SIDE"), Some("BTN_TOP_RIGHT"));
        assert_eq!(button_label(0x8018, "BTN_RIGHT"), Some("BTN_BOTTOM_RIGHT"));
        assert_eq!(button_label(0x8018, "BTN_LEFT"), Some("BTN_BOTTOM_LEFT"));
        assert_eq!(button_label(0x8018, "BTN_EXTRA"), None);
    }

    #[test]
    fn unknown_models_keep_raw_button_names() {
        assert_eq!(button_label(0x2041, "BTN_SIDE"), None);
        assert!(model_buttons(0x2041).is_none());
    }

    #[test]
    fn expert_mouse_layout_has_four_buttons() {
        assert_eq!(model_buttons(0x8018).unwrap().len(), 4);
    }
}
