// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Harsh Nikarsa

//! Inter-process communication — the heart of the microkernel.
//!
//! Samsara's kernel core exposes exactly four capabilities to processes:
//! threads, address spaces, scheduling and **IPC**. Every server in the
//! system (filesystem, device drivers, console) is a process that answers
//! requests on an *endpoint*; clients obtain services purely by sending and
//! receiving messages. The kernel never interprets payload contents — it
//! only delivers messages between endpoints and wakes blocked threads.
//!
//! ## Model
//!
//! * Every process owns one endpoint (`EndpointId`). Endpoints are numbered
//!   by process id, and a small range of ids is reserved for well-known
//!   bootstrap servers so clients can find them without a registry.
//! * `Call` and `Reply` pair up as synchronous request/response; `Notify` is
//!   a fire-and-forget signal; `Irq` is the kernel delivering a hardware
//!   interrupt as a message (see [`crate::interrupts`]).
//! * A receiver that finds its inbox empty parks itself in the endpoint's
//!   wait queue; the next sender to the endpoint wakes the first waiter. All
//!   message traffic therefore double-checks the inbox to avoid wakeup
//!   races (single CPU, syscall context keeps interrupts masked).

use alloc::collections::{BTreeMap, VecDeque};
use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};

use crate::sync::Spinlock;
use crate::task::TaskId;

/// Endpoint identifier exposed to processes (also the owning process id).
pub type EndpointId = u64;

/// Reserved well-known endpoint: the root filesystem server (`vfsd`).
pub const EP_VFSD: EndpointId = 1;
/// Reserved well-known endpoint: the console/event logger (`consoled`).
pub const EP_CONSOLED: EndpointId = 2;
/// Reserved well-known endpoint: the PS/2 input driver (`inputd`).
pub const EP_INPUTD: EndpointId = 3;
/// Reserved well-known endpoint: the bootstrap demo client.
pub const EP_DEMO: EndpointId = 4;
/// Endpoint 5 was the window-manager compositor (`wmsrv`). The id stays
/// reserved so the well-known block is never renumbered.
pub const EP_RESERVED_WMSRV: EndpointId = 5;
/// First endpoint id handed to dynamically spawned processes.
pub const EP_DYNAMIC_BASE: EndpointId = 6;

/// Greatest number of data bytes carried by a single message.
pub const MAX_MSG_DATA: usize = 2048;

/// Semantic kind of a message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum MsgKind {
    /// Synchronous request: `Call` + later `Reply` (server answers response).
    Call = 0,
    /// Response to a previous `Call` from the counterpart server.
    Reply = 1,
    /// Asynchronous signal, no answer expected.
    Notify = 2,
    /// Hardware interrupt delivered by the kernel to a bound endpoint.
    Irq = 3,
}

impl MsgKind {
    /// Decode from the numeric wire representation.
    pub fn from_wire(v: u64) -> Option<MsgKind> {
        match v {
            0 => Some(MsgKind::Call),
            1 => Some(MsgKind::Reply),
            2 => Some(MsgKind::Notify),
            3 => Some(MsgKind::Irq),
            _ => None,
        }
    }
}

/// A message in transit between two endpoints.
#[derive(Debug, Clone)]
pub struct Message {
    /// Semantic kind (see [`MsgKind`]).
    pub kind: MsgKind,
    /// Sender's endpoint id (`0` = the kernel itself).
    pub from: EndpointId,
    /// Destination endpoint id.
    pub to: EndpointId,
    /// Call/response correlation id (meaningful for `Call` and `Reply`).
    pub call_id: u64,
    /// Application-defined tag.
    pub tag: u32,
    /// Fixed-size argument words; layout is the protocol's concern.
    pub args: [u64; 12],
    /// Optional payload bytes, copied by the kernel across address spaces.
    pub data: Vec<u8>,
}

/// Errors surfaced by the IPC syscalls (negated ABI errnos).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IpcError {
    /// Destination endpoint does not exist.
    NoEndpoint,
    /// Message data exceeded [`MAX_MSG_DATA`].
    TooBig,
    /// Bad argument / user buffer unreachable.
    Invalid,
}

