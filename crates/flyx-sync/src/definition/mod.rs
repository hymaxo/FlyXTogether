//! An aircraft's sync definition: what both seats sync for it, built from
//! its cockpit objects, the built-in list and an optional profile.

pub mod manip;
pub mod profile;

use std::collections::{BTreeMap, BTreeSet};

use sha2::{Digest, Sha256};

pub use manip::{DatarefName, Manipulators};
pub use profile::{Profile, ProfileError};

use profile::{DatarefEntry, applies, expand, matches};

/// The built-in list, shared by every aircraft.
pub const BUILTIN_TOML: &str = include_str!("builtin.toml");

/// Parses the built-in list. It is tested, so a failure is a build bug.
pub fn builtin() -> Profile {
    Profile::parse("builtin.toml", BUILTIN_TOML).expect("the built-in sync list is valid")
}

/// How an entry is synced.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Class {
    /// A cockpit value either pilot can change.
    Shared,
    /// A cockpit command forwarded to the other seat.
    Command,
    /// A simulated value owned by the pilot flying.
    State,
    /// A flight-control input owned by the pilot flying.
    Input,
    /// An override the pilot monitoring sets while following.
    MonitorOverride,
}

impl Class {
    fn tag(self) -> &'static str {
        match self {
            Class::Shared => "shared",
            Class::Command => "command",
            Class::State => "state",
            Class::Input => "input",
            Class::MonitorOverride => "monitor_override",
        }
    }
}

/// What an entry names.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Target {
    Dataref(DatarefName),
    Command(String),
}

impl std::fmt::Display for Target {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Target::Dataref(d) => d.fmt(f),
            Target::Command(c) => f.write_str(c),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub class: Class,
    pub target: Target,
    /// Smallest change of a float value that counts as a change.
    pub epsilon: f32,
}

/// Default for float values without an `epsilon`.
pub const DEFAULT_EPSILON: f32 = 1e-4;

/// Version of the definition encoding hashed into the identity.
const IDENTITY_VERSION: &str = "flyx-definition-1";

/// The aircraft-specific inputs to [`Definition::build`].
#[derive(Debug, Clone)]
pub struct Aircraft<'a> {
    /// Name of the folder holding the `.acf`.
    pub folder: &'a str,
    pub acf: &'a str,
    pub engines: usize,
    /// What the cockpit objects' manipulators use.
    pub manipulators: &'a Manipulators,
}

/// An aircraft's complete sync definition.
#[derive(Debug, Clone)]
pub struct Definition {
    /// Sorted by class, then target. Message keys are indices into this.
    pub entries: Vec<Entry>,
    /// SHA-256 over the canonical entry list; seats must have equal ones.
    pub identity: [u8; 32],
    /// A matching profile marked the aircraft as verified.
    pub verified: bool,
    /// The matching profile and variant, if any.
    pub profile: Option<(String, String)>,
}

impl Definition {
    /// Builds the definition: generated entries, then the built-in list,
    /// then the first matching profile; local patterns are applied last.
    pub fn build(aircraft: &Aircraft, builtin: &Profile, profiles: &[Profile]) -> Definition {
        let mut entries: BTreeMap<(Class, Target), f32> = BTreeMap::new();
        let mut local_commands: Vec<String> = Vec::new();
        let mut local_datarefs: Vec<String> = Vec::new();

        for command in &aircraft.manipulators.commands {
            entries.insert((Class::Command, Target::Command(command.clone())), 0.0);
        }
        for dataref in &aircraft.manipulators.datarefs {
            entries.insert(
                (Class::Shared, Target::Dataref(dataref.clone())),
                DEFAULT_EPSILON,
            );
        }

        let matched = profiles.iter().find_map(|p| {
            p.variant_for(aircraft.folder, aircraft.acf)
                .map(|v| (p, v.id.clone()))
        });
        let mut sources: Vec<(&Profile, Option<&str>)> = vec![(builtin, None)];
        if let Some((p, variant)) = &matched {
            sources.push((p, Some(variant.as_str())));
        }
        for (source, variant) in sources {
            let mut add = |class, list: &[DatarefEntry]| {
                for entry in list.iter().filter(|e| applies(&e.variants, variant)) {
                    for (name, epsilon) in expand_dataref(entry, aircraft.engines) {
                        entries.insert((class, Target::Dataref(name)), epsilon);
                    }
                }
            };
            add(Class::Shared, &source.shared);
            add(Class::State, &source.state);
            add(Class::Input, &source.input);
            add(Class::MonitorOverride, &source.monitor_override);
            for entry in source
                .command
                .iter()
                .filter(|e| applies(&e.variants, variant))
            {
                for name in expand(&entry.command).unwrap_or_default() {
                    entries.insert((Class::Command, Target::Command(name)), 0.0);
                }
            }
            for entry in source
                .local
                .iter()
                .filter(|e| applies(&e.variants, variant))
            {
                if let Some(c) = &entry.command {
                    local_commands.extend(expand(c).unwrap_or_default());
                }
                if let Some(d) = &entry.dataref {
                    local_datarefs.extend(expand(d).unwrap_or_default());
                }
            }
        }

        // The pilot flying owns its inputs and state; they are never shared.
        let owned: BTreeSet<String> = entries
            .keys()
            .filter(|(class, _)| matches!(class, Class::Input | Class::State))
            .filter_map(|(_, target)| match target {
                Target::Dataref(d) => Some(d.name.clone()),
                Target::Command(_) => None,
            })
            .collect();
        entries.retain(|(class, target), _| match (class, target) {
            (Class::Shared, Target::Dataref(d)) => {
                !owned.contains(&d.name) && !local_datarefs.iter().any(|p| matches(p, &d.name))
            }
            (Class::State, Target::Dataref(d)) => {
                !local_datarefs.iter().any(|p| matches(p, &d.name))
            }
            (Class::Command, Target::Command(c)) => !local_commands.iter().any(|p| matches(p, c)),
            _ => true,
        });

        let entries: Vec<Entry> = entries
            .into_iter()
            .map(|((class, target), epsilon)| Entry {
                class,
                target,
                epsilon,
            })
            .collect();
        Definition {
            identity: identity(&entries),
            entries,
            verified: matched.as_ref().is_some_and(|(p, _)| p.verified),
            profile: matched.map(|(p, v)| (p.name.clone(), v)),
        }
    }

