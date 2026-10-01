//! Shared cockpit logic, independent of the simulator:
//!
//! - [`Register`]: shared values, ordered by the host, last change wins;
//! - [`Commands`]: forwarding cockpit commands without echoes;
//! - [`Repair`]: drift repair of shared values after quiet periods.
//!
//! Keys are indices into the sync definition both seats share.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use flyx_protocol::{CommandPhase, Control, Seat, Value};

/// A seat sends at most one message per key per this many seconds; the
/// latest value is always sent at the end of a burst.
pub const MIN_INTERVAL_S: f64 = 0.05;
/// Changes seen within this many frames after replaying a command from the
/// other seat are attributed to that seat.
pub const ECHO_FRAMES: u64 = 2;
/// A key that changed in this many distinct seconds out of the last
/// [`NOISY_WINDOW_S`] changes on its own (no person operates one control
/// that long): the simulator drives it, for example an autopilot. It is
/// muted: this seat stops sending its changes. On the pilot flying, a muted
/// key's value goes to the pilot monitoring with the systems state instead.
/// Mutes are cleared whenever the controls change hands.
pub const NOISY_SECONDS: usize = 20;
pub const NOISY_WINDOW_S: f64 = 30.0;

/// Whether two values differ by more than `epsilon`.
pub fn differs(a: Value, b: Value, epsilon: f32) -> bool {
    match (a, b) {
        (Value::Int(a), Value::Int(b)) => a != b,
        _ => (as_f64(a) - as_f64(b)).abs() > epsilon as f64,
    }
}

fn as_f64(v: Value) -> f64 {
    match v {
        Value::Int(v) => v as f64,
        Value::Float(v) => v as f64,
        Value::Double(v) => v,
    }
}

#[derive(Debug)]
struct Slot {
    epsilon: f32,
    /// The value this seat last saw, wrote, or sent.
    last_known: Option<Value>,
    /// A local change not yet sent (rate limit).
    unsent: Option<Value>,
    last_sent_at: f64,
    /// Crew: the newest `req` sent for this key and not yet acknowledged.
    pending: Option<u32>,
    /// Whole seconds in which this seat saw the key change locally.
    change_seconds: VecDeque<i64>,
    muted: bool,
}

/// What the register asks the plugin to do.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    /// Write this value into the simulator.
    Write { key: u16, value: Value },
    /// Send this message to the other seat.
    Send(Control),
    /// The key was muted because it changes on its own.
    Muted { key: u16 },
}

/// The shared values of one seat.
#[derive(Debug)]
pub struct Register {
    seat: Seat,
    slots: BTreeMap<u16, Slot>,
    next_req: u32,
    /// Changes are absorbed up to and including this frame.
    echo_until: Option<u64>,
}

impl Register {
    /// A register for `keys` (key, epsilon) on `seat`.
    pub fn new(seat: Seat, keys: impl IntoIterator<Item = (u16, f32)>) -> Self {
        let slots = keys
            .into_iter()
            .map(|(key, epsilon)| {
                (
                    key,
                    Slot {
                        epsilon,
                        last_known: None,
                        unsent: None,
                        last_sent_at: f64::NEG_INFINITY,
                        pending: None,
                        change_seconds: VecDeque::new(),
                        muted: false,
                    },
                )
            })
            .collect();
        Self {
            seat,
            slots,
            next_req: 1,
            echo_until: None,
        }
    }

