//! Cockpit sync for the headless peer: the same sync definition and
//! register as the plugin, over a simulated set of cockpit values.

use std::collections::BTreeMap;
use std::path::Path;

use flyx_net::{NetCommand, NetHandle};
use flyx_protocol::{AircraftId, Control, FlightState, MAX_INPUTS, Seat, Value};
use flyx_sync::aircraft;
use flyx_sync::cockpit::{Action, Commands, HandlerPhase, Register, Repair};
use flyx_sync::definition::{
    Aircraft, Class, Definition, Manipulators, Profile, Target, builtin, manip,
};
use tracing::{info, warn};

/// Systems datagrams per second while pilot flying.
const SYSTEMS_INTERVAL_S: f64 = 0.5;

#[derive(Debug, Clone, PartialEq)]
pub enum ScriptAction {
    Set { name: String, value: Value },
    Press { command: String },
    Hold { command: String, seconds: f64 },
    Take,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ScriptStep {
    /// Seconds after the session connected.
    pub at: f64,
    pub action: ScriptAction,
}

impl ScriptStep {
    pub fn take(at: f64) -> Self {
        Self {
            at,
            action: ScriptAction::Take,
        }
    }

    pub fn is_take(&self) -> bool {
        self.action == ScriptAction::Take
    }

    /// Parses `<seconds>:set <dataref>=<value>`, `<seconds>:press <command>`,
    /// `<seconds>:hold <command> <seconds>` or `<seconds>:take`.
    pub fn parse(text: &str) -> Result<Self, String> {
        let (at, rest) = text.split_once(':').ok_or("expected <seconds>:<action>")?;
        let at: f64 = at.trim().parse().map_err(|_| "bad time")?;
        let mut words = rest.split_whitespace();
        let action = match (words.next(), words.next(), words.next()) {
            (Some("set"), Some(assignment), None) => {
                let (name, value) = assignment.split_once('=').ok_or("expected name=value")?;
                let value = if value.contains('.') {
                    Value::Float(value.parse().map_err(|_| "bad value")?)
                } else {
                    Value::Int(value.parse().map_err(|_| "bad value")?)
                };
                ScriptAction::Set {
                    name: name.to_owned(),
                    value,
                }
            }
            (Some("press"), Some(command), None) => ScriptAction::Press {
                command: command.to_owned(),
            },
            (Some("hold"), Some(command), Some(seconds)) => ScriptAction::Hold {
                command: command.to_owned(),
                seconds: seconds.parse().map_err(|_| "bad hold time")?,
            },
            (Some("take"), None, None) => ScriptAction::Take,
            _ => return Err("unknown action".into()),
        };
        Ok(Self { at, action })
    }
}

pub struct Cockpit {
    pub aircraft: AircraftId,
    pub definition: Definition,
    keys: BTreeMap<String, u16>,
    seat: Seat,
    register: Option<Register>,
    commands: Commands,
    repair: Repair,
    /// The simulated cockpit values this peer knows.
    values: BTreeMap<u16, Value>,
    /// Scripted held commands: when to release them.
    holds: Vec<(f64, u16)>,
    next_systems: f64,
}

impl Cockpit {
    /// Builds the sync definition of the aircraft at `acf_path`.
    pub fn load(acf_path: &Path, engines: usize, profiles_dir: &Path) -> Result<Self, String> {
        let acf_text = std::fs::read(acf_path)
            .map(|b| String::from_utf8_lossy(&b).into_owned())
            .map_err(|e| format!("cannot read {}: {e}", acf_path.display()))?;
        let mut manipulators = Manipulators::default();
        for path in manip::object_paths(acf_path, &acf_text) {
            if let Ok(bytes) = std::fs::read(&path) {
                manipulators.extend(manip::parse_object(&String::from_utf8_lossy(&bytes)));
            }
        }
        let mut profiles = Vec::new();
        if let Ok(dir) = std::fs::read_dir(profiles_dir) {
            for entry in dir.flatten() {
                let path = entry.path();
                if path.extension().is_some_and(|e| e == "toml") {
                    let text = std::fs::read_to_string(&path).unwrap_or_default();
                    match Profile::parse(&path.display().to_string(), &text) {
                        Ok(p) => profiles.push(p),
                        Err(e) => warn!("{e}"),
                    }
                }
            }
        }
        let aircraft = aircraft::identify(acf_path, "");
        let definition = Definition::build(
            &Aircraft {
                folder: &aircraft.folder,
                acf: &aircraft.acf,
                engines,
                manipulators: &manipulators,
            },
            &builtin(),
            &profiles,
        );
        info!(
            acf = %aircraft.acf,
            commands = definition.count(Class::Command),
            shared = definition.count(Class::Shared),
            state = definition.count(Class::State),
            verified = definition.verified,
            "sync definition"
        );
        let keys = definition
            .entries
            .iter()
            .enumerate()
            .map(|(i, e)| (e.target.to_string(), i as u16))
            .collect();
        Ok(Self {
            aircraft,
            definition,
            keys,
            seat: Seat::Host,
            register: None,
            commands: Commands::default(),
            repair: Repair::default(),
            values: BTreeMap::new(),
            holds: Vec::new(),
            next_systems: 0.0,
        })
    }

