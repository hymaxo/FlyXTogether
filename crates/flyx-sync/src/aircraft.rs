//! Identifying the loaded aircraft, and how seats compare aircraft. Every
//! aircraft is supported; what is synced for it comes from its sync
//! definition (see [`crate::definition`]).

use std::path::Path;

use flyx_protocol::AircraftId;

/// Identifies an aircraft from the full path of its `.acf` file and the
/// name X-Plane shows for it. A path that is not an `.acf` file (X-Plane
/// reports the install folder before any aircraft has loaded) gives an
/// empty identity.
pub fn identify(acf_path: &Path, ui_name: &str) -> AircraftId {
    let is_acf = acf_path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("acf"));
    if !is_acf {
        return AircraftId {
            folder: String::new(),
            acf: String::new(),
            name: String::new(),
        };
    }
    let name = |p: Option<&Path>| {
        p.and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    };
    AircraftId {
        folder: name(acf_path.parent()),
        acf: name(Some(acf_path)),
        name: ui_name.trim().to_owned(),
    }
}

/// Whether an aircraft is loaded at all.
pub fn is_loaded(id: &AircraftId) -> bool {
    !id.acf.is_empty()
}

/// Whether two seats have the same aircraft loaded (same folder and `.acf`;
/// the variants of an aircraft are different aircraft).
pub fn same_aircraft(a: &AircraftId, b: &AircraftId) -> bool {
    a.folder.eq_ignore_ascii_case(&b.folder) && a.acf.eq_ignore_ascii_case(&b.acf)
}

/// Human-readable name: X-Plane's name for it, else the `.acf` file name.
pub fn display_name(id: &AircraftId) -> String {
    if !is_loaded(id) {
        "no aircraft".to_owned()
    } else if !id.name.is_empty() {
        id.name.clone()
    } else {
        id.acf
            .strip_suffix(".acf")
            .unwrap_or(&id.acf)
            .replace('_', " ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(folder: &str, acf: &str, name: &str) -> AircraftId {
        AircraftId {
            folder: folder.into(),
            acf: acf.into(),
            name: name.into(),
        }
    }

    #[test]
    fn identifies_from_xplane_path() {
        let path = Path::new(
            "D:/Games/X-Plane 12/Aircraft/Laminar Research/Cessna 172 SP/Cessna_172SP_G1000.acf",
        );
        assert_eq!(
            identify(path, " Cessna 172 SP G1000 "),
            id(
                "Cessna 172 SP",
                "Cessna_172SP_G1000.acf",
                "Cessna 172 SP G1000"
            )
        );
    }

    #[test]
    fn non_acf_path_is_no_aircraft() {
        let none = identify(Path::new("D:/Games/X-Plane 12/"), "x");
        assert_eq!(none, id("", "", ""));
        assert!(!is_loaded(&none));
        assert_eq!(display_name(&none), "no aircraft");
    }

    #[test]
    fn display_names() {
        assert_eq!(display_name(&id("A", "a321.acf", "A321neo")), "A321neo");
        assert_eq!(display_name(&id("A", "Baron_58.acf", "")), "Baron 58");
    }

    #[test]
    fn variants_are_different_aircraft_and_names_do_not_matter() {
        let base = id("Cessna 172 SP", "Cessna_172SP.acf", "Cessna 172 SP");
        let sea = id(
            "Cessna 172 SP",
            "Cessna_172SP_seaplane.acf",
            "Cessna 172 SP",
        );
        assert!(!same_aircraft(&base, &sea));
        assert!(same_aircraft(
            &base,
            &id("CESSNA 172 SP", "cessna_172sp.acf", "Skyhawk")
        ));
    }
}
