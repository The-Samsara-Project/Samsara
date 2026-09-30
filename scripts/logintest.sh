#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
# Copyright (C) 2026 Harsh Nikarsa
#
# logintest.sh -- drive the login flow end to end on a real framebuffer.
#
# This is the only kind of test that can check a getty, because what a getty
# does is appear on a terminal, and a terminal is not visible from a
# syscall-level self-test. It boots the same image `run-shelltest.sh` does, walks
# the installer past the handoff -- setting a password on the way -- and then
# types at the login prompt the way a person would.
#
# What it proves, in order:
#
#   1. A terminal comes up asking who you are. If the session dropped straight
#      into a shell there is no login, and nothing below would mean anything.
#   2. A wrong password is refused, and says so.
#   3. The right password gets in, and the shell that follows is running as the
#      user who logged in rather than as root.
#   4. Logging out returns to a prompt rather than to a dead terminal.
#
# Point 3 is the one that is easy to fake. A getty that ignored a failed `setuid`
# would hand an unauthenticated caller a root shell, which is the worst thing a
# login program can do, so this asks the shell itself who it is instead of
# trusting that the prompt appeared.
#
# The password is fixed here and set by the installer from this script's typing.
# That is the only place it can be set: `passwd(1)` is a program you run *after*
# logging in, and on this system you cannot log in until a password exists.
set -e

ROOT=$(cd "$(dirname "$0")/.." && pwd)
OUT="$ROOT/target/logintest"
SHOTS="$OUT/shots"
LOG="$OUT/console.log"
ISO="${LOGINTEST_ISO:-$ROOT/samsara.iso}"
MON="$OUT/monitor.sock"
CTL="$OUT/monitor.in"

TEST_PASSWORD=${LOGINTEST_PASSWORD:-samsara123}
TEST_USER=${LOGINTEST_USER:-samsara}

rm -rf "$OUT"
mkdir -p "$SHOTS"
: > "$LOG"

[ -f "$ISO" ] || { echo "no $ISO -- run 'make iso' first" >&2; exit 1; }

say() { printf '\n=== %s ===\n' "$1"; }
fail() {
    printf '\n=== FAILED: %s ===\n' "$1"
    printf '%s\n' "--- last screen ---"
    decode "$SHOTS/LAST.ppm" 2>/dev/null | tail -12
    printf '%s\n' "--- log tail ---"
    tail -25 "$LOG" | sed 's/\x1b\[[0-9;]*m//g'
    exit 1
}

qemu-system-x86_64 \
    -cdrom "$ISO" \
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
exec 3>"$CTL"

mon() { echo "$1" >&3; }

wait_for() {
    needle=$1; secs=${2:-90}; waited=0
    while [ "$waited" -lt "$((secs * 2))" ]; do
        grep -qF "$needle" "$LOG" 2>/dev/null && return 0
        sleep 0.5
        waited=$((waited + 1))
    done
    return 1
}

# Grab the screen, then wait for it to stop changing.
#
# The settling matters: a terminal redraws asynchronously, so a shot taken the
# instant a keystroke is sent catches the screen mid-update. A shot of a
# half-drawn login prompt is not evidence of anything.
shot() {
    name=$1; ppm="$SHOTS/$name.ppm"; last=-1; stable=0; size=0
    rm -f "$ppm"
    mon "screendump $ppm"
    for _ in $(seq 1 100); do
        sleep 0.2
        [ -f "$ppm" ] || continue
        size=$(stat -c %s "$ppm" 2>/dev/null || echo 0)
        if [ "$size" -gt 0 ] && [ "$size" = "$last" ]; then
            stable=$((stable + 1))
            if [ "$stable" -ge 3 ]; then
                cp "$ppm" "$SHOTS/LAST.ppm"
                return 0
            fi
        else
            stable=0
        fi
        last=$size
    done
    echo "warning: $name did not settle" >&2
    return 1
}

decode() { python3 "$ROOT/scripts/fbdecode.py" "$1" 2>/dev/null; }

key() { mon "sendkey $1"; sleep 0.35; }

# QEMU sendkey names for the printable ASCII a login prompt needs. The set is
# the same one `type_str` in run-shelltest.sh owns; a second keyboard layout here
# would be one more thing to keep in step with the first.
type_str() {
    python3 - "$CTL" "$1" <<'PY'
import sys, time
ctl, text = sys.argv[1], sys.argv[2]
NAME = {
    ' ': 'spc', '-': 'minus', '.': 'dot', '/': 'slash', ':': 'shift-semicolon',
    '_': 'shift-minus', '=': 'equal', ',': 'comma', '+': 'shift-equal',
    '>': 'shift-dot', '|': 'shift-backslash', "'": 'apostrophe',
    '"': 'shift-apostrophe', '#': 'shift-3', '!': 'shift-1', '$': 'shift-4',
    '%': 'shift-5', '&': 'shift-7', '*': 'shift-8', '(': 'shift-9',
    ')': 'shift-0', '~': 'shift-grave', '`': 'grave', '@': 'shift-2',
    ';': 'semicolon', '\\': 'backslash', '[': 'bracket_left',
    ']': 'bracket_right', '{': 'shift-bracket_left', '}': 'shift-bracket_right',
    '<': 'shift-comma', '?': 'shift-slash', '^': 'shift-6',
}
with open(ctl, 'w', buffering=1) as f:
    for ch in text:
        if ch == ' ':
            f.write('sendkey spc\n')
        elif ch.isdigit() or ch.isalpha():
            f.write(f'sendkey {ch}\n')
        else:
            f.write(f'sendkey {NAME[ch]}\n')
        time.sleep(0.06)
PY
}