    fn name(&self, key: u16) -> String {
        self.definition
            .entries
            .get(key as usize)
            .map(|e| e.target.to_string())
            .unwrap_or_else(|| format!("#{key}"))
    }

    fn is_shared(&self, key: u16) -> bool {
        self.definition
            .entries
            .get(key as usize)
            .is_some_and(|e| e.class == Class::Shared)
    }

    pub fn connected(&mut self, is_host: bool, net: &NetHandle) {
        self.seat = if is_host { Seat::Host } else { Seat::Crew };
        let keys = self
            .definition
            .of_class(Class::Shared)
            .map(|(k, e)| (k as u16, e.epsilon));
        let mut register = Register::new(self.seat, keys);
        for (&key, &value) in &self.values {
            register.wrote(key, value);
        }
        if is_host {
            let values = register.snapshot();
            info!(values = values.len(), "sending the join snapshot");
            net.send(NetCommand::SendCockpit(Control::Snapshot { values }));
        }
        self.register = Some(register);
    }

    pub fn disconnected(&mut self) {
        for key in self.commands.end_all() {
            info!(command = %self.name(key), "ending a command the other seat held");
        }
        self.register = None;
    }

    fn apply(&mut self, actions: Vec<Action>, net: &NetHandle) {
        for action in actions {
            match action {
                Action::Write { key, value } => {
                    info!(value = ?value, "{} written", self.name(key));
                    self.values.insert(key, value);
                }
                Action::Send(message) => net.send(NetCommand::SendCockpit(message)),
                Action::Muted { key } => warn!("{} muted: it changes on its own", self.name(key)),
            }
        }
    }

    pub fn receive(&mut self, message: Control, frame: u64, now: f64, net: &NetHandle) {
        let Some(register) = &mut self.register else {
            return;
        };
        let actions = match message {
            Control::Change { key, value, req } => {
                self.repair.activity(now);
                register.receive_change(key, value, req)
            }
            Control::Set { key, value, ack } => {
                self.repair.activity(now);
                register.receive_set(key, value, ack)
            }
            Control::Snapshot { values } => {
                info!(values = values.len(), "join snapshot received");
                register.apply_snapshot(&values)
            }
            Control::Command { key, phase } => {
                self.repair.activity(now);
                if self.commands.remote(key, phase) {
                    register.note_replay(frame);
                    info!(?phase, "other seat: {}", self.name(key));
                }
                vec![]
            }
            other => {
                info!(?other, "cockpit message");
                vec![]
            }
        };
        self.apply(actions, net);
    }

    pub fn systems(
        &mut self,
        epoch: u32,
        state: &[(u16, Value)],
        repair: &[(u16, Value)],
        now: f64,
        net: &NetHandle,
    ) {
        tracing::debug!(
            epoch,
            state = state.len(),
            repair = repair.len(),
            "systems state"
        );
        let values = &self.values;
        let selection = self.repair.select(now, repair, |key| {
            // The peer can write every key; a key it does not know differs.
            Some((
                values.get(&key).copied().unwrap_or(Value::Int(i32::MIN)),
                1e-4,
            ))
        });
        for (key, value) in selection.repair {
            if !self.is_shared(key) {
                continue;
            }
            info!(value = ?value, "repairing {}", self.name(key));
            let actions = match &mut self.register {
                Some(r) => r.repair(key, value, now),
                None => vec![],
            };
            self.apply(actions, net);
        }
    }

    /// One frame: notice local changes, send due messages, release held
    /// commands, and send systems state while pilot flying.
    pub fn frame(&mut self, frame: u64, now: f64, streaming: Option<u32>, net: &NetHandle) {
        let Some(register) = &mut self.register else {
            return;
        };
        let mut actions = Vec::new();
        for (&key, &value) in &self.values {
            actions.extend(register.observe(key, value, frame, now));
        }
        actions.extend(register.flush(now));
        self.apply(actions, net);

        let due: Vec<u16> = self
            .holds
            .iter()
            .filter(|(until, _)| *until <= now)
            .map(|(_, key)| *key)
            .collect();
        self.holds.retain(|(until, _)| *until > now);
        for key in due {
            self.send_command(key, HandlerPhase::End, net);
        }

        if let Some(epoch) = streaming
            && now >= self.next_systems
            && let Some(register) = &self.register
        {
            self.next_systems = now + SYSTEMS_INTERVAL_S;
            let repair = self.repair.next_chunk(&register.snapshot());
            net.send(NetCommand::SendSystems {
                epoch,
                state: self.state_values(),
                repair,
            });
        }
    }

