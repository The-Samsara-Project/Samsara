#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-2.0-or-later
# Copyright (C) 2026 Harsh Nikarsa
#
# Boot the ISO, get past the installer, and then *use the terminal*: type shell
# commands into fbterm and read the answers back off the framebuffer.
#
# This exists because the self-tests do not cover the thing users actually do.
# Every one of them runs a program that prints to a pipe; none of them types at a
# terminal and waits for a shell to answer. That gap is where a whole class of
# failure hides -- a program that runs perfectly when its output is a pipe and
# dies on the first read from a tty, or a terminal that draws but never gets a
# shell behind it -- and both look like "the terminal is dead" from the outside.
#
# So this drives the keyboard. It is a worse test than run-selftests.sh in every
# respect except the one it exists for: slower, dependent on the installer's key
# bindings, and unable to say *why* anything failed. What it can say is whether a
# person sitting at this machine would get a working prompt.
#
#   ./scripts/run-shelltest.sh [outdir]
#
# Exits non-zero if the shell never answers.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT="${1:-$ROOT/target/shelltest}"
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
exec 3>"$CTL"

mon() { echo "$1" >&3; }

wait_for() {
    local pat=$1 secs=${2:-90} waited=0
    while [ "$waited" -lt "$((secs * 2))" ]; do
        grep -qE "$pat" "$LOG" 2>/dev/null && return 0
        sleep 0.5
        waited=$((waited + 1))
    done
    return 1
}

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

key() { mon "sendkey $1"; sleep 0.35; }

# Decode a shot by matching cells against the font's own glyph table, rather than
# by recognising shapes the way fbshot.py does. A broken font makes OCR report
# garbage, and garbage cannot be told apart from a broken framebuffer, a wrong
# palette or a corrupted pixmap -- which is exactly the ambiguity that made a font
# bug look like a rendering bug. This asks a question with a decidable answer:
# do these pixels match the glyphs that were supposed to be drawn here?
decode() { python3 "$ROOT/scripts/fbdecode.py" "$1" "$ROOT/build/fbterm-src" 2>/dev/null; }

# Type a literal string one QEMU key name at a time. QEMU's monitor has no
# "paste", and inventing a per-character mapping here would be a second keyboard
# layout to keep in step with the first; `type` in scripts/fbshot.py already owns
# that mapping and the QEMU names are the same set.
type_str() {
    python3 - "$CTL" "$1" <<'PY'
import sys, time
ctl, text = sys.argv[1], sys.argv[2]
# QEMU sendkey names for the printable ASCII a shell prompt needs.
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
        if ch == '\n':
            f.write('sendkey ret\n')
        elif ch == ' ':
            f.write('sendkey spc\n')
        elif ch.isdigit():
            f.write(f'sendkey {ch}\n')
        elif ch.isalpha():
            f.write(f'sendkey {ch}\n')
        else:
            f.write(f'sendkey {NAME[ch]}\n')
        time.sleep(0.06)
PY
}

echo "waiting for the installer..."
wait_for "setup wizard online" 90 || { echo "installer did not start" >&2; tail -20 "$LOG"; exit 1; }
sleep 3
shot 00-intro

# Walk the installer to the handoff.
#
# This deliberately follows the same key sequence `run-selftests.sh` uses, even
# though the self-test results are not wanted here. The installer's navigation is
# the part of this system with no reverse-engineerable contract -- `q` from the
# main menu does not quit, it selects an item -- and the one sequence known to
# reach the handoff is the one that has been driving that script. Re-deriving it
# here would be a second guess at a menu, and a wrong guess does not fail
# loudly: it lands on some other screen, and the run then reports a terminal that
# never came up.
#
#   ret   dismiss the Introduction screen ("press any key to continue")
#   6     Diagnostics, by digit shortcut
#   ret   run all self-tests -- row 0, selected on entry
#   esc   back out to the main menu
#   q     to the Finish screen
#   ret   confirm the handoff, which is what populates /bin
#
# The last Enter is a confirmation rather than a screen that acts on arrival.
# Stopping one short of it leaves a system that boots, passes its tests, and has
# an empty /bin.
key ret
sleep 1.5
shot 01-main
key 6
sleep 1.5
key ret
sleep 1
echo "running the installer's self-tests on the way past..."
shot 02-diag
done=0
for _ in $(seq 1 120); do
    sleep 1
    shot probe >/dev/null 2>&1 || true
    if python3 "$ROOT/scripts/fbshot.py" "$SHOTS/probe.ppm" 2>/dev/null | grep -qE 'PASS|FAIL'; then
        sleep 1
        shot probe2 >/dev/null 2>&1 || true
        if python3 "$ROOT/scripts/fbshot.py" "$SHOTS/probe2.ppm" 2>/dev/null | grep -qE 'PASS|FAIL'; then
            done=1
            break
        fi
    fi
