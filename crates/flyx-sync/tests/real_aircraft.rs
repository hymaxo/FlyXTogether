//! Builds sync definitions from the default aircraft of a local X-Plane 12
//! installation. Laminar's aircraft files cannot be redistributed, so this
//! test only runs when `XPLANE_ROOT` points at an installation:
//!
//! ```text
//! XPLANE_ROOT="/path/to/X-Plane 12" cargo test -p flyx-sync --test real_aircraft -- --nocapture
//! ```

use std::path::{Path, PathBuf};

use flyx_sync::definition::{Aircraft, Class, Definition, Manipulators, builtin, manip};

fn root() -> Option<PathBuf> {
    let root = PathBuf::from(std::env::var_os("XPLANE_ROOT")?);
    root.join("Aircraft").is_dir().then_some(root)
}

fn read_manipulators(acf_path: &Path) -> Manipulators {
    let acf = String::from_utf8_lossy(&std::fs::read(acf_path).unwrap()).into_owned();
    let mut all = Manipulators::default();
    for path in manip::object_paths(acf_path, &acf) {
        if let Ok(bytes) = std::fs::read(&path) {
            all.extend(manip::parse_object(&String::from_utf8_lossy(&bytes)));
        }
    }
    all
}

fn build(root: &Path, folder: &str, acf: &str, engines: usize) -> Definition {
    let path = root
        .join("Aircraft/Laminar Research")
        .join(folder)
        .join(acf);
    let manipulators = read_manipulators(&path);
    let d = Definition::build(
        &Aircraft {
            folder,
            acf,
            engines,
            manipulators: &manipulators,
        },
        &builtin(),
        &[],
    );
    println!(
        "{acf}: {} manipulator lines, {} commands, {} shared, {} state, {} inputs",
        manipulators.count,
        d.count(Class::Command),
        d.count(Class::Shared),
        d.count(Class::State),
        d.count(Class::Input),
    );
    d
}

fn has(d: &Definition, class: Class, name: &str) -> bool {
    d.of_class(class).any(|(_, e)| e.target.to_string() == name)
}

#[test]
fn default_c172_variants() {
    let Some(root) = root() else {
        eprintln!("XPLANE_ROOT not set; skipped");
        return;
    };
    for acf in [
        "Cessna_172SP.acf",
        "Cessna_172SP_G1000.acf",
        "Cessna_172SP_seaplane.acf",
    ] {
        let d = build(&root, "Cessna 172 SP", acf, 1);
        for command in [
            "laminar/c172/ignition_up",
            "laminar/c172/fuel_selector_up",
            "sim/lights/landing_lights_on",
        ] {
            assert!(has(&d, Class::Command, command), "{acf}: {command}");
        }
        assert!(has(&d, Class::Shared, "sim/cockpit2/controls/flap_ratio"));
        assert!(has(
            &d,
            Class::Shared,
            "sim/cockpit2/engine/actuators/mixture_ratio[0]"
        ));
        assert!(has(
            &d,
            Class::Input,
            "sim/cockpit2/engine/actuators/throttle_ratio[0]"
        ));
        assert!(!has(
            &d,
            Class::Shared,
            "sim/cockpit2/engine/actuators/throttle_ratio[0]"
        ));
        assert!(
            !d.of_class(Class::Command)
                .any(|(_, e)| e.target.to_string().contains("popup")),
            "{acf}: popups must stay local"
        );
        assert!(!has(&d, Class::Command, "sim/operation/toggle_yoke"));
    }
    let g1000 = build(&root, "Cessna 172 SP", "Cessna_172SP_G1000.acf", 1);
    assert!(has(&g1000, Class::Command, "sim/GPS/g1000n1_softkey1"));
    assert!(has(&g1000, Class::Command, "sim/GPS/g1000n3_ap"));
    let steam = build(&root, "Cessna 172 SP", "Cessna_172SP.acf", 1);
    assert!(has(&steam, Class::Command, "sim/GPS/g430n1_com_ff"));
    assert_ne!(steam.identity, g1000.identity);
}

#[test]
fn default_twin() {
    let Some(root) = root() else {
        eprintln!("XPLANE_ROOT not set; skipped");
        return;
    };
    let d = build(&root, "Beechcraft Baron 58", "Baron_58.acf", 2);
    assert!(d.count(Class::Command) > 20);
    assert!(has(
        &d,
        Class::Input,
        "sim/cockpit2/engine/actuators/throttle_ratio[1]"
    ));
}