    pub fn keys(&self) -> impl Iterator<Item = u16> + '_ {
        self.slots.iter().filter(|(_, s)| !s.muted).map(|(k, _)| *k)
    }

    pub fn is_muted(&self, key: u16) -> bool {
        self.slots.get(&key).is_some_and(|s| s.muted)
    }

    /// The keys this seat stopped sending because they change on their own.
    pub fn muted_keys(&self) -> Vec<u16> {
        self.slots
            .iter()
            .filter(|(_, s)| s.muted)
            .map(|(k, _)| *k)
            .collect()
    }

    /// Pilot monitoring: the pilot flying sends `key` with its systems
    /// state, so this seat follows it and stops sending its own changes.
    pub fn mute(&mut self, key: u16) {
        if let Some(slot) = self.slots.get_mut(&key) {
            slot.muted = true;
            slot.unsent = None;
        }
    }

    /// The controls changed hands: values the old pilot flying's simulator
    /// drove may now be driven by the other seat's, so every key starts
    /// unmuted again.
    pub fn unmute_all(&mut self) {
        for slot in self.slots.values_mut() {
            slot.muted = false;
            slot.change_seconds.clear();
        }
    }

    /// A command from the other seat was replayed in `frame`: changes seen
    /// until [`ECHO_FRAMES`] frames later are caused by it.
    pub fn note_replay(&mut self, frame: u64) {
        self.echo_until = Some(frame + ECHO_FRAMES);
    }

    /// The plugin wrote `value` itself; it is not a local change.
    pub fn wrote(&mut self, key: u16, value: Value) {
        if let Some(slot) = self.slots.get_mut(&key) {
            slot.last_known = Some(value);
        }
    }

    /// The simulator's current value of `key`, read in `frame` at time
    /// `now` (seconds). The first reading of a key only seeds it.
    pub fn observe(&mut self, key: u16, value: Value, frame: u64, now: f64) -> Vec<Action> {
        let echo = self.echo_until.is_some_and(|until| frame <= until);
        let Some(slot) = self.slots.get_mut(&key) else {
            return vec![];
        };
        if slot.muted {
            return vec![];
        }
        let Some(last) = slot.last_known else {
            slot.last_known = Some(value);
            return vec![];
        };
        if !differs(last, value, slot.epsilon) {
            return vec![];
        }
        slot.last_known = Some(value);
        if echo {
            return vec![];
        }
        // A local change.
        let second = now.floor() as i64;
        if slot.change_seconds.back() != Some(&second) {
            slot.change_seconds.push_back(second);
        }
        while slot
            .change_seconds
            .front()
            .is_some_and(|&s| (now - s as f64) > NOISY_WINDOW_S)
        {
            slot.change_seconds.pop_front();
        }
        if slot.change_seconds.len() >= NOISY_SECONDS {
            slot.muted = true;
            slot.unsent = None;
            return vec![Action::Muted { key }];
        }
        slot.unsent = Some(value);
        self.flush_key(key, now)
    }

    /// Sends local changes whose rate limit has passed. Call every frame.
    pub fn flush(&mut self, now: f64) -> Vec<Action> {
        let due: Vec<u16> = self
            .slots
            .iter()
            .filter(|(_, s)| s.unsent.is_some())
            .map(|(k, _)| *k)
            .collect();
        due.into_iter()
            .flat_map(|key| self.flush_key(key, now))
            .collect()
    }

    fn flush_key(&mut self, key: u16, now: f64) -> Vec<Action> {
        let seat = self.seat;
        let Some(slot) = self.slots.get_mut(&key) else {
            return vec![];
        };
        let Some(value) = slot.unsent else {
            return vec![];
        };
        if now - slot.last_sent_at < MIN_INTERVAL_S {
            return vec![];
        }
        slot.unsent = None;
        slot.last_sent_at = now;
        let message = match seat {
            Seat::Host => Control::Set {
                key,
                value,
                ack: None,
            },
            Seat::Crew => {
                let req = self.next_req;
                self.next_req += 1;
                slot.pending = Some(req);
                Control::Change { key, value, req }
            }
        };
        vec![Action::Send(message)]
    }

    /// Host: the crew changed `key`. The host applies it and broadcasts it.
    pub fn receive_change(&mut self, key: u16, value: Value, req: u32) -> Vec<Action> {
        let Some(slot) = self.slots.get_mut(&key) else {
            return vec![];
        };
        slot.last_known = Some(value);
        // The crew's change wins over a local change not yet sent.
        slot.unsent = None;
        vec![
            Action::Write { key, value },
            Action::Send(Control::Set {
                key,
                value,
                ack: Some(req),
            }),
        ]
    }

    /// Crew: the host's value of `key`, in the host's order.
    pub fn receive_set(&mut self, key: u16, value: Value, ack: Option<u32>) -> Vec<Action> {
        let Some(slot) = self.slots.get_mut(&key) else {
            return vec![];
        };
        if slot.unsent.is_some() {
            // A newer local change is about to be sent.
            return vec![];
        }
        if let Some(pending) = slot.pending {
            match ack {
                Some(ack) if ack >= pending => slot.pending = None,
                // Older than the crew's latest change still in flight.
                _ => return vec![],
            }
        }
        slot.last_known = Some(value);
        vec![Action::Write { key, value }]
    }

    /// A value for drift repair: written here and sent on as a change
    /// from this seat, so the host's order still holds.
    pub fn repair(&mut self, key: u16, value: Value, now: f64) -> Vec<Action> {
        let Some(slot) = self.slots.get_mut(&key) else {
            return vec![];
        };
        if slot.muted || slot.unsent.is_some() || slot.pending.is_some() {
            return vec![];
        }
        slot.last_known = Some(value);
        slot.unsent = Some(value);
        let mut actions = vec![Action::Write { key, value }];
        actions.extend(self.flush_key(key, now));
        actions
    }

    /// Every value this seat knows, for a join snapshot and drift repair.
    /// Muted keys are left out: their last known value is stale.
    pub fn snapshot(&self) -> Vec<(u16, Value)> {
        self.slots
            .iter()
            .filter(|(_, s)| !s.muted)
            .filter_map(|(k, s)| s.last_known.map(|v| (*k, v)))
            .collect()
    }

    /// Crew: applies the host's snapshot; the writes are not echoed.
    pub fn apply_snapshot(&mut self, values: &[(u16, Value)]) -> Vec<Action> {
        values
            .iter()
            .filter_map(|&(key, value)| {
                let slot = self.slots.get_mut(&key)?;
                slot.last_known = Some(value);
                slot.unsent = None;
                Some(Action::Write { key, value })
            })
            .collect()
    }
}

