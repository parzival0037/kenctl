//! KenCtl — host-side configuration app for Kensington trackballs.
//!
//! Layout:
//!   devices   – discovery of supported Kensington input nodes
//!   profiles  – profile model + on-disk persistence
//!   engine    – evdev grab -> transform -> uinput re-injection
//!   server    – localhost REST API + embedded web UI
//!   hardware  – vendor HID (hidraw) channel probe

pub mod client;
pub mod devices;
pub mod engine;
pub mod hardware;
pub mod profiles;
pub mod proto;
pub mod server;
pub mod state;

/// Vendor id for all Kensington devices.
pub const KENSINGTON_VID: u16 = 0x047d;

/// Name prefix used for every virtual device we create. Discovery skips these
/// so the engine can never grab (and feed back into) its own output.
pub const VIRTUAL_DEVICE_NAME: &str = "KenCtl Virtual";
