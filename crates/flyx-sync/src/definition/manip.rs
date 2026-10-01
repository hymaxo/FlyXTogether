//! Reads an aircraft's cockpit objects: which `.obj` files the `.acf`
//! attaches, and which commands and datarefs their manipulators use.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// A dataref name with an optional array index, as written in objects
/// and profiles (`sim/cockpit2/switches/panel_brightness_ratio[2]`).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DatarefName {
    pub name: String,
    pub index: Option<usize>,
}

impl DatarefName {
    /// Parses `name` or `name[index]`. `None` for things that are not
    /// dataref names (`none`, numbers, empty strings).
    pub fn parse(text: &str) -> Option<Self> {
        if !text.contains('/') {
            return None;
        }
        match text.split_once('[') {
            Some((name, rest)) => {
                let index = rest.strip_suffix(']')?.parse().ok()?;
                Some(Self {
                    name: name.to_owned(),
                    index: Some(index),
                })
            }
            None => Some(Self {
                name: text.to_owned(),
                index: None,
            }),
        }
    }
}

impl std::fmt::Display for DatarefName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.index {
            Some(i) => write!(f, "{}[{i}]", self.name),
            None => f.write_str(&self.name),
        }
    }
}

/// What the manipulators of one or more objects use.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Manipulators {
    pub commands: BTreeSet<String>,
    pub datarefs: BTreeSet<DatarefName>,
    /// Manipulator lines seen, including ones that name nothing.
    pub count: usize,
}

impl Manipulators {
    pub fn extend(&mut self, other: Manipulators) {
        self.commands.extend(other.commands);
        self.datarefs.extend(other.datarefs);
        self.count += other.count;
    }
}

/// What a manipulator kind names: how many numbers follow the cursor,
/// how many names follow those, and whether the names are commands.
fn layout(kind: &str) -> Option<(usize, usize, bool)> {
    Some(match kind {
        "command" | "command_knob2" | "command_switch_up_down2" | "command_switch_left_right2" => {
            (0, 1, true)
        }
        "command_knob" | "command_switch_up_down" | "command_switch_left_right" => (0, 2, true),
        "command_axis" => (3, 2, true),
        "drag_axis" | "drag_axis_pix" => (5, 1, false),
        "drag_xy" => (6, 2, false),
        "drag_rotate" => (13, 2, false),
        "push" | "toggle" => (2, 1, false),
        "radio" => (1, 1, false),
        "delta" | "wrap" => (4, 1, false),
        "axis_knob" | "axis_switch_up_down" | "axis_switch_left_right" => (4, 1, false),
        _ => return None,
    })
}

/// Parses the manipulator lines of an OBJ8 file.
pub fn parse_object(text: &str) -> Manipulators {
    let mut out = Manipulators::default();
    for line in text.lines() {
        let mut tokens = line.split_whitespace();
        let Some(kind) = tokens.next().and_then(|t| t.strip_prefix("ATTR_manip_")) else {
            continue;
        };
        out.count += 1;
        let Some((numbers, names, commands)) = layout(kind) else {
            continue;
        };
        // Skip the cursor and the numbers; the names follow, then the tooltip.
        for name in tokens.skip(1 + numbers).take(names) {
            if commands {
                if name.contains('/') {
                    out.commands.insert(name.to_owned());
                }
            } else if let Some(dataref) = DatarefName::parse(name) {
                out.datarefs.insert(dataref);
            }
        }
    }
    out
}

/// The objects an `.acf` attaches (`P _obja/<n>/_v10_att_file_stl <path>`),
/// as paths relative to the aircraft's `objects` folder.
pub fn attached_objects(acf: &str) -> Vec<String> {
    acf.lines()
        .filter_map(|line| {
            let mut tokens = line.split_whitespace();
            (tokens.next() == Some("P")).then_some(())?;
            let key = tokens.next()?;
            (key.starts_with("_obja/") && key.ends_with("/_v10_att_file_stl")).then_some(())?;
            let path = tokens.collect::<Vec<_>>().join(" ");
            (!path.is_empty()).then_some(path)
        })
        .collect()
}

/// The cockpit objects of the aircraft whose `.acf` is at `acf_path`: every
/// attached object, plus `<acf name>_cockpit.obj` next to the `.acf` (the
/// conventional 3D cockpit, which some aircraft do not attach explicitly).
/// Duplicates are removed; files are not checked for existence.
pub fn object_paths(acf_path: &Path, acf_text: &str) -> Vec<PathBuf> {
    let dir = acf_path.parent().unwrap_or(Path::new(""));
    let objects = dir.join("objects");
    let mut paths: Vec<PathBuf> = attached_objects(acf_text)
        .into_iter()
        .map(|rel| normalize(&objects.join(rel.replace('\\', "/"))))
        .collect();
    if let Some(stem) = acf_path.file_stem() {
        paths.push(dir.join(format!("{}_cockpit.obj", stem.to_string_lossy())));
    }
    let mut seen = BTreeSet::new();
    paths.retain(|p| seen.insert(p.clone()));
    paths
}