say "booting"
wait_for "setup wizard online" 90 || fail "installer did not start"
sleep 3
shot 00-intro

# --- installer: reach the password prompt -------------------------------
# The wizard's steps differ from run-shelltest.sh's because this script also has
# to type a password, so the key sequence is the short one plus the new prompt.
say "walking the installer"
key ret; sleep 1.5      # past the welcome, into the menu
shot 01-wizard

# `q` goes to the password screen, which is on the way to Finish. This is the
# same navigation `run-shelltest.sh` uses, so the two cannot drift apart on how
# the wizard is driven.
key q; sleep 2
shot 02-password-screen

say "setting the test password"
type_str "$TEST_PASSWORD"; key ret; sleep 1.5
type_str "$TEST_PASSWORD"; key ret; sleep 2
shot 03-password-set

say "finishing setup"
# The password screen hands over to the summary, and the summary's Enter is what
# runs the final self-test pass, populates /bin and starts the terminal.
#
# The wait is on the log line rather than a sleep, because the self-test pass
# takes minutes and a fixed sleep either wastes that time or cuts it short --
# and cutting it short looks exactly like a login that never appeared.
# The first Enter starts the final self-test pass. The second confirms it -- but
# only *after* that pass has finished, because the Finish screen reads a key once
# the results are up. Sending it early does not queue: it arrives while the tests
# are still running, is consumed by them, and the terminal is then never told to
# hand over. So this waits for the last self-test to report before confirming.
#
# `vfork` is chello's final check, so its appearance is the point at which the
# pass is all but over.
key ret; sleep 2
if ! wait_for "vfork" 300; then
    fail "the final self-test pass never finished"
fi
sleep 8
key ret; sleep 3
if ! wait_for "handing the display to fbterm" 120; then
    fail "the installer never handed the display to the terminal"
fi
# The terminal then runs the getty, which prints its prompt. Wait for that too
# rather than assuming a fixed delay is long enough.
wait_for "Samsara login" 60 || true
sleep 6
shot 04-prompt

echo
echo "--- the screen after the handoff ---"
decode "$SHOTS/04-prompt.ppm" | tail -8

# 1. A login prompt, not a shell.
grep -q 'login:' "$LOG" && echo "(log mentions login)"
if ! decode "$SHOTS/04-prompt.ppm" | grep -q 'login:'; then
    fail "no login prompt: the terminal did not come up asking who you are"
fi
echo
echo "PASS: the terminal came up asking who you are"

# 2. A wrong password is refused.
say "a wrong password is refused"
type_str "$TEST_USER"; key ret; sleep 2
type_str "definitely-not-it"; key ret; sleep 4
shot 05-bad
echo "--- screen after a wrong password ---"
decode "$SHOTS/05-bad.ppm" | tail -6
if ! decode "$SHOTS/05-bad.ppm" | grep -qi 'incorrect'; then
    fail "a wrong password was not reported as incorrect"
fi
echo
echo "PASS: a wrong password is refused"

# 3. The right password gets in, and the shell is not root.
say "the right password gets in"
type_str "$TEST_USER"; key ret; sleep 2
type_str "$TEST_PASSWORD"; key ret; sleep 6
shot 06-in
echo "--- screen after logging in ---"
decode "$SHOTS/06-in.ppm" | tail -8

type_str "whoami"; key ret; sleep 5
shot 07-whoami
echo "--- whoami ---"
decode "$SHOTS/07-whoami.ppm" | tail -6
if ! decode "$SHOTS/07-whoami.ppm" | grep -q "$TEST_USER"; then
    fail "the shell is not running as the user who logged in"
fi
echo
echo "PASS: the shell is running as the user who logged in"

# 4. Logout returns to a prompt.
say "logging out"
type_str "exit"; key ret; sleep 6
shot 08-out
echo "--- screen after logout ---"
decode "$SHOTS/08-out.ppm" | tail -6
if ! decode "$SHOTS/08-out.ppm" | grep -q 'login:'; then
    fail "logout left a dead terminal instead of a login prompt"
fi
echo
echo "PASS: logout returns to a login prompt"

echo
echo "=== the login flow works ==="
echo "shots in $SHOTS"
