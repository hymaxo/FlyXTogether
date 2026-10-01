use std::collections::{BTreeMap, VecDeque};

use super::*;

const FRAME_S: f64 = 1.0 / 60.0;

/// One seat: its register, its simulator values and its outgoing link.
struct SeatSim {
    reg: Register,
    sim: BTreeMap<u16, Value>,
    outbox: VecDeque<Control>,
    /// Values the register wrote into the simulator, in order.
    writes: Vec<(u16, Value)>,
}

impl SeatSim {
    fn new(seat: Seat, keys: &[u16]) -> Self {
        let mut s = Self {
            reg: Register::new(seat, keys.iter().map(|&k| (k, 1e-4))),
            sim: keys.iter().map(|&k| (k, Value::Float(0.0))).collect(),
            outbox: VecDeque::new(),
            writes: Vec::new(),
        };
        // Seed every key.
        s.frame(0, 0.0);
        s
    }

    fn apply(&mut self, actions: Vec<Action>) {
        for action in actions {
            match action {
                Action::Write { key, value } => {
                    self.sim.insert(key, value);
                    self.writes.push((key, value));
                }
                Action::Send(m) => self.outbox.push_back(m),
                Action::Muted { .. } => {}
            }
        }
    }

    /// One simulator frame: read every key, then flush rate-limited sends.
    fn frame(&mut self, frame: u64, now: f64) {
        let keys: Vec<u16> = self.sim.keys().copied().collect();
        for key in keys {
            let actions = self.reg.observe(key, self.sim[&key], frame, now);
            self.apply(actions);
        }
        let actions = self.reg.flush(now);
        self.apply(actions);
    }

    /// Receives one message from the other seat.
    fn receive(&mut self, message: Control) {
        let actions = match message {
            Control::Change { key, value, req } => self.reg.receive_change(key, value, req),
            Control::Set { key, value, ack } => self.reg.receive_set(key, value, ack),
            other => panic!("unexpected {other:?}"),
        };
        self.apply(actions);
    }
}

fn deliver(from: &mut SeatSim, to: &mut SeatSim) {
    while let Some(m) = from.outbox.pop_front() {
        to.receive(m);
    }
}

/// Runs frames with immediate delivery until nothing more is sent.
fn settle(host: &mut SeatSim, crew: &mut SeatSim, mut frame: u64) -> u64 {
    for _ in 0..30 {
        frame += 1;
        let now = frame as f64 * FRAME_S;
        host.frame(frame, now);
        crew.frame(frame, now);
        deliver(host, crew);
        deliver(crew, host);
    }
    frame
}

fn pair(keys: &[u16]) -> (SeatSim, SeatSim) {
    (
        SeatSim::new(Seat::Host, keys),
        SeatSim::new(Seat::Crew, keys),
    )
}

#[test]
fn a_single_change_propagates_both_ways() {
    let (mut host, mut crew) = pair(&[0, 1]);
    crew.sim.insert(0, Value::Float(1.0));
    let frame = settle(&mut host, &mut crew, 1);
    assert_eq!(host.sim[&0], Value::Float(1.0));
    host.sim.insert(1, Value::Float(0.5));
    settle(&mut host, &mut crew, frame);
    assert_eq!(crew.sim[&1], Value::Float(0.5));
    assert_eq!(host.sim, crew.sim);
}

#[test]
fn simultaneous_changes_converge() {
    for crew_first in [false, true] {
        let (mut host, mut crew) = pair(&[7]);
        host.sim.insert(7, Value::Float(90.0));
        crew.sim.insert(7, Value::Float(270.0));
        let now = FRAME_S;
        host.frame(1, now);
        crew.frame(1, now);
        if crew_first {
            deliver(&mut crew, &mut host);
            deliver(&mut host, &mut crew);
        } else {
            deliver(&mut host, &mut crew);
            deliver(&mut crew, &mut host);
        }
        settle(&mut host, &mut crew, 1);
        assert_eq!(host.sim[&7], crew.sim[&7], "crew first: {crew_first}");
    }
}

