#!/usr/bin/env bash
# aisle installer (Linux/GNOME v1). Run from repo root.
set -euo pipefail
cd "$(dirname "$0")"
. scripts/env.sh   # project-local toolchain (or use system cargo if present)

PREFIX="${PREFIX:-$HOME/.local/lib/aisle}"

echo "==> migrating from the old 'sideagent' name if present"
for pair in "$HOME/.config/sideagent:$HOME/.config/aisle"             "$HOME/.local/share/sideagent:$HOME/.local/share/aisle"; do
  old_dir=${pair%%:*}; new_dir=${pair##*:}
  [ -d "$old_dir" ] && [ ! -d "$new_dir" ] && mv "$old_dir" "$new_dir"
done
systemctl --user stop sideagentd.service 2>/dev/null || true
systemctl --user disable sideagentd.service 2>/dev/null || true
rm -f "$HOME/.config/systemd/user/sideagentd.service"
gnome-extensions uninstall org.sideagent 2>/dev/null || true
rm -f "$HOME/.local/share/glib-2.0/schemas/org.gnome.shell.extensions.sideagent.gschema.xml"
rm -f "$HOME/.local/bin/sideagentctl" "$HOME/.local/bin/side-ask"
rm -rf "$HOME/.local/lib/sideagent"
echo "==> building (release)"
cargo build --release

echo "==> installing binaries to $PREFIX"
mkdir -p "$PREFIX"
cp target/release/aisled target/release/aislectl target/release/aisle "$PREFIX/"
# put the user-facing commands on PATH (~/.local/bin is on PATH on most distros)
mkdir -p "$HOME/.local/bin"
ln -sf "$PREFIX/aislectl" "$HOME/.local/bin/aislectl"
ln -sf "$PREFIX/aisle" "$HOME/.local/bin/aisle"
ln -sf "$PREFIX/aisle" "$HOME/.local/bin/psst"   # alias, same binary

echo "==> systemd user service"
mkdir -p "$HOME/.config/systemd/user"
sed "s|%h|$HOME|g" systemd/aisle.service > "$HOME/.config/systemd/user/aisle.service"
systemctl --user daemon-reload
systemctl --user enable --now aisle

echo "==> gnome extension"
# register the extension's gsettings schema in the user dir too, so
# `gsettings set org.gnome.shell.extensions.aisle ...` works from CLI
mkdir -p "$HOME/.local/share/glib-2.0/schemas"
cp extension/schemas/org.gnome.shell.extensions.aisle.gschema.xml \
   "$HOME/.local/share/glib-2.0/schemas/"
glib-compile-schemas "$HOME/.local/share/glib-2.0/schemas"
XDG_CACHE_HOME="${XDG_CACHE_HOME:-$PWD/.dev/cache}" gnome-extensions pack extension --force
gnome-extensions install org.aisle.shell-extension.zip --force || {
  echo "NOTE: extension installed on disk; GNOME loads new extensions at login."
  echo "      log out/in once, then: gnome-extensions enable org.aisle"
}

echo
echo "==> config: copy config.example.toml to ~/.config/aisle/config.toml, set provider + api_key, chmod 600"
echo "    then: systemctl --user restart aisle && aislectl status"
