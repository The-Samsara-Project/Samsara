#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-2.0-or-later
# Copyright (C) 2026 Harsh Nikarsa
#
# Boot the ISO under QEMU, drive the installer with synthetic key presses, and
# read the results back off the framebuffer.
#
# The installer's diagnostics menu is the only path that runs chello and
# termiostst. A boot-spawned chello run does not: three tty bugs survived a
# reported green suite for exactly that reason, so the tests have to be run the
# way a user runs them -- by driving the menu, not by spawning the programs
# directly.
#
# The results also exist only as pixels. The menu draws pass/fail into the
# framebuffer and says nothing to the serial console, so scripts/fbshot.py decodes
# the screendumps back into text. Without that step a headless run produces
# screenshots of the results rather than the results.
#
#   ./scripts/run-selftests.sh [outdir]
#
# Exits non-zero if any test reported FAIL, so this is usable unattended.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT="${1:-$ROOT/target/selftests}"
MON="$OUT/monitor.sock"
LOG="$OUT/console.log"
SHOTS="$OUT/shots"
CTL="$OUT/monitor.in"

rm -rf "$OUT"
mkdir -p "$SHOTS"

[ -f "$ROOT/samsara.iso" ] || { echo "no samsara.iso -- run make first" >&2; exit 1; }

qemu-system-x86_64 \
    -cdrom "$ROOT/samsara.iso" \
    -cpu qemu64,+rdseed,+rdrand \
    -serial "file:$LOG" \
    -display none \
    -no-reboot \
    -monitor "unix:$MON,server,nowait" &
QEMU_PID=$!

mkfifo "$CTL"

cleanup() {
    exec 3>&- 2>/dev/null || true
    kill "$MONITOR_PID" 2>/dev/null || true
    kill "$QEMU_PID" 2>/dev/null || true
    wait "$QEMU_PID" 2>/dev/null || true
    rm -f "$CTL"
}
trap cleanup EXIT

python3 "$ROOT/scripts/monitor.py" "$MON" "$CTL" "$OUT/monitor.log" &
MONITOR_PID=$!

# Opened for writing, which blocks until the monitor helper opens it for reading.
# The helper is started first, so this is a rendezvous rather than a wait: if the
# helper fails to start, this blocks forever, which the boot wait below turns
# into a diagnosable failure instead of a hang with no explanation.
exec 3>"$CTL"

mon() { echo "$1" >&3; }

# Wait for the guest to reach a known state rather than sleeping a guessed time.
# Boot time varies with host load, and a fixed wait is a race that shows up as an
# occasional lost keystroke -- which reads as a bug in the thing being tested.
wait_for() {
    local pat=$1 secs=${2:-90} waited=0
    while [ "$waited" -lt "$((secs * 2))" ]; do
        grep -qE "$pat" "$LOG" 2>/dev/null && return 0
        sleep 0.5
        waited=$((waited + 1))
    done
    return 1
}

# `screendump` is queued by the monitor and written whenever QEMU gets round to
# it, so the file's existence is not a synchronisation point -- a file caught
# mid-write decodes to a half-drawn screen. Wait for the size to stop changing.
shot() {
    local name=$1 ppm="$SHOTS/$1.ppm" last=-1 stable=0 size=0
    rm -f "$ppm"
    mon "screendump $ppm"
    for _ in $(seq 1 100); do
        sleep 0.2
        [ -f "$ppm" ] || continue
        size=$(stat -c %s "$ppm" 2>/dev/null || echo 0)
        if [ "$size" -gt 0 ] && [ "$size" = "$last" ]; then
            stable=$((stable + 1))
            [ "$stable" -ge 3 ] && return 0
        else
            stable=0
        fi
        last=$size
    done
    echo "warning: $name did not settle" >&2
    return 1
}

key() { mon "sendkey $1"; sleep 0.4; }

text() { python3 "$ROOT/scripts/fbshot.py" "$1" 2>/dev/null; }

wait_for "setup wizard online" 90 || { echo "installer did not start" >&2; tail -20 "$LOG"; exit 1; }
sleep 3
shot 00-intro

# Dismiss the Introduction screen. It ends in `wait_key()` -- "press any key to
# continue" -- so *every* run has to spend one keypress here before the main menu
# exists to be navigated. Skipping it is not a harmless off-by-one: the key meant
# for the main menu is consumed here, and the main menu then opens whatever was
# selected by default, which is item 1. The symptom is a run that reaches a
# screen, just not the one that was asked for.
key ret
sleep 1.5
shot 00-main

# Main menu -> Diagnostics, by digit shortcut. The installer binds 1-8 to the
# top-level items, so no cursor positioning is needed and the sequence does not
# depend on the selection having been initialised to a known row.
key 6
sleep 1.5
shot 01-diag

# Row 0 is "run all self-tests" and is selected on entry.
key ret

# Poll the decoded screen for the results table. Sleeping a guessed number of
# seconds instead is the flake this script exists to remove: seven short programs
# finish in well under a second, and the only thing that reliably distinguishes
# "done" from "still drawing" is the screen itself.
done=0
for _ in $(seq 1 120); do
    sleep 1
    shot probe >/dev/null 2>&1 || true
    if text "$SHOTS/probe.ppm" | grep -qE 'PASS|FAIL'; then
        # Require two consecutive agreeing reads, so a half-drawn table mid-paint
        # is not mistaken for the finished one.
        sleep 1
        shot probe2 >/dev/null 2>&1 || true
        if text "$SHOTS/probe2.ppm" | grep -qE 'PASS|FAIL'; then
            done=1
            break
        fi
    fi
done
cp "$SHOTS/probe2.ppm" "$SHOTS/03-results.ppm" 2>/dev/null ||
    cp "$SHOTS/probe.ppm" "$SHOTS/03-results.ppm" 2>/dev/null || true
rm -f "$SHOTS/probe.ppm" "$SHOTS/probe2.ppm"

key esc
sleep 0.5
key q
sleep 1
mon "quit" >/dev/null 2>&1 || true
sleep 1

echo "=== installer screen, decoded from the framebuffer ==="
text "$SHOTS/03-results.ppm" | grep -vE '^[[:space:]|]*$' || true

echo
echo "=== serial console ==="
grep -E "^\[" "$LOG" | grep -vE "^\[inputd\] (scan|fwd)" || true

fails=$(text "$SHOTS/03-results.ppm" 2>/dev/null | grep -c "FAIL" || true)
if [ "$done" -ne 1 ]; then
    echo
    echo "=== the results table never appeared; the run did not finish ==="
    exit 1
fi
if [ "$fails" -gt 0 ]; then
    echo
    echo "=== $fails FAIL marker(s) on the results screen ==="
    exit 1
fi
echo
echo "no FAIL markers; screenshots in $SHOTS"
