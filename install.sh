#!/usr/bin/env bash
# sideagent installer (Linux/GNOME v1). Run from repo root.
set -euo pipefail
cd "$(dirname "$0")"
. scripts/env.sh   # project-local toolchain (or use system cargo if present)

PREFIX="${PREFIX:-$HOME/.local/lib/sideagent}"
echo "==> building (release)"
cargo build --release

echo "==> installing binaries to $PREFIX"
mkdir -p "$PREFIX"
cp target/release/sideagentd target/release/sideagentctl "$PREFIX/"

echo "==> systemd user service"
mkdir -p "$HOME/.config/systemd/user"
sed "s|%h|$HOME|g" systemd/sideagentd.service > "$HOME/.config/systemd/user/sideagentd.service"
systemctl --user daemon-reload
systemctl --user enable --now sideagentd

echo "==> gnome extension"
XDG_CACHE_HOME="${XDG_CACHE_HOME:-$PWD/.dev/cache}" gnome-extensions pack extension --force
gnome-extensions install org.sideagent.shell-extension.zip --force || {
  echo "NOTE: extension installed on disk; GNOME loads new extensions at login."
  echo "      log out/in once, then: gnome-extensions enable org.sideagent"
}

echo
echo "==> config: copy config.example.toml to ~/.config/sideagent/config.toml, set provider + api_key, chmod 600"
echo "    then: systemctl --user restart sideagentd && sideagentctl status"
