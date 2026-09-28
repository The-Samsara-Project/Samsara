// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Harsh Nikarsa
//
// IPC wire format mirror of the kernel's `abi::MsgFrame`. The kernel copies
// this structure verbatim across address spaces on `IPC_SEND`/`IPC_RECV`.

use crate::syscall::{self, IPC_CALL, IPC_RECV, IPC_REPLY, IPC_SEND};

/// Greatest payload size carried in one frame (kernel `ipc::MAX_MSG_DATA`).
pub const MAX_MSG_DATA: usize = 2048;

/// `MsgKind` wire values (kernel `ipc::MsgKind`).
pub const KIND_CALL: u32 = 0;
pub const KIND_REPLY: u32 = 1;
pub const KIND_NOTIFY: u32 = 2;
pub const KIND_IRQ: u32 = 3;

/// Input-event opcodes carried in `MsgFrame::tag` by the input daemon.
/// `args[0]` stays zero; the remaining words are laid out per event below.
pub mod inp {
    /// Keyboard state change: `args[1] = make code`, `args[2] = extended`,
    /// `args[3] = down`, `args[4] = modifier mask`.
    pub const KEY: u32 = 1;
    /// Mouse packet: `args[1] = buttons` (bit0 L, bit1 R, bit2 M),
    /// `args[2] = dx`, `args[3] = dy` (both sign-extended).
    pub const MOUSE: u32 = 2;
}

/// Modifier mask bits accompanying an [`inp::KEY`] event.
pub const MOD_SHIFT: u64 = 1 << 0;
pub const MOD_CTRL: u64 = 1 << 1;
pub const MOD_ALT: u64 = 1 << 2;
pub const MOD_CAPS: u64 = 1 << 3;

/// Message classification decoded from a received frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Call,
    Reply,
    Notify,
    Irq,
}

impl Kind {
    fn from_wire(v: u32) -> Kind {
        match v {
            KIND_CALL => Kind::Call,
            KIND_REPLY => Kind::Reply,
            KIND_IRQ => Kind::Irq,
            _ => Kind::Notify,
        }
    }
}

/// User-visible IPC frame; layout matches `abi::MsgFrame` exactly.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct MsgFrame {
    /// Application tag; for `KIND_IRQ` frames the kernel stores the IRQ number.
    pub tag: u32,
    /// Source endpoint on received frames (0 = kernel).
    pub from: u32,
    /// Call/response correlation id (kernel-issued for `Call`s).
    pub call_id: u64,
    /// Twelve protocol argument words.
    pub args: [u64; 12],
    /// Number of valid bytes in `data`.
    pub data_len: u32,
    /// Kind wire value on received frames.
    pub kind: u32,
    /// Payload bytes.
    pub data: [u8; MAX_MSG_DATA],
}

impl MsgFrame {
    /// Build an empty frame.
    pub const fn new() -> Self {
        MsgFrame {
            tag: 0,
            from: 0,
            call_id: 0,
            args: [0; 12],
            data_len: 0,
            kind: 0,
            data: [0; MAX_MSG_DATA],
        }
    }

    /// Copy `bytes` into the payload slot and set `data_len`.
    pub fn set_payload(&mut self, bytes: &[u8]) {
        let n = bytes.len().min(MAX_MSG_DATA);
        self.data[..n].copy_from_slice(&bytes[..n]);
        self.data_len = n as u32;
    }

    /// Trailing payload as a slice.
    pub fn payload(&self) -> &[u8] {
        &self.data[..self.data_len.min(MAX_MSG_DATA as u32) as usize]
    }

    /// Decoded message kind (meaningful after `recv`).
    pub fn msg_kind(&self) -> Kind {
        Kind::from_wire(self.kind)
    }
}

/// Fire a `Notify` (no reply expected) at endpoint `dst`.
pub fn send(dst: u64, f: &MsgFrame) -> Result<(), i64> {
    // SAFETY: `f` lives in our mapped stack/text space; kernel reads it.
    let r = unsafe { syscall::raw(IPC_SEND, dst, f as *const MsgFrame as u64, 0, 0, 0, 0) };
    if r < 0 {
        Err(r)
    } else {
        Ok(())
    }
}

/// Answer a `Call` from `dst` (its `call_id`) with `f`.
pub fn reply(dst: u64, call_id: u64, f: &MsgFrame) -> Result<(), i64> {
    // SAFETY: kernel validates the pointer and endpoint.
    let r = unsafe {
        syscall::raw(
            IPC_REPLY,
            dst,
            call_id,
            f as *const MsgFrame as u64,
            0,
            0,
            0,
        )
    };
    if r < 0 {
        Err(r)
    } else {
        Ok(())
    }
}

/// Block until a message lands at this endpoint and fill `f` with it.
pub fn recv(f: &mut MsgFrame) -> Result<(), i64> {
    // SAFETY: kernel writes at most one MsgFrame into `f`.
    let r = unsafe { syscall::raw(IPC_RECV, f as *mut MsgFrame as u64, 0, 0, 0, 0, 0) };
    if r < 0 {
        Err(r)
    } else {
        Ok(())
    }
}

/// Make a synchronous `Call` to `dst`: send `req`, wait for the matching
/// `Reply`, and fill `rep` with it.
pub fn call(dst: u64, req: &MsgFrame, rep: &mut MsgFrame) -> Result<(), i64> {
    // SAFETY: both pointers reference mapped memory; kernel manages matching.
    let r = unsafe {
        syscall::raw(
            IPC_CALL,
            dst,
            req as *const MsgFrame as u64,
            rep as *mut MsgFrame as u64,
            0,
            0,
            0,
        )
    };
    if r < 0 {
        Err(r)
    } else {
        Ok(())
    }
}