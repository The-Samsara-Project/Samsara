#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-2.0-or-later
# Copyright (C) 2026 Harsh Nikarsa

"""One persistent QEMU monitor session, driven from a shell over a fifo.

    scripts/monitor.py <socket> <control-fifo> <logfile>

Connects once and stays connected. The shell writes monitor commands to the
control fifo; this process relays them to QEMU.

A connection per command does not work, and it is worth saying why rather than
leaving the next person to rediscover it. QEMU's monitor takes the command
*asynchronously* -- it reads a line, queues it, and writes the result whenever it
gets round to it -- and it does not echo commands. So a `socat` that connects,
writes `screendump x.ppm` and exits can have the command queued and then the
socket closed out from under the reply. The screendump usually still lands,
because QEMU had already parsed the line, but the caller has no way to know
whether it did: the connection gave it nothing to wait on. Observed as
"screendump produced no file" roughly as often as it worked.

Holding the connection open and waiting for the *effect* -- the file appearing and
stopping growing -- is the only version of this that can be made reliable.
"""

import os
import queue
import socket
import sys
import threading
import time

sock_path, fifo_path, log_path = sys.argv[1], sys.argv[2], sys.argv[3]


def log(msg):
    with open(log_path, "a", encoding="utf-8") as f:
        f.write(f"[monitor] {msg}\n")


# QEMU's monitor creates the socket before it is willing to accept, and the guest
# takes a while to boot; retrying is the difference between a script that waits
# for the monitor and one that races it.
s = None
deadline = time.time() + 60
while time.time() < deadline:
    try:
        s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        s.connect(sock_path)
        break
    except OSError:
        s = None
        time.sleep(0.2)
if s is None:
    log("could not connect to the monitor socket")
    sys.exit(1)
s.settimeout(0.5)
log("connected")


def drain():
    """Read whatever the monitor has to say and throw it away.

    The reply is not used. Waiting for one is unreliable -- a screendump produces
    no output at all, a sendkey echoes nothing, and `info` produces output that
    does not correlate with any particular command -- so the monitor's output is
    discarded and the caller synchronises on the observable effect instead.
    """
    while True:
        try:
            if not s.recv(65536):
                return False
        except socket.timeout:
            return True
        except OSError:
            return False


stop = threading.Event()


def reader():
    while not stop.is_set():
        if not drain():
            break


threading.Thread(target=reader, daemon=True).start()

# Commands are read from the fifo in blocking mode on a dedicated thread, because
# a blocking read of a fifo cannot share a thread with the monitor's own reads.
cmds = queue.Queue()


def feeder():
    with open(fifo_path, "r", encoding="utf-8") as f:
        for line in f:
            line = line.strip()
            if line:
                cmds.put(line)


threading.Thread(target=feeder, daemon=True).start()

while True:
    try:
        cmd = cmds.get(timeout=1)
    except queue.Empty:
        if not drain():
            break
        continue
    log(f"-> {cmd}")
    try:
        s.sendall((cmd + "\n").encode())
    except OSError as e:
        log(f"send failed: {e}")
        break
    drain()
stop.set()
log("exiting")