/// Resolves `..` and `.` without touching the file system.
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for part in path.components() {
        match part {
            std::path::Component::ParentDir => {
                out.pop();
            }
            std::path::Component::CurDir => {}
            other => out.push(other),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // Lines in the shape of the C172 SP cockpit object (X-Plane 12.4).
    const COCKPIT: &str = "\
I
800
OBJ
ATTR_manip_drag_rotate hand -0.054190 -0.213776 0.328208\t1.000000 0.000000 0.000000\t-1440.000000 1440.000000 0.000000\t-1.000000 1.000000  0.000000 0.000000 sim/cockpit2/controls/elevator_trim none Elevator trim
ATTR_manip_keyframe 0.000000 0.000000
ATTR_manip_wheel 0.005000
ATTR_manip_command_knob rotate_large laminar/c172/fuel_selector_up laminar/c172/fuel_selector_dwn FUEL SELECTOR
ATTR_manip_drag_axis hand 0.000000 -0.050285 0.000000 0.000000 1.000000 sim/cockpit2/controls/flap_ratio Flaps handle
ATTR_manip_command button sim/flight_controls/brakes_toggle_max Parking brake
ATTR_manip_drag_xy hand 0.1 0.1 -1 1 -1 1 sim/cockpit2/controls/yoke_roll_ratio sim/cockpit2/controls/yoke_pitch_ratio Yoke
\tATTR_manip_axis_knob rotate_small 0.000000 1.000000 0.050000 0.050000 sim/cockpit2/switches/panel_brightness_ratio[0] Flood light - pilot
ATTR_manip_command_switch_up_down up_down sim/systems/avionics_on sim/systems/avionics_off Avionics BUS 1/2
ATTR_manip_toggle hand 1 0 sim/cockpit2/radios/actuators/adf1_power ADF on / off
ATTR_manip_wrap hand 0 1 0 2 laminar/c172/knob_OAT Toggle OAT
ATTR_manip_noop
ATTR_manip_none
ATTR_manip_command_axis hand 0 0.1 0 sim/flight_controls/landing_gear_up sim/flight_controls/landing_gear_down GEAR
";

    #[test]
    fn finds_commands_and_datarefs() {
        let found = parse_object(COCKPIT);
        let commands: Vec<_> = found.commands.iter().map(String::as_str).collect();
        assert_eq!(
            commands,
            [
                "laminar/c172/fuel_selector_dwn",
                "laminar/c172/fuel_selector_up",
                "sim/flight_controls/brakes_toggle_max",
                "sim/flight_controls/landing_gear_down",
                "sim/flight_controls/landing_gear_up",
                "sim/systems/avionics_off",
                "sim/systems/avionics_on",
            ]
        );
        let datarefs: Vec<_> = found.datarefs.iter().map(|d| d.to_string()).collect();
        assert_eq!(
            datarefs,
            [
                "laminar/c172/knob_OAT",
                "sim/cockpit2/controls/elevator_trim",
                "sim/cockpit2/controls/flap_ratio",
                "sim/cockpit2/controls/yoke_pitch_ratio",
                "sim/cockpit2/controls/yoke_roll_ratio",
                "sim/cockpit2/radios/actuators/adf1_power",
                "sim/cockpit2/switches/panel_brightness_ratio[0]",
            ]
        );
        assert_eq!(found.count, 14);
    }

    #[test]
    fn tooltips_with_slashes_are_not_names() {
        let found = parse_object("ATTR_manip_command button sim/a/b Turn on/off\n");
        assert_eq!(found.commands.len(), 1);
    }

    #[test]
    fn parses_dataref_names() {
        assert_eq!(
            DatarefName::parse("sim/x/y[3]"),
            Some(DatarefName {
                name: "sim/x/y".into(),
                index: Some(3)
            })
        );
        assert_eq!(DatarefName::parse("none"), None);
        assert_eq!(DatarefName::parse("sim/x/y[a]"), None);
    }

    #[test]
    fn lists_attached_objects_and_the_cockpit() {
        let acf = "I\r\n1200 Version\r\nPROPERTIES_BEGIN\r\n\
                   P _obja/0/_obj_flags 6157\r\n\
                   P _obja/0/_v10_att_file_stl ../Cessna_172SP_cockpit.obj\r\n\
                   P _obja/1/_v10_att_file_stl Instruments/com nav/com.obj\r\n\
                   P _obja/2/_v10_att_file_stl \r\n";
        assert_eq!(
            attached_objects(acf),
            ["../Cessna_172SP_cockpit.obj", "Instruments/com nav/com.obj"]
        );
        let paths = object_paths(Path::new("/x/C172/Cessna_172SP.acf"), acf);
        assert_eq!(
            paths,
            [
                PathBuf::from("/x/C172/Cessna_172SP_cockpit.obj"),
                PathBuf::from("/x/C172/objects/Instruments/com nav/com.obj"),
            ]
        );
    }
}
