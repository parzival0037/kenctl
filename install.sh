#!/usr/bin/env bash
# Host setup for kenctl:
#   * grant the desktop user access to uinput
#   * install the release binary and a systemd user service
#
# Build first:
#   cargo build --release --locked
#
# Then run from the project directory:
#   sudo ./install.sh
#
# Safe to re-run.

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

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
RELEASE_BIN="${SCRIPT_DIR}/target/release/kenctl"
SERVICE_SRC="${SCRIPT_DIR}/packaging/kenctl.service"

if [[ ! -x "${RELEASE_BIN}" ]]; then
  echo "error: ${RELEASE_BIN} not found" >&2
  echo "       run 'cargo build --release --locked' first" >&2
  exit 1
fi
if [[ ! -f "${SERVICE_SRC}" ]]; then
  echo "error: ${SERVICE_SRC} not found" >&2
  exit 1
fi

TARGET_HOME="$(getent passwd "${TARGET_USER}" | cut -d: -f6)"
TARGET_GROUP="$(id -gn "${TARGET_USER}")"
TARGET_UID="$(id -u "${TARGET_USER}")"
RUNTIME_DIR="/run/user/${TARGET_UID}"
INSTALL_DIR="${TARGET_HOME}/.local/bin"
SERVICE_DIR="${TARGET_HOME}/.config/systemd/user"

as_user() {
  sudo -u "${TARGET_USER}" env \
    XDG_RUNTIME_DIR="${RUNTIME_DIR}" \
    DBUS_SESSION_BUS_ADDRESS="unix:path=${RUNTIME_DIR}/bus" \
    "$@"
}

UINPUT_RULE=/etc/udev/rules.d/99-kenctl-uinput.rules
MODULES_LOAD=/etc/modules-load.d/kenctl-uinput.conf

echo "==> target user: ${TARGET_USER} (${TARGET_HOME})"

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

groupadd -f input

# udev applies this on next boot; do it now for the running kernel.
chgrp input /dev/uinput
chmod 0660 /dev/uinput

WAS_IN_INPUT=0
if id -nG "${TARGET_USER}" | tr ' ' '\n' | grep -qx input; then
  WAS_IN_INPUT=1
fi

echo "==> adding ${TARGET_USER} to the input group"
usermod -aG input "${TARGET_USER}"

# Stop the old manual daemon / service before replacing the binary.
if [[ -S "${RUNTIME_DIR}/bus" ]]; then
  as_user systemctl --user stop kenctl.service 2>/dev/null || true
fi
if pgrep -u "${TARGET_USER}" -x kenctl >/dev/null 2>&1; then
  echo "==> stopping the running kenctl process"
  pkill -u "${TARGET_USER}" -x kenctl || true
  sleep 1
fi

echo "==> installing ${INSTALL_DIR}/kenctl"
install -d -m 0755 -o "${TARGET_USER}" -g "${TARGET_GROUP}" "${INSTALL_DIR}"
install -m 0755 -o "${TARGET_USER}" -g "${TARGET_GROUP}" "${RELEASE_BIN}" "${INSTALL_DIR}/kenctl"

echo "==> installing ${SERVICE_DIR}/kenctl.service"
install -d -m 0755 -o "${TARGET_USER}" -g "${TARGET_GROUP}" "${SERVICE_DIR}"
install -m 0644 -o "${TARGET_USER}" -g "${TARGET_GROUP}" "${SERVICE_SRC}" "${SERVICE_DIR}/kenctl.service"

if [[ -S "${RUNTIME_DIR}/bus" ]]; then
  echo "==> enabling the user service"
  as_user systemctl --user daemon-reload
  as_user systemctl --user enable kenctl.service

  if [[ ${WAS_IN_INPUT} -eq 1 ]]; then
    echo "==> starting the user service"
    if ! as_user systemctl --user restart kenctl.service; then
      echo "warning: service failed to start" >&2
      echo "         journalctl --user -u kenctl -n 30" >&2
    fi
    echo
    as_user systemctl --user --no-pager --full status kenctl.service || true
  else
    echo "==> ${TARGET_USER} was just added to the input group"
    echo "    log out and back in, then start the service:"
    echo "      systemctl --user start kenctl"
  fi
else
  echo
  echo "No active systemd user session found. Enable the service after login:"
  echo "  systemctl --user daemon-reload"
  echo "  systemctl --user enable --now kenctl.service"
fi

echo
echo "Done."
echo "  /dev/uinput -> $(stat -c '%A %U:%G' /dev/uinput)"
echo "  binary      -> ${INSTALL_DIR}/kenctl"
echo "  service     -> ${SERVICE_DIR}/kenctl.service"
echo
echo "If you were just added to the input group, log out and back in, then:"
echo "  systemctl --user restart kenctl"
echo
echo "Logs:  journalctl --user -u kenctl -f"
echo "UI:    http://127.0.0.1:3789/"