#[test]
fn a_drag_never_snaps_back_on_the_dragging_seat() {
    let (mut host, mut crew) = pair(&[3]);
    // The link delays host -> crew by 6 frames (100 ms).
    let mut in_flight: VecDeque<(u64, Control)> = VecDeque::new();
    let mut frame = 1;
    for step in 0..120 {
        frame += 1;
        let now = frame as f64 * FRAME_S;
        let dragged = Value::Float(step as f32 / 120.0);
        crew.sim.insert(3, dragged);
        crew.frame(frame, now);
        host.frame(frame, now);
        deliver(&mut crew, &mut host);
        while let Some(m) = host.outbox.pop_front() {
            in_flight.push_back((frame + 6, m));
        }
        while in_flight.front().is_some_and(|(at, _)| *at <= frame) {
            let (_, m) = in_flight.pop_front().unwrap();
            crew.receive(m);
        }
        // The crew's lever is never moved by the register during the drag.
        assert!(
            crew.writes.is_empty(),
            "snapped back at step {step}: {:?}",
            crew.writes
        );
    }
    for (_, m) in in_flight.drain(..) {
        crew.receive(m);
    }
    settle(&mut host, &mut crew, frame);
    assert_eq!(host.sim[&3], Value::Float(119.0 / 120.0));
    assert_eq!(crew.sim[&3], host.sim[&3]);
}

#[test]
fn the_trailing_value_is_always_sent() {
    let (mut host, mut crew) = pair(&[2]);
    // Three changes within 50 ms: only the first goes out at once.
    for (i, v) in [0.1f32, 0.2, 0.3].into_iter().enumerate() {
        crew.sim.insert(2, Value::Float(v));
        crew.frame(1 + i as u64, 1.0 + i as f64 * 0.01);
    }
    assert_eq!(crew.outbox.len(), 1);
    crew.frame(10, 1.2);
    assert_eq!(crew.outbox.len(), 2);
    assert!(matches!(
        crew.outbox.back(),
        Some(Control::Change { value: Value::Float(v), .. }) if *v == 0.3
    ));
    deliver(&mut crew, &mut host);
    assert_eq!(host.sim[&2], Value::Float(0.3));
}

#[test]
fn values_the_plugin_wrote_are_not_echoed() {
    let (mut host, mut crew) = pair(&[5]);
    crew.sim.insert(5, Value::Int(1));
    crew.frame(1, 0.1);
    deliver(&mut crew, &mut host);
    // The host wrote the crew's value; observing it is not a local change.
    host.outbox.clear();
    host.frame(2, 0.2);
    assert!(host.outbox.is_empty());
    host.reg.wrote(5, Value::Int(7));
    host.sim.insert(5, Value::Int(7));
    host.frame(3, 0.3);
    assert!(host.outbox.is_empty());
}

#[test]
fn changes_inside_the_replay_window_are_absorbed() {
    let (mut host, _) = pair(&[1, 2]);
    host.reg.note_replay(10);
    host.sim.insert(1, Value::Int(1));
    host.frame(11, 1.0);
    host.frame(12, 1.1);
    assert!(host.outbox.is_empty(), "{:?}", host.outbox);
    host.sim.insert(2, Value::Int(1));
    host.frame(13, 1.2);
    assert_eq!(host.outbox.len(), 1);
}

#[test]
fn a_key_changing_on_its_own_is_muted() {
    let (mut host, _) = pair(&[4]);
    let mut muted = false;
    for second in 0..25 {
        host.sim.insert(4, Value::Float(second as f32));
        let actions = host
            .reg
            .observe(4, host.sim[&4], 100 + second, 10.0 + second as f64);
        muted |= actions.contains(&Action::Muted { key: 4 });
    }
    assert!(muted);
    assert!(host.reg.is_muted(4));
    assert_eq!(host.reg.keys().count(), 0);
    // A person working one lever for a few seconds is not muted.
    let (mut host, _) = pair(&[4]);
    for step in 0..300 {
        host.sim.insert(4, Value::Float(step as f32));
        host.frame(step + 1, 10.0 + step as f64 / 60.0);
    }
    assert!(!host.reg.is_muted(4));
}

// Scenario: the autopilot trims for a long time on the pilot flying.
#[test]
fn muted_keys_leave_the_snapshot_and_unmute_on_handover() {
    let (mut host, _) = pair(&[4, 5]);
    for second in 0..25 {
        host.sim.insert(4, Value::Float(second as f32));
        let actions = host
            .reg
            .observe(4, host.sim[&4], 100 + second, 10.0 + second as f64);
        host.apply(actions);
    }
    assert_eq!(host.reg.muted_keys(), vec![4]);
    // Its stale value is neither repaired nor snapshotted.
    let keys: Vec<u16> = host.reg.snapshot().iter().map(|(k, _)| *k).collect();
    assert_eq!(keys, vec![5]);
    // The controls change hands: it is a normal shared value again.
    host.reg.unmute_all();
    assert!(host.reg.muted_keys().is_empty());
    host.outbox.clear();
    host.sim.insert(4, Value::Float(99.0));
    host.frame(200, 60.0);
    assert_eq!(host.outbox.len(), 1);
    // A pilot monitoring told to follow the pilot flying's value mutes it.
    host.reg.mute(5);
    host.sim.insert(5, Value::Float(7.0));
    host.frame(201, 61.0);
    assert_eq!(host.outbox.len(), 1);
}