    pub fn count(&self, class: Class) -> usize {
        self.entries.iter().filter(|e| e.class == class).count()
    }

    /// Entries of one class with their keys (indices into `entries`).
    pub fn of_class(&self, class: Class) -> impl Iterator<Item = (usize, &Entry)> {
        self.entries
            .iter()
            .enumerate()
            .filter(move |(_, e)| e.class == class)
    }
}

fn expand_dataref(entry: &DatarefEntry, engines: usize) -> Vec<(DatarefName, f32)> {
    let epsilon = entry.epsilon.unwrap_or(DEFAULT_EPSILON);
    let indices: Vec<Option<usize>> = match &entry.index {
        None => vec![None],
        Some(index) => index
            .indices(engines)
            .unwrap_or_default()
            .into_iter()
            .map(Some)
            .collect(),
    };
    expand(&entry.dataref)
        .unwrap_or_default()
        .into_iter()
        .flat_map(|name| {
            indices.iter().map(move |&index| {
                (
                    DatarefName {
                        name: name.clone(),
                        index,
                    },
                    epsilon,
                )
            })
        })
        .collect()
}

fn identity(entries: &[Entry]) -> [u8; 32] {
    let mut hash = Sha256::new();
    hash.update(IDENTITY_VERSION.as_bytes());
    hash.update(b"\n");
    for e in entries {
        let line = format!("{}\t{}\t{}\n", e.class.tag(), e.target, e.epsilon);
        hash.update(line.as_bytes());
    }
    hash.finalize().into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manipulators(commands: &[&str], datarefs: &[&str]) -> Manipulators {
        Manipulators {
            commands: commands.iter().map(|c| c.to_string()).collect(),
            datarefs: datarefs
                .iter()
                .map(|d| DatarefName::parse(d).unwrap())
                .collect(),
            count: commands.len() + datarefs.len(),
        }
    }

    fn c172() -> Manipulators {
        manipulators(
            &[
                "laminar/c172/ignition_up",
                "laminar/c172/ignition_down",
                "sim/GPS/g430n1_com_ff",
                "sim/GPS/g430n1_popup",
                "sim/operation/toggle_yoke",
                "sim/operation/slider_20",
                "sim/audio_panel/monitor_audio_com1",
                "sim/lights/landing_lights_on",
            ],
            &[
                "sim/cockpit2/controls/flap_ratio",
                "sim/cockpit2/engine/actuators/throttle_ratio[0]",
                "sim/cockpit2/controls/yoke_pitch_ratio",
                "sim/cockpit2/radios/actuators/audio_volume_com1",
                "laminar/c172/knob_OAT",
            ],
        )
    }

    fn build(m: &Manipulators, profiles: &[Profile]) -> Definition {
        let aircraft = Aircraft {
            folder: "Cessna 172 SP",
            acf: "Cessna_172SP.acf",
            engines: 1,
            manipulators: m,
        };
        Definition::build(&aircraft, &builtin(), profiles)
    }

    fn names(d: &Definition, class: Class) -> Vec<String> {
        d.of_class(class)
            .map(|(_, e)| e.target.to_string())
            .collect()
    }

    #[test]
    fn shipped_c172_profile_matches_all_variants() {
        let p =
            Profile::parse("c172.toml", include_str!("../../../../profiles/c172.toml")).unwrap();
        assert!(p.verified);
        for (acf, id) in [
            ("Cessna_172SP.acf", "standard"),
            ("Cessna_172SP_G1000.acf", "g1000"),
            ("Cessna_172SP_seaplane.acf", "seaplane"),
        ] {
            assert_eq!(
                p.variant_for("Cessna 172 SP", acf).map(|v| v.id.as_str()),
                Some(id)
            );
        }
        let d = build(&c172(), std::slice::from_ref(&p));
        assert!(d.verified);
        assert!(
            names(&d, Class::State)
                .contains(&"sim/cockpit/electrical/battery_charge_watt_hr[1]".to_owned())
        );
    }

    /// Every TOML example in docs/profiles.md is a valid profile.
    #[test]
    fn profile_doc_examples_parse() {
        let doc = include_str!("../../../../docs/profiles.md");
        let mut examples = 0;
        for block in doc.split("```toml").skip(1) {
            let toml = block.split("```").next().unwrap();
            Profile::parse("docs/profiles.md", toml).unwrap();
            examples += 1;
        }
        assert!(examples >= 1);
    }

    #[test]
    fn builtin_list_is_valid() {
        let b = builtin();
        assert!(!b.shared.is_empty() && !b.input.is_empty() && !b.local.is_empty());
    }

    #[test]
    fn generated_commands_are_forwarded_except_local_ones() {
        let d = build(&c172(), &[]);
        assert_eq!(
            names(&d, Class::Command),
            [
                "laminar/c172/ignition_down",
                "laminar/c172/ignition_up",
                "sim/GPS/g430n1_com_ff",
                "sim/lights/landing_lights_on",
            ]
        );
        assert!(!d.verified);
        assert_eq!(d.profile, None);
    }

    #[test]
    fn inputs_and_local_values_are_not_shared() {
        let d = build(&c172(), &[]);
        let shared = names(&d, Class::Shared);
        assert!(shared.contains(&"sim/cockpit2/controls/flap_ratio".to_owned()));
        assert!(shared.contains(&"laminar/c172/knob_OAT".to_owned()));
        assert!(shared.contains(&"sim/cockpit2/switches/landing_lights_on".to_owned()));
        assert!(
            shared.contains(
                &"sim/cockpit2/radios/actuators/com2_standby_frequency_hz_833".to_owned()
            )
        );
        for not_shared in [
            "sim/cockpit2/engine/actuators/throttle_ratio[0]",
            "sim/cockpit2/controls/yoke_pitch_ratio",
            "sim/cockpit2/radios/actuators/audio_volume_com1",
        ] {
            assert!(!shared.contains(&not_shared.to_owned()), "{not_shared}");
        }
        assert!(
            names(&d, Class::Input)
                .contains(&"sim/cockpit2/engine/actuators/throttle_ratio[0]".to_owned())
        );
        // Overrides live under sim/operation/ but are not removed as local.
        assert_eq!(d.count(Class::MonitorOverride), 7);
    }

    #[test]
    fn per_engine_entries_follow_the_engine_count() {
        let m = c172();
        let twin = Aircraft {
            folder: "Baron",
            acf: "Baron_58.acf",
            engines: 2,
            manipulators: &m,
        };
        let d = Definition::build(&twin, &builtin(), &[]);
        let inputs = names(&d, Class::Input);
        assert!(inputs.contains(&"sim/cockpit2/engine/actuators/throttle_ratio[1]".to_owned()));
        assert!(!inputs.contains(&"sim/cockpit2/engine/actuators/throttle_ratio[2]".to_owned()));
    }

    #[test]
    fn identity_is_stable_and_content_sensitive() {
        let a = build(&c172(), &[]);
        let b = build(&c172(), &[]);
        assert_eq!(a.identity, b.identity);
        let mut more = c172();
        more.commands.insert("sim/lights/landing_lights_off".into());
        assert_ne!(build(&more, &[]).identity, a.identity);
    }

    #[test]
    fn a_profile_adds_makes_local_and_verifies() {
        let profile = Profile::parse(
            "c172.toml",
            r#"
format = 1
name = "Cessna 172 SP"
verified = true
[[variant]]
id = "standard"
folder = "Cessna 172 SP"
acf = "Cessna_172SP.acf"
[[variant]]
id = "g1000"
folder = "Cessna 172 SP"
acf = "Cessna_172SP_G1000.acf"
[[local]]
dataref = "laminar/c172/knob_OAT"
[[command]]
command = "sim/GPS/g1000n1_softkey{1..2}"
variants = ["g1000"]
[[state]]
dataref = "sim/cockpit/electrical/battery_charge_watt_hr"
index = [0]
"#,
        )
        .unwrap();
        let plain = build(&c172(), &[]);
        let d = build(&c172(), std::slice::from_ref(&profile));
        assert!(d.verified);
        assert_eq!(d.profile, Some(("Cessna 172 SP".into(), "standard".into())));
        assert!(!names(&d, Class::Shared).contains(&"laminar/c172/knob_OAT".to_owned()));
        assert!(
            names(&d, Class::State)
                .contains(&"sim/cockpit/electrical/battery_charge_watt_hr[0]".to_owned())
        );
        // The G1000-only commands do not apply to the standard variant.
        assert!(
            !names(&d, Class::Command)
                .iter()
                .any(|c| c.contains("g1000"))
        );
        assert_ne!(d.identity, plain.identity);
    }
}