done
rm -f "$SHOTS/probe.ppm" "$SHOTS/probe2.ppm"
echo "self-tests finished on screen: $done"
key esc
sleep 0.5
key q
sleep 1.5
shot 03-finish
key ret
sleep 3
shot 04-after-handoff

echo "waiting for fbterm to hand over..."
wait_for "handing the display to fbterm" 120 || { echo "fbterm never started" >&2; tail -20 "$LOG"; exit 1; }
sleep 6
shot 04-prompt

# The shell is exec'd by fbterm's forked child. If the thread block is wrong the
# child dies on its first thread-local access and nothing is ever typed, so this
# is the wait that actually decides whether the terminal works.
wait_for "execve: /bin/sh" 60 || { echo "no /bin/sh exec -- the shell never started" >&2; exit 1; }
sleep 5
shot 05-shell-up

# What the shell looks like before anything is typed. The prompt is what a user
# judges "is this thing alive" by, and every symptom below is measured against it:
# a missing prompt, a prompt on the wrong line, and a prompt that needs a second
# Enter all look like the same dead terminal from the outside.
echo "=== the prompt, before typing anything ==="
decode "$SHOTS/05-shell-up.ppm" | tail -6

# Type one command, then press Enter exactly once, and look. A shell that needs a
# second Enter to show a new prompt is a real bug with a specific cause, and it is
# invisible to a test that sends several newlines in a row -- the extra one hides
# it. So this sends one and checks what came back.
echo
echo "=== typing 'ls /bin' and pressing Enter once ==="
type_str 'ls /bin'
sleep 2
shot 06a-typed
decode "$SHOTS/06a-typed.ppm" | tail -4
key ret
sleep 4
shot 06b-after-one-enter
echo "--- after one Enter ---"
decode "$SHOTS/06b-after-one-enter.ppm" | tail -6

echo
echo "=== typing 'echo hello from the shell' and pressing Enter once ==="
type_str 'echo hello from the shell'
sleep 2
key ret
sleep 4
shot 07-echo
echo "--- after one Enter ---"
decode "$SHOTS/07-echo.ppm" | tail -6

# Scrolling. A command whose output is taller than the screen must scroll, and
# the prompt has to end up on the *last* line. If the screen does not scroll, the
# prompt ends up somewhere above the bottom of the display and the terminal looks
# frozen -- which is indistinguishable, from the outside, from one that is merely
# waiting.
echo
echo "=== a command with more output than the screen has rows ==="
type_str 'ls /bin /bin /bin /bin /bin /bin /bin /bin'
sleep 8
shot 08-scroll
echo "--- every row the terminal still has, top to bottom ---"
decode "$SHOTS/08-scroll.ppm"

echo
echo "=== the session, read back out of the framebuffer ==="
decode "$SHOTS/07-echo.ppm" | tail -8

# Did the shell actually answer? Decoding the screen is the only way to know, and
# "the terminal drew something" is not the same as "the terminal is readable" --
# which is the distinction this whole script exists to make.
if decode "$SHOTS/07-echo.ppm" | grep -q 'hello from the shell'; then
    echo
    echo "the shell ran the command and its output is legible on screen"
else
    echo
    echo "=== the shell's output is not legible on screen ==="
    exit 1
fi

echo
echo "--- kernel-side faults ---"
grep -aE "killing pid" "$LOG" | sed 's/\x1b\[[0-9;]*m//g' || echo "(none)"
echo
echo "shots in $SHOTS"
