#!/usr/bin/env bash
# Installs OmniMonitor as a systemd service: a dedicated system user, the
# release binary + frontend under /opt/omnimonitor, data under
# /var/lib/omnimonitor, and packaging/omnimonitor.service enabled (not
# started - see the printed instructions at the end).
#
# Run from a built checkout (./scripts/build-all.sh first, or this
# script will offer to run it for you) as root:
#   sudo ./scripts/install.sh
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

if [[ $EUID -ne 0 ]]; then
    echo "This installs a system service and system user - run it with sudo." >&2
    exit 1
fi

BINARY="$ROOT/target/release/omni-server"
FRONTEND_DIST="$ROOT/frontend/dist"

if [[ ! -x "$BINARY" || ! -d "$FRONTEND_DIST" ]]; then
    echo "No release build found yet."
    read -r -p "Run ./scripts/build-all.sh now? [y/N] " answer
    if [[ "$answer" =~ ^[Yy]$ ]]; then
        # build-all.sh calls npm/cargo as the invoking user, not root -
        # drop privileges for the build, then continue installing as root.
        if [[ -n "${SUDO_USER:-}" ]]; then
            sudo -u "$SUDO_USER" "$ROOT/scripts/build-all.sh"
        else
            "$ROOT/scripts/build-all.sh"
        fi
    else
        echo "Build it first, then re-run this script." >&2
        exit 1
    fi
fi

INSTALL_DIR=/opt/omnimonitor
DATA_DIR=/var/lib/omnimonitor
SERVICE_USER=omnimonitor

if ! id "$SERVICE_USER" >/dev/null 2>&1; then
    echo "Creating system user '$SERVICE_USER'..."
    useradd --system --no-create-home --shell /usr/sbin/nologin "$SERVICE_USER"
fi

if getent group video >/dev/null 2>&1; then
    usermod -a -G video "$SERVICE_USER"
else
    echo "Warning: no 'video' group found - USB camera device permissions may need manual setup." >&2
fi

echo "Installing to $INSTALL_DIR..."
mkdir -p "$INSTALL_DIR/bin" "$INSTALL_DIR/frontend"
cp "$BINARY" "$INSTALL_DIR/bin/omni-server"
rm -rf "$INSTALL_DIR/frontend/dist"
cp -r "$FRONTEND_DIST" "$INSTALL_DIR/frontend/dist"
chown -R root:root "$INSTALL_DIR"
chmod -R a+rX "$INSTALL_DIR"

echo "Setting up data directory $DATA_DIR..."
mkdir -p "$DATA_DIR"
chown -R "$SERVICE_USER:$SERVICE_USER" "$DATA_DIR"
chmod 750 "$DATA_DIR"

echo "Installing systemd unit..."
cp "$ROOT/packaging/omnimonitor.service" /etc/systemd/system/omnimonitor.service
systemctl daemon-reload
systemctl enable omnimonitor.service

# Re-running this script (e.g. after `git pull` + a rebuild to update)
# should just apply the update, not leave the old binary running - if
# the service was already active, restart it into what was just
# installed instead of requiring a separate manual step.
if systemctl is-active --quiet omnimonitor.service; then
    echo "Service was already running - restarting into the newly installed build..."
    systemctl restart omnimonitor.service
    cat <<EOF

Updated and restarted. Tail the log with:

    sudo journalctl -u omnimonitor -f
EOF
else
    cat <<EOF

Installed. Not started yet - start it with:

    sudo systemctl start omnimonitor
    sudo journalctl -u omnimonitor -f

The web UI will be on http://<this-host>:8090/ - check the journal for
the auto-generated admin and RTSP passwords on first start (printed
exactly once), or set OMNI_ADMIN_PASSWORD/OMNI_RTSP_PASSWORD in
/etc/systemd/system/omnimonitor.service before the first start to pick
your own. After editing that file, run:

    sudo systemctl daemon-reload
    sudo systemctl restart omnimonitor

For HTTPS, see packaging/Caddyfile.example or packaging/nginx.conf.example.
EOF
fi
