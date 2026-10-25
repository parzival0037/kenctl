#!/usr/bin/env bash
# KenCtl one-line installer.
#
#   curl -fsSL https://raw.githubusercontent.com/parzival0037/kenctl/main/install-remote.sh | sudo bash
#
# Downloads the latest release, installs the binary and systemd user service
# for the invoking desktop user, and grants access to /dev/uinput.
#
# Optional environment:
#   KENCTL_VERSION=v0.1.0    install a specific release (default: latest)
#   KENCTL_ASSET_URL=...     override the release tarball URL
#   KENCTL_USER=name         target user when not invoked via sudo
#
# Safe to re-run.

set -euo pipefail

REPO="parzival0037/kenctl"
RAW_URL="https://raw.githubusercontent.com/${REPO}/main/install-remote.sh"

if [[ ${EUID} -ne 0 ]]; then
  echo "error: this installer needs root for /dev/uinput and systemd setup." >&2
  echo "       run:" >&2
  echo "         curl -fsSL ${RAW_URL} | sudo bash" >&2
  exit 1
fi

for cmd in curl tar; do
  if ! command -v "${cmd}" >/dev/null 2>&1; then
    echo "error: ${cmd} is required" >&2
    exit 1
  fi
done

TARGET_USER="${KENCTL_USER:-${SUDO_USER:-}}"
if [[ -z "${TARGET_USER}" || "${TARGET_USER}" == "root" ]]; then
  echo "error: cannot determine the desktop user." >&2
  echo "       run this with sudo from your normal account, or set KENCTL_USER=name." >&2
  exit 1
fi

TARGET_HOME="$(getent passwd "${TARGET_USER}" | cut -d: -f6)"
if [[ -z "${TARGET_HOME}" ]]; then
  echo "error: user ${TARGET_USER} not found" >&2
  exit 1
fi

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

case "$(uname -m)" in
  x86_64|amd64) ARCH=x86_64 ;;
  aarch64|arm64)
    echo "error: no prebuilt ARM64 release yet; use Method 2 (build from source)." >&2
    exit 1
    ;;
  *)
    echo "error: unsupported architecture: $(uname -m)" >&2
    echo "       use Method 2 (build from source)." >&2
    exit 1
    ;;
esac

ASSET="kenctl-${ARCH}-unknown-linux-gnu.tar.gz"
if [[ -n "${KENCTL_ASSET_URL:-}" ]]; then
  ASSET_URL="${KENCTL_ASSET_URL}"
elif [[ -n "${KENCTL_VERSION:-}" ]]; then
  VERSION="${KENCTL_VERSION#v}"
  ASSET_URL="https://github.com/${REPO}/releases/download/v${VERSION}/${ASSET}"
else
  ASSET_URL="https://github.com/${REPO}/releases/latest/download/${ASSET}"
fi

TMP="$(mktemp -d)"
trap 'rm -rf "${TMP}"' EXIT

echo "==> downloading ${ASSET_URL}"
if ! curl -fL --retry 3 --connect-timeout 10 -o "${TMP}/${ASSET}" "${ASSET_URL}"; then
  echo "error: download failed." >&2
  echo "       If no release is published yet, use Method 2:" >&2
  echo "       https://github.com/${REPO}#method-2--build-from-source" >&2
  exit 1
fi

if command -v sha256sum >/dev/null 2>&1 \
  && curl -fsSL --retry 3 -o "${TMP}/${ASSET}.sha256" "${ASSET_URL}.sha256" 2>/dev/null; then
  (cd "${TMP}" && sha256sum -c "${ASSET}.sha256")
else
  echo "==> no checksum available; skipping verification"
fi

mkdir -p "${TMP}/pkg"
tar -xzf "${TMP}/${ASSET}" -C "${TMP}/pkg"
if [[ ! -x "${TMP}/pkg/kenctl" || ! -f "${TMP}/pkg/kenctl.service" ]]; then
  echo "error: release archive is missing kenctl or kenctl.service" >&2
  exit 1
fi

UINPUT_RULE=/etc/udev/rules.d/99-kenctl-uinput.rules
MODULES_LOAD=/etc/modules-load.d/kenctl-uinput.conf

echo "==> target user: ${TARGET_USER} (${TARGET_HOME})"
groupadd -f input

echo "==> configuring uinput"
rm -f /etc/udev/rules.d/99-konnect-uinput.rules \
      /etc/modules-load.d/konnect-uinput.conf

cat >"${UINPUT_RULE}" <<'EOF'
# kenctl: allow the `input` group to create virtual devices for remapping
KERNEL=="uinput", SUBSYSTEM=="misc", GROUP="input", MODE="0660", OPTIONS+="static_node=uinput"
EOF

echo "uinput" >"${MODULES_LOAD}"
modprobe uinput 2>/dev/null || true

udevadm control --reload-rules
udevadm trigger --sysname-match=uinput 2>/dev/null || true
udevadm trigger --subsystem-match=misc 2>/dev/null || true
udevadm settle 2>/dev/null || true

if [[ ! -e /dev/uinput ]]; then
  echo "error: /dev/uinput is still missing; is the uinput module available?" >&2
  exit 1
fi

chgrp input /dev/uinput
chmod 0660 /dev/uinput

WAS_IN_INPUT=0
if id -nG "${TARGET_USER}" | tr ' ' '\n' | grep -qx input; then
  WAS_IN_INPUT=1
fi

echo "==> adding ${TARGET_USER} to the input group"
usermod -aG input "${TARGET_USER}"

# Stop the old service and any manual instance before replacing the binary.
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
install -m 0755 -o "${TARGET_USER}" -g "${TARGET_GROUP}" \
  "${TMP}/pkg/kenctl" "${INSTALL_DIR}/kenctl"

echo "==> installing ${SERVICE_DIR}/kenctl.service"
install -d -m 0755 -o "${TARGET_USER}" -g "${TARGET_GROUP}" "${SERVICE_DIR}"
install -m 0644 -o "${TARGET_USER}" -g "${TARGET_GROUP}" \
  "${TMP}/pkg/kenctl.service" "${SERVICE_DIR}/kenctl.service"

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
    echo "    log out and back in; the service will start automatically."
  fi
else
  echo
  echo "No active systemd user session found. Enable the service after login:"
  echo "  systemctl --user daemon-reload"
  echo "  systemctl --user enable --now kenctl.service"
fi

echo
echo "Done."
echo "  version     -> $("${INSTALL_DIR}/kenctl" --version)"
echo "  binary      -> ${INSTALL_DIR}/kenctl"
echo "  service     -> ${SERVICE_DIR}/kenctl.service"
echo "  /dev/uinput -> $(stat -c '%A %U:%G' /dev/uinput)"
echo
echo "If you were just added to the input group, log out and back in, then:"
echo "  systemctl --user restart kenctl"
echo
echo "Logs:  journalctl --user -u kenctl -f"
echo "UI:    http://127.0.0.1:3789/"
