use serde::Serialize;
use std::path::PathBuf;

/// Honest report on the vendor HID (config) channel.
///
/// The 32-byte vendor output report exists in the report descriptor, but on
/// this device every write is rejected at the USB level (see
/// `docs/RE-STATUS.md`). Rather than pretend to control hardware we surface
/// exactly what is and is not reachable.
#[derive(Debug, Clone, Serialize)]
pub struct HardwareStatus {
    /// hidraw node for the vendor/config interface, if one exists.
    pub config_node: Option<String>,
    /// Whether that node can be opened for reading.
    pub hidraw_readable: bool,
    /// usbfs node for the same USB device — required for a control
    /// `SET_REPORT`, which is the transport that bypasses the failing
    /// interrupt-OUT pipe.
    pub usbfs_node: Option<String>,
    /// Whether we may open usbfs for writing (root or a udev rule).
    pub usbfs_writable: bool,
    /// Always false until the opcode table is recovered from the vendor
    /// binaries or a live capture.
    pub protocol_recovered: bool,
    pub message: String,
}

impl HardwareStatus {
    pub fn probe() -> Self {
        let Some((hid_dir, hidraw)) = find_config_interface() else {
            return HardwareStatus {
                config_node: None,
                hidraw_readable: false,
                usbfs_node: None,
                usbfs_writable: false,
                protocol_recovered: false,
                message: "No Kensington config interface present (device unplugged).".into(),
            };
        };

        let hidraw_readable = hidraw
            .as_ref()
            .and_then(|p| std::fs::OpenOptions::new().read(true).open(p).ok())
            .is_some();

        let usbfs = usbfs_path(&hid_dir);
        let usbfs_writable = usbfs.as_ref().is_some_and(|p| {
            std::fs::OpenOptions::new()
                .write(true)
                .open(p)
                .is_ok()
        });

        let message = if !usbfs_writable {
            "Hardware (on-device) settings are not reachable yet: the vendor \
             interrupt-OUT pipe rejects every payload, so a control SET_REPORT \
             over usbfs is required, and usbfs is not writable by this user. \
             Run ./install.sh to grant it. Button, pointer and scroll \
             configuration do not need this — they run host-side."
                .to_string()
        } else {
            "usbfs is writable, but the vendor opcode table has not been \
             recovered yet, so no on-device settings are exposed. See \
             docs/RE-STATUS.md."
                .to_string()
        };

        HardwareStatus {
            config_node: hidraw.map(|p| p.display().to_string()),
            hidraw_readable,
            usbfs_node: usbfs.map(|p| p.display().to_string()),
            usbfs_writable,
            protocol_recovered: false,
            message,
        }
    }
}

/// Locate the vendor/config HID interface: report descriptor carries usage
/// page 0xFF00 and no report ids (the mouse interface always has them).
fn find_config_interface() -> Option<(PathBuf, Option<PathBuf>)> {
    let base = std::path::Path::new("/sys/bus/hid/devices");
    let entries = std::fs::read_dir(base).ok()?;

    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.starts_with("0003:047D:") && !name.starts_with("0003:047d:") {
            continue;
        }
        let desc = std::fs::read(entry.path().join("report_descriptor")).unwrap_or_default();
        let vendor_page = desc.windows(3).any(|w| w == [0x06, 0x00, 0xff]);
        let has_report_ids = desc.windows(2).any(|w| w == [0x85, 0x01]);
        if !vendor_page || has_report_ids {
            continue;
        }

        let hidraw_dir = entry.path().join("hidraw");
        let hidraw = std::fs::read_dir(&hidraw_dir)
            .ok()
            .and_then(|mut d| d.next())
            .and_then(|e| e.ok())
            .map(|e| PathBuf::from("/dev").join(e.file_name()));
        return Some((entry.path(), hidraw));
    }
    None
}

/// Walk up from the HID node to the USB device directory, which carries
/// `busnum` / `devnum`.
///
/// Layouts differ by kernel: some expose a `device` symlink to the interface,
/// others place the HID node directly under the interface directory. Walking
/// parents until `busnum` is found covers both.
fn usbfs_path(hid_dir: &std::path::Path) -> Option<PathBuf> {
    let mut node = std::fs::canonicalize(hid_dir).ok()?;
    for _ in 0..5 {
        let busnum = node.join("busnum");
        let devnum = node.join("devnum");
        if let (Ok(b), Ok(d)) = (
            std::fs::read_to_string(&busnum),
            std::fs::read_to_string(&devnum),
        ) {
            let bus: u32 = b.trim().parse().ok()?;
            let dev: u32 = d.trim().parse().ok()?;
            return Some(PathBuf::from(format!("/dev/bus/usb/{bus:03}/{dev:03}")));
        }
        node = node.parent()?.to_path_buf();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_never_panics() {
        let status = HardwareStatus::probe();
        // Whatever the machine looks like, we must produce a usable message.
        assert!(!status.message.is_empty());
        assert!(!status.protocol_recovered);
    }
}