    fn send_command(&mut self, key: u16, phase: HandlerPhase, net: &NetHandle) {
        if let Some(message) = self.commands.local(key, phase) {
            info!(?phase, "pressing {}", self.name(key));
            net.send(NetCommand::SendCockpit(message));
        }
    }

    /// Runs one scripted step.
    pub fn run(&mut self, step: &ScriptStep, now: f64, net: &NetHandle) {
        self.repair.activity(now);
        match &step.action {
            ScriptAction::Set { name, value } => match self.keys.get(name.as_str()) {
                Some(&key) if self.is_shared(key) => {
                    info!(value = ?value, "script: setting {name}");
                    // Unlike X-Plane, the peer starts without values; give
                    // the register a previous value so this is a change.
                    if !self.values.contains_key(&key)
                        && let Some(register) = &mut self.register
                    {
                        register.wrote(key, Value::Int(i32::MIN));
                    }
                    self.values.insert(key, *value);
                }
                _ => warn!("script: {name} is not a shared value of this aircraft"),
            },
            ScriptAction::Press { command } | ScriptAction::Hold { command, .. } => {
                let key = self.keys.get(command.as_str()).copied().filter(|&k| {
                    matches!(
                        self.definition.entries[k as usize].target,
                        Target::Command(_)
                    )
                });
                let Some(key) = key else {
                    warn!("script: {command} is not a forwarded command of this aircraft");
                    return;
                };
                self.send_command(key, HandlerPhase::Begin, net);
                match step.action {
                    ScriptAction::Hold { seconds, .. } => self.holds.push((now + seconds, key)),
                    _ => self.send_command(key, HandlerPhase::End, net),
                }
            }
            ScriptAction::Take => {}
        }
    }

    /// Sets a shared value before connecting, so the host's join snapshot
    /// carries it.
    pub fn preset(&mut self, text: &str) -> Result<(), String> {
        let step = ScriptStep::parse(&format!("0:set {text}"))?;
        let ScriptAction::Set { name, value } = step.action else {
            unreachable!()
        };
        match self.keys.get(name.as_str()) {
            Some(&key) if self.is_shared(key) => {
                self.values.insert(key, value);
                Ok(())
            }
            _ => Err(format!("{name} is not a shared value of this aircraft")),
        }
    }

    /// Synthetic flight-control inputs matching `state`, in definition order.
    pub fn controls(&self, state: &FlightState) -> [f32; MAX_INPUTS] {
        let speed = state.velocity.iter().map(|v| v * v).sum::<f32>().sqrt();
        let throttle = if !state.on_ground {
            0.8
        } else if speed > 1.0 {
            0.25
        } else {
            0.05
        };
        let mut out = [0.0; MAX_INPUTS];
        let inputs = self.definition.of_class(Class::Input);
        for (slot, (_, entry)) in out.iter_mut().zip(inputs) {
            let name = entry.target.to_string();
            *slot = if name.contains("roll_ratio") {
                (state.phi_deg / 40.0).clamp(-1.0, 1.0)
            } else if name.contains("pitch_ratio") {
                (0.05 + state.theta_deg / 20.0).clamp(-1.0, 1.0)
            } else if name.contains("throttle_ratio") {
                throttle
            } else {
                0.0
            };
        }
        out
    }

    /// Synthetic systems state: fuel.
    fn state_values(&self) -> Vec<(u16, Value)> {
        self.definition
            .of_class(Class::State)
            .filter_map(|(key, entry)| {
                let Target::Dataref(d) = &entry.target else {
                    return None;
                };
                let value = match d.name.rsplit('/').next()? {
                    "m_fuel" if d.index.is_some_and(|i| i < 2) => 60.0,
                    _ => return None,
                };
                Some((key as u16, Value::Float(value)))
            })
            .collect()
    }

    /// Logs every known shared value, for comparing seats after a test.
    pub fn log_values(&self) {
        let list: Vec<String> = self
            .values
            .iter()
            .map(|(k, v)| format!("{}={:?}", self.name(*k), v))
            .collect();
        info!("final cockpit values: {}", list.join(", "));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_script_steps() {
        assert_eq!(
            ScriptStep::parse("2.5:set sim/a/b[1]=0.5").unwrap(),
            ScriptStep {
                at: 2.5,
                action: ScriptAction::Set {
                    name: "sim/a/b[1]".into(),
                    value: Value::Float(0.5)
                }
            }
        );
        assert_eq!(
            ScriptStep::parse("3:set sim/a/c=1").unwrap().action,
            ScriptAction::Set {
                name: "sim/a/c".into(),
                value: Value::Int(1)
            }
        );
        assert_eq!(
            ScriptStep::parse("4:hold laminar/c172/ignition_up 3")
                .unwrap()
                .action,
            ScriptAction::Hold {
                command: "laminar/c172/ignition_up".into(),
                seconds: 3.0
            }
        );
        assert!(ScriptStep::parse("5:take").unwrap().is_take());
        assert!(ScriptStep::parse("x:take").is_err());
        assert!(ScriptStep::parse("1:fly").is_err());
    }
}