impl IpcError {
    /// Negated errno value returned over the syscall ABI.
    pub fn to_abi(self) -> i64 {
        match self {
            IpcError::NoEndpoint => crate::abi::errno::ENOENT,
            IpcError::TooBig | IpcError::Invalid => crate::abi::errno::EINVAL,
        }
    }
}

/// One receiver's message box. Messages are queued FIFO; blocked receivers
/// are parked in `waiters` and handed the head of the queue when a sender
/// arrives (the first sender wakes the first waiter).
struct Endpoint {
    name: String,
    inbox: VecDeque<Message>,
    waiters: VecDeque<TaskId>,
}

/// Endpoint table shared by the whole kernel.
static ENDPOINTS: Spinlock<BTreeMap<EndpointId, Endpoint>> = Spinlock::new(BTreeMap::new());

/// Next dynamically allocated endpoint id.
static NEXT_EP: AtomicU64 = AtomicU64::new(EP_DYNAMIC_BASE);

/// Allocate a fresh endpoint id (never reuses a well-known id).
fn alloc_ep_id() -> EndpointId {
    NEXT_EP.fetch_add(1, Ordering::Relaxed)
}

/// Register a new endpoint owned by `owner`. Well-known ids are handed out
/// to the bootstrap servers at boot via [`register_well_known`].
pub fn create_endpoint(owner: TaskId, name: &str) -> EndpointId {
    let id = alloc_ep_id();
    ENDPOINTS.lock().insert(
        id,
        Endpoint {
            name: String::from(name),
            inbox: VecDeque::new(),
            waiters: VecDeque::new(),
        },
    );
    id
}

/// Register an endpoint under a specific (well-known) id. Used only by the
/// kernel to stand up the bootstrap servers with deterministic ids.
pub fn register_well_known(id: EndpointId, name: &str) {
    let mut g = ENDPOINTS.lock();
    if !g.contains_key(&id) {
        g.insert(
            id,
            Endpoint {
                name: String::from(name),
                inbox: VecDeque::new(),
                waiters: VecDeque::new(),
            },
        );
    }
}

/// Check whether an endpoint exists.
pub fn endpoint_alive(id: EndpointId) -> bool {
    ENDPOINTS.lock().contains_key(&id)
}

/// Generate a fresh call-correlation id.
pub fn gen_call_id() -> u64 {
    static NEXT_CALL: AtomicU64 = AtomicU64::new(1);
    NEXT_CALL.fetch_add(1, Ordering::Relaxed)
}

/// Non-blocking receive: pop the head message of `ep`, if any.
pub fn try_recv(ep: EndpointId) -> Option<Message> {
    ENDPOINTS.lock().get_mut(&ep)?.inbox.pop_front()
}

/// Outcome of a blocking receive.
pub enum RecvWait {
    /// A message was retrieved.
    Message(Message),
    /// Woken with an empty inbox and a deliverable signal pending: the
    /// syscall should return `EINTR` (or restart per `SA_RESTART`).
    Interrupted,
    /// The endpoint no longer exists.
    Gone,
}

/// Blocking receive: returns the next message for `ep`. The caller thread is
/// parked in the endpoint's wait queue until a sender delivers a message.
///
/// [`RecvWait::Gone`] only signals a vanished endpoint; [`RecvWait::Interrupted`]
/// is returned when a catchable/fatal signal makes the block `EINTR` — the
/// caller's waiter slot is already removed.
pub fn recv_wait(ep: EndpointId) -> RecvWait {
    loop {
        if let Some(m) = try_recv(ep) {
            return RecvWait::Message(m);
        }
        let mut parked = false;
        {
            let mut g = ENDPOINTS.lock();
            match g.get_mut(&ep) {
                Some(e) if e.inbox.is_empty() => {
                    if let Some(cur) = crate::task::sched::current_task_id() {
                        e.waiters.push_back(cur);
                        parked = true;
                    }
                }
                Some(_) => {}
                None => return RecvWait::Gone,
            }
        }
        if parked {
            // A deliverable signal interrupts the block. Drop our waiter slot
            // so a later send cannot wake a stale queue entry.
            if crate::sig::deliverable_now() {
                let mut g = ENDPOINTS.lock();
                if let Some(cur) = crate::task::sched::current_task_id() {
                    if let Some(e) = g.get_mut(&ep) {
                        e.waiters.retain(|w| *w != cur);
                    }
                }
                return RecvWait::Interrupted;
            }
            // Park the current thread; a sender's wake() rechains it.
            crate::task::sched::block_current();
        }
    }
}

