#!/usr/bin/env bash
# SPDX-License-Identifier: MIT
#
# Installs the COSMIC panel applet, the app icons and the ProjectGui launcher into the user's home (no root).
# Build first: cargo build --release (in applet/). `projectgui` itself comes from the release installer.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PREFIX="${PREFIX:-$HOME/.local}"
APP_ID="io.github.Gu1L3RM3.ProjectGui"
BIN_DIR="$PREFIX/bin"
APPS_DIR="$PREFIX/share/applications"
ICON_DIR="$PREFIX/share/icons/hicolor"

applet="$ROOT/applet/target/release/projectgui-applet"
[[ -x "$applet" ]] || { echo "error: $applet not found; run 'cargo build --release' in applet/ first" >&2; exit 1; }

projectgui="$(command -v projectgui || true)"
[[ -n "$projectgui" ]] || projectgui="$BIN_DIR/projectgui"
[[ -x "$projectgui" ]] || echo "warning: projectgui not found; install it first (see README)" >&2

mkdir -p "$BIN_DIR" "$APPS_DIR" "$ICON_DIR/scalable/apps"
install -m755 "$applet" "$BIN_DIR/projectgui-applet"
install -m644 "$ROOT/assets/$APP_ID.svg" "$ICON_DIR/scalable/apps/$APP_ID.svg"
install -m644 "$ROOT/assets/$APP_ID-symbolic.svg" "$ICON_DIR/scalable/apps/$APP_ID-symbolic.svg"

# cosmic-panel and launchers run with a minimal PATH that lacks ~/.local/bin, so Exec gets absolute paths.
sed "s|^Exec=.*|Exec=$BIN_DIR/projectgui-applet|" "$ROOT/applet/data/$APP_ID.Applet.desktop" \
    > "$APPS_DIR/$APP_ID.Applet.desktop"
sed "s|^Exec=.*|Exec=$projectgui gui|" "$ROOT/assets/$APP_ID.desktop" > "$APPS_DIR/$APP_ID.desktop"

command -v update-desktop-database >/dev/null && update-desktop-database "$APPS_DIR" >/dev/null 2>&1 || true
command -v gtk-update-icon-cache >/dev/null && gtk-update-icon-cache -f -t "$ICON_DIR" >/dev/null 2>&1 || true

cat <<MSG
Installed projectgui-applet, the ProjectGui launcher and icons under $PREFIX.
Add the applet: Settings -> Desktop -> Panel -> Configure panel applets -> Add applet -> "ProjectGui".
If it is not listed yet: systemctl --user restart cosmic-panel (or log out and in).
MSG
