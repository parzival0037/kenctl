#!/usr/bin/env bash
# One-time host setup for kenctl: grant the desktop user access to uinput so
# the input engine can re-inject remapped buttons, keys and scroll deltas.
#
# Run from the project directory:
#   sudo ./install.sh
#
# Safe to re-run. It does not install or start any service yet.

set -euo pipefail

if [[ ${EUID} -ne 0 ]]; then
  echo "error: run as root: sudo $0" >&2
  exit 1
fi

TARGET_USER="${SUDO_USER:-}"
if [[ -z "${TARGET_USER}" || "${TARGET_USER}" == "root" ]]; then
  echo "error: run this with sudo from your normal desktop account, not as root directly" >&2
  exit 1
fi

UINPUT_RULE=/etc/udev/rules.d/99-kenctl-uinput.rules
MODULES_LOAD=/etc/modules-load.d/kenctl-uinput.conf

echo "==> target user: ${TARGET_USER}"

# Older dev builds used the konnect-* names; remove them so only one rule loads.
rm -f /etc/udev/rules.d/99-konnect-uinput.rules \
      /etc/modules-load.d/konnect-uinput.conf

echo "==> writing ${UINPUT_RULE}"
cat >"${UINPUT_RULE}" <<'EOF'
# kenctl: allow the `input` group to create virtual devices for remapping
KERNEL=="uinput", SUBSYSTEM=="misc", GROUP="input", MODE="0660", OPTIONS+="static_node=uinput"
EOF

echo "==> making uinput load at boot"
echo "uinput" >"${MODULES_LOAD}"
modprobe uinput 2>/dev/null || true

echo "==> reloading udev rules"
udevadm control --reload-rules
udevadm trigger --sysname-match=uinput 2>/dev/null || true
udevadm trigger --subsystem-match=misc 2>/dev/null || true
udevadm settle 2>/dev/null || true

if [[ ! -e /dev/uinput ]]; then
  echo "error: /dev/uinput is still missing; is the uinput module available?" >&2
  exit 1
fi

# udev applies this on next boot; do it now for the running kernel.
chgrp input /dev/uinput
chmod 0660 /dev/uinput

echo "==> adding ${TARGET_USER} to the input group"
groupadd -f input
usermod -aG input "${TARGET_USER}"

echo
echo "Done."
echo "  /dev/uinput -> $(stat -c '%A %U:%G' /dev/uinput)"
echo
echo "Next steps (no logout needed if you were already in the input group):"
echo "  ./target/debug/kenctl proto button --from BTN_SIDE --to KEY_C --modifier KEY_LEFTCTRL"
echo "  ./target/debug/kenctl proto scroll --speed 2.0"
echo
echo "If opening /dev/uinput still fails, log out and back in once so the"
echo "new group membership takes effect."
