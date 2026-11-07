#!/usr/bin/env bash
# Live-test the sideagent extension in a HEADLESS nested gnome-shell,
# isolated from the user's real session (own dbus/dconf/dirs).
# Invoke as:  env XDG_* overrides dbus-run-session -- bash .dev/nested-test.sh
set -u
S=$(cd "$(dirname "$0")/.." && pwd)
N=$S/.dev/nested
rm -rf "$N"; mkdir -p "$N/config" "$N/data" "$N/runtime" "$N/cache"
chmod 700 "$N/runtime"

# nested shell reads extensions from ITS XDG_DATA_HOME
mkdir -p "$N/data/gnome-shell/extensions/org.sideagent"
cp -a $S/extension/. "$N/data/gnome-shell/extensions/org.sideagent/"
glib-compile-schemas "$N/data/gnome-shell/extensions/org.sideagent/schemas"

export SIDEAGENT_CONFIG=$S/.dev/config.toml SIDEAGENT_DATA=$N/data SIDEAGENT_RUNTIME=$N/runtime
export RUST_LOG=info

python3 $S/.dev/mockprovider.py 38411 > $N/mock.log 2>&1 & MOCK=$!
$S/target/debug/sideagentd > $N/daemon.log 2>&1 & D=$!
sleep 1

gsettings set org.gnome.shell enabled-extensions "['org.sideagent']" || echo "WARN: gsettings failed"

gnome-shell --headless --no-x11 --virtual-monitor 1280x800 > $N/shell.log 2>&1 & GS=$!
sleep 7
kill -0 $GS 2>/dev/null && echo "shell: alive" || echo "shell: DEAD (see shell.log)"

$S/target/debug/sideagentctl selftest > $N/ctl.log 2>&1; echo "ctl selftest: exit=$?"
sleep 9

echo "===== EVIDENCE ====="
echo "--- shell (extension lines):"
grep -iE 'sideagent|Could not load|CRITICAL' $N/shell.log | head -15
echo "--- daemon:"
grep -iE 'frontend registered|captured|asked|no bus' $N/daemon.log | head -8
echo "--- mock:"
cat $N/mock.log
echo "--- ctl:"
cat $N/ctl.log

kill $GS $D $MOCK 2>/dev/null
wait 2>/dev/null
echo "===== DONE ====="