/// Forwarding of cockpit commands between the seats.
#[derive(Debug, Default)]
pub struct Commands {
    replaying: bool,
    /// Commands the other seat holds that this seat is replaying.
    held: BTreeSet<u16>,
}

impl Commands {
    /// A local handler saw `phase` of command `key`. Returns the message to
    /// send, if any. Phases seen while replaying are not sent back.
    pub fn local(&mut self, key: u16, phase: HandlerPhase) -> Option<Control> {
        if self.replaying {
            return None;
        }
        let phase = match phase {
            HandlerPhase::Begin => CommandPhase::Begin,
            HandlerPhase::End => CommandPhase::End,
            HandlerPhase::Continue => return None,
        };
        Some(Control::Command { key, phase })
    }

    /// Marks the start and end of replaying a command from the other seat.
    pub fn set_replaying(&mut self, replaying: bool) {
        self.replaying = replaying;
    }

    /// A command phase arrived from the other seat. Returns whether it
    /// should be replayed (a duplicate Begin or a stray End is not).
    pub fn remote(&mut self, key: u16, phase: CommandPhase) -> bool {
        match phase {
            CommandPhase::Begin => self.held.insert(key),
            CommandPhase::End => self.held.remove(&key),
        }
    }

    /// The commands to end because the session ended while the other seat
    /// held them.
    pub fn end_all(&mut self) -> Vec<u16> {
        std::mem::take(&mut self.held).into_iter().collect()
    }
}

/// Command phases as a command handler sees them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HandlerPhase {
    Begin,
    Continue,
    End,
}

/// Seconds without any cockpit activity before drift repair applies.
pub const QUIET_S: f64 = 3.0;
/// Budget for the repair slice of one systems datagram.
pub const REPAIR_BYTES: usize = 900;

/// Encoded size of one (key, value) pair, upper bound.
fn entry_size(value: Value) -> usize {
    // Key varint (up to 3) + value tag (1) + value.
    4 + match value {
        Value::Int(_) => 5,
        Value::Float(_) => 4,
        Value::Double(_) => 8,
    }
}

/// Drift repair bookkeeping.
#[derive(Debug, Default)]
pub struct Repair {
    last_activity: f64,
    cursor: usize,
    reported_unwritable: BTreeSet<u16>,
}

impl Repair {
    /// Cockpit activity on either seat (a command or a shared change).
    pub fn activity(&mut self, now: f64) {
        self.last_activity = now;
    }

    pub fn is_quiet(&self, now: f64) -> bool {
        now - self.last_activity >= QUIET_S
    }

    /// Pilot flying: the next round-robin slice of `values` that fits in
    /// one systems datagram.
    pub fn next_chunk(&mut self, values: &[(u16, Value)]) -> Vec<(u16, Value)> {
        if values.is_empty() {
            return vec![];
        }
        let mut out = Vec::new();
        let mut bytes = 0;
        let start = self.cursor % values.len();
        for i in 0..values.len() {
            let entry = values[(start + i) % values.len()];
            let size = entry_size(entry.1);
            if bytes + size > REPAIR_BYTES {
                break;
            }
            bytes += size;
            out.push(entry);
        }
        self.cursor = (start + out.len()) % values.len();
        out
    }

    /// Pilot monitoring: which received values to repair now. `local`
    /// gives this seat's value and epsilon, or `None` if the key is not
    /// writable here.
    pub fn select(
        &mut self,
        now: f64,
        chunk: &[(u16, Value)],
        local: impl Fn(u16) -> Option<(Value, f32)>,
    ) -> RepairSelection {
        let mut selection = RepairSelection::default();
        if !self.is_quiet(now) {
            return selection;
        }
        for &(key, value) in chunk {
            match local(key) {
                Some((current, epsilon)) => {
                    if differs(current, value, epsilon) {
                        selection.repair.push((key, value));
                    }
                }
                None => {
                    if self.reported_unwritable.insert(key) {
                        selection.unwritable.push(key);
                    }
                }
            }
        }
        selection
    }
}

/// Result of [`Repair::select`].
#[derive(Debug, Default, PartialEq)]
pub struct RepairSelection {
    /// Values to repair through [`Register::repair`].
    pub repair: Vec<(u16, Value)>,
    /// Unwritable keys to log (each only once per session).
    pub unwritable: Vec<u16>,
}

#[cfg(test)]
mod tests;