#[test]
fn snapshot_seeds_values_without_echo() {
    let (mut host, mut crew) = pair(&[0, 1]);
    host.sim.insert(0, Value::Int(1));
    host.sim.insert(1, Value::Double(29.92));
    host.frame(1, 0.1);
    host.outbox.clear();
    let snapshot = host.reg.snapshot();
    let actions = crew.reg.apply_snapshot(&snapshot);
    crew.apply(actions);
    assert_eq!(crew.sim, host.sim);
    crew.frame(2, 0.2);
    assert!(crew.outbox.is_empty());
}

#[test]
fn commands_are_forwarded_once_and_not_echoed() {
    let mut c = Commands::default();
    // A click is a Begin and an End.
    assert_eq!(
        c.local(3, HandlerPhase::Begin),
        Some(Control::Command {
            key: 3,
            phase: CommandPhase::Begin
        })
    );
    assert_eq!(c.local(3, HandlerPhase::Continue), None);
    assert_eq!(
        c.local(3, HandlerPhase::End),
        Some(Control::Command {
            key: 3,
            phase: CommandPhase::End
        })
    );
    // Replaying a command from the other seat sends nothing back.
    assert!(c.remote(4, CommandPhase::Begin));
    c.set_replaying(true);
    assert_eq!(c.local(4, HandlerPhase::Begin), None);
    c.set_replaying(false);
    // A Begin without its End is ended when the session ends.
    assert!(!c.remote(4, CommandPhase::Begin), "duplicate Begin");
    assert!(c.remote(9, CommandPhase::Begin));
    assert!(c.remote(9, CommandPhase::End));
    assert!(!c.remote(9, CommandPhase::End), "stray End");
    assert_eq!(c.end_all(), vec![4]);
    assert!(c.end_all().is_empty());
}

#[test]
fn repair_chunks_cycle_through_every_key_within_budget() {
    let values: Vec<(u16, Value)> = (0..400).map(|k| (k, Value::Double(k as f64))).collect();
    let mut repair = Repair::default();
    let mut seen = BTreeSet::new();
    for _ in 0..6 {
        let chunk = repair.next_chunk(&values);
        let bytes: usize = chunk.iter().map(|(_, v)| entry_size(*v)).sum();
        assert!(bytes <= REPAIR_BYTES);
        let encoded = flyx_protocol::Datagram::Systems {
            epoch: u32::MAX,
            state: vec![],
            repair: chunk.clone(),
        }
        .encode();
        assert!(encoded.len() < 1000);
        seen.extend(chunk.into_iter().map(|(k, _)| k));
    }
    assert_eq!(seen.len(), 400);
}

#[test]
fn repair_waits_for_quiet_and_reports_unwritable_keys_once() {
    let mut repair = Repair::default();
    repair.activity(10.0);
    let chunk = [(1, Value::Int(1)), (2, Value::Int(5)), (3, Value::Int(0))];
    let local = |key: u16| match key {
        1 => Some((Value::Int(1), 0.0)),
        2 => Some((Value::Int(4), 0.0)),
        _ => None,
    };
    assert_eq!(
        repair.select(12.0, &chunk, local),
        RepairSelection::default()
    );
    let picked = repair.select(13.5, &chunk, local);
    assert_eq!(picked.repair, vec![(2, Value::Int(5))]);
    assert_eq!(picked.unwritable, vec![3]);
    assert!(repair.select(14.0, &chunk, local).unwritable.is_empty());
}

#[test]
fn repair_goes_through_the_register() {
    let (mut host, mut crew) = pair(&[6]);
    // The crew repairs to the pilot flying's value: it is written and
    // sent to the host as a change from the crew.
    let actions = crew.reg.repair(6, Value::Int(3), 5.0);
    crew.apply(actions);
    assert_eq!(crew.sim[&6], Value::Int(3));
    assert!(matches!(
        crew.outbox.front(),
        Some(Control::Change { key: 6, .. })
    ));
    deliver(&mut crew, &mut host);
    assert_eq!(host.sim[&6], Value::Int(3));
    // The host's own repair is broadcast as a Set.
    let actions = host.reg.repair(6, Value::Int(4), 6.0);
    host.apply(actions);
    deliver(&mut crew, &mut host);
    assert!(matches!(
        host.outbox.back(),
        Some(Control::Set {
            key: 6,
            ack: None,
            ..
        })
    ));
}