/// Deliver `msg` to endpoint `dst` and wake the first waiting receiver.
pub fn send(dst: EndpointId, mut msg: Message) -> Result<(), IpcError> {
    if msg.data.len() > MAX_MSG_DATA {
        return Err(IpcError::TooBig);
    }
    let to_wake = {
        let mut g = ENDPOINTS.lock();
        let ep = g.get_mut(&dst).ok_or(IpcError::NoEndpoint)?;
        msg.to = dst;
        ep.inbox.push_back(msg);
        ep.waiters.pop_front()
    };
    if let Some(t) = to_wake {
        crate::task::sched::wake(t);
    }
    Ok(())
}

/// Re-queue an already-received but unwanted `msg` at the *front* of its own
/// inbox, preserving ordering for the next receiver. Used by `IPC_CALL` to
/// slide past unrelated notifications while waiting for its reply.
pub fn requeue(msg: Message) {
    let ep = msg.to;
    let mut g = ENDPOINTS.lock();
    if let Some(e) = g.get_mut(&ep) {
        e.inbox.push_front(msg);
    }
}

/// Deliver a hardware-IRQ message to a bound endpoint from interrupt context.
///
/// Uses advisory (`try_`) locks because the IRQ may have interrupted a
/// critical section. If a wake cannot be issued the waiter is put back on
/// the endpoint's wait queue so a later delivery (or the driver's own poll
/// fallback) can still wake it — otherwise the receiver would block forever
/// with the message stranded in the inbox.
pub fn deliver_irq(ep: EndpointId, irq: u8) {
    let to_wake = {
        let mut g = match ENDPOINTS.try_lock() {
            Some(g) => g,
            None => {
                crate::log::kdebug!("irq{}: deliver_irq dropped (endpoint lock contended)", irq);
                return;
            }
        };
        let e = match g.get_mut(&ep) {
            Some(e) => e,
            None => return,
        };
        e.inbox.push_back(Message {
            kind: MsgKind::Irq,
            from: 0,
            to: ep,
            call_id: 0,
            tag: irq as u32,
            args: [0; 12],
            data: Vec::new(),
        });
        e.waiters.pop_front()
    };
    if let Some(t) = to_wake {
        if crate::task::sched::try_wake(t) {
            return;
        }
        // The message is queued but the receiver was not re-scheduled (the
        // scheduler lock was contended, or the task is not yet Blocked).
        // Restore it to the wait queue so delivery retries on the next IRQ;
        // recv_wait re-checks the inbox before parking, so no duplicate
        // consumption can occur.
        crate::log::kdebug!(
            "irq{}: wake of task {} lost; re-queueing waiter on ep {}",
            irq,
            t.0,
            ep
        );
        let mut g = match ENDPOINTS.try_lock() {
            Some(g) => g,
            None => return,
        };
        if let Some(e) = g.get_mut(&ep) {
            e.waiters.push_front(t);
        }
    }
}

/// Kind short name used in diagnostics.
pub fn kind_name(k: MsgKind) -> &'static str {
    match k {
        MsgKind::Call => "call",
        MsgKind::Reply => "reply",
        MsgKind::Notify => "notify",
        MsgKind::Irq => "irq",
    }
}

/// Debug snapshot of live endpoints.
pub fn snapshot() -> Vec<(EndpointId, String, usize, usize)> {
    let g = ENDPOINTS.lock();
    g.iter()
        .map(|(id, e)| (*id, e.name.clone(), e.inbox.len(), e.waiters.len()))
        .collect()
}