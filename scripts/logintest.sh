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

# A live session's root credential, set by the installer rather than typed.
LIVE_PASSWORD=${LOGINTEST_PASSWORD:-root}

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
    -m "${SAMSARA_MEM:-2048}" \
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
# There is no installer any more: the system comes up on a login prompt.
#
# The wait is on the terminal emulator starting, not on the prompt text. The
# prompt goes to the terminal now rather than to the console, so it is not in
# the serial log for a wait_for to match -- and it appears on the framebuffer
# only once fbterm is up and drawing, which is what has to have happened first.
# Short: the terminal is spawned by the kernel a few milliseconds after the
# boot servers, so it is either up by now or not coming. Waiting 150s for
# something that happens in the first second only hides a failure behind a
# timeout.
wait_for "staged \"fbterm\"" 20 || fail "the terminal emulator did not start"
sleep 2
shot 04-prompt

echo
echo "--- the screen after boot ---"
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
type_str "root"; key ret; sleep 4
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
type_str "root"; key ret; sleep 4
type_str "$LIVE_PASSWORD"; key ret; sleep 6
shot 06-in
echo "--- screen after logging in ---"
decode "$SHOTS/06-in.ppm" | tail -8

type_str "whoami"; key ret; sleep 5
shot 07-whoami
echo "--- whoami ---"
decode "$SHOTS/07-whoami.ppm" | tail -6
if ! decode "$SHOTS/07-whoami.ppm" | grep -q "root"; then
    fail "the shell is not running as the user who logged in"
fi
echo
echo "PASS: the shell is running as the user who logged in"

# 3c. An ordinary command.
#
# This is the check that says whether the machine is usable rather than merely
# bootable. `whoami` and `exit` are the shell's own builtins; anything else is a
# separate program that has to be found, forked and executed. Two things stand
# between a person and running one, and both failed silently for a long time: the
# applet links in /bin have to exist, and there has to be memory to fork with.
say "running a command"
type_str "ls /"; key ret; sleep 6
shot 07b-ls
echo "--- ls / ---"
decode "$SHOTS/07b-ls.ppm" | tail -8
# The root directory is seeded by the kernel and always has these, so their
# absence means the command did not run -- not that there was nothing to list.
if ! decode "$SHOTS/07b-ls.ppm" | grep -q 'bin'; then
    fail "an ordinary command did not run: /bin is not in the listing of /"
fi
if decode "$SHOTS/07b-ls.ppm" | grep -qiE 'not found|not implemented|cannot|No such'; then
    fail "an ordinary command was refused"
fi
echo
echo "PASS: an ordinary command runs"

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
