//! Which aircraft FlyXTogether supports, and how seats compare aircraft.

use std::path::Path;

use flyx_protocol::AircraftId;

/// An aircraft this version can sync.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SupportedAircraft {
    /// Folder containing the `.acf`.
    pub folder: &'static str,
    pub acf: &'static str,
    /// Name shown to users.
    pub name: &'static str,
}

/// The supported aircraft: the three X-Plane 12 Cessna 172 SP variants.
pub const SUPPORTED: &[SupportedAircraft] = &[
    SupportedAircraft {
        folder: "Cessna 172 SP",
        acf: "Cessna_172SP.acf",
        name: "Cessna 172 SP",
    },
    SupportedAircraft {
        folder: "Cessna 172 SP",
        acf: "Cessna_172SP_G1000.acf",
        name: "Cessna 172 SP G1000",
    },
    SupportedAircraft {
        folder: "Cessna 172 SP",
        acf: "Cessna_172SP_seaplane.acf",
        name: "Cessna 172 SP Seaplane",
    },
];

/// Identifies an aircraft from the full path of its `.acf` file. A path
/// that is not an `.acf` file (X-Plane reports the install folder before
/// any aircraft has loaded) gives an empty identity.
pub fn identify(acf_path: &Path) -> AircraftId {
    let is_acf = acf_path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("acf"));
    if !is_acf {
        return AircraftId {
            folder: String::new(),
            acf: String::new(),
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
    }
}

/// The supported-aircraft entry for `id`, compared case-insensitively.
pub fn lookup(id: &AircraftId) -> Option<&'static SupportedAircraft> {
    SUPPORTED
        .iter()
        .find(|s| s.folder.eq_ignore_ascii_case(&id.folder) && s.acf.eq_ignore_ascii_case(&id.acf))
}

pub fn is_supported(id: &AircraftId) -> bool {
    lookup(id).is_some()
}

/// Whether two seats have the same aircraft loaded (same folder and variant).
pub fn same_aircraft(a: &AircraftId, b: &AircraftId) -> bool {
    a.folder.eq_ignore_ascii_case(&b.folder) && a.acf.eq_ignore_ascii_case(&b.acf)
}

/// Human-readable name: the supported name, or the `.acf` without extension.
pub fn display_name(id: &AircraftId) -> String {
    match lookup(id) {
        Some(s) => s.name.to_owned(),
        None if id.acf.is_empty() => "no aircraft".to_owned(),
        None => id
            .acf
            .strip_suffix(".acf")
            .unwrap_or(&id.acf)
            .replace('_', " "),
    }
}

/// Comma-separated list of supported aircraft names, for messages.
pub fn supported_list() -> String {
    SUPPORTED
        .iter()
        .map(|s| s.name)
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(folder: &str, acf: &str) -> AircraftId {
        AircraftId {
            folder: folder.into(),
            acf: acf.into(),
        }
    }

    #[test]
    fn identifies_from_xplane_path() {
        let path = Path::new(
            "D:/Games/X-Plane 12/Aircraft/Laminar Research/Cessna 172 SP/Cessna_172SP_G1000.acf",
        );
        assert_eq!(
            identify(path),
            id("Cessna 172 SP", "Cessna_172SP_G1000.acf")
        );
    }

    #[test]
    fn non_acf_path_is_no_aircraft() {
        let id = identify(Path::new("D:/Games/X-Plane 12/"));
        assert_eq!(
            id,
            AircraftId {
                folder: String::new(),
                acf: String::new()
            }
        );
        assert!(!is_supported(&id));
        assert_eq!(display_name(&id), "no aircraft");
    }

    #[test]
    fn supported_variants_case_insensitive() {
        assert!(is_supported(&id("Cessna 172 SP", "Cessna_172SP.acf")));
        assert!(is_supported(&id(
            "cessna 172 sp",
            "CESSNA_172SP_SEAPLANE.ACF"
        )));
        assert_eq!(
            display_name(&id("Cessna 172 SP", "cessna_172sp_g1000.acf")),
            "Cessna 172 SP G1000"
        );
    }

    #[test]
    fn unsupported_aircraft() {
        let a321 = id("ToLissA321", "a321.acf");
        assert!(!is_supported(&a321));
        assert_eq!(display_name(&a321), "a321");
        // Right file name in a different folder is not the Laminar C172.
        assert!(!is_supported(&id("My C172", "Cessna_172SP.acf")));
        assert_eq!(display_name(&id("", "")), "no aircraft");
    }

    #[test]
    fn different_variants_are_different_aircraft() {
        let base = id("Cessna 172 SP", "Cessna_172SP.acf");
        let sea = id("Cessna 172 SP", "Cessna_172SP_seaplane.acf");
        assert!(!same_aircraft(&base, &sea));
        assert!(same_aircraft(
            &base,
            &id("CESSNA 172 SP", "cessna_172sp.acf")
        ));
    }

    #[test]
    fn supported_list_names_all_variants() {
        assert_eq!(
            supported_list(),
            "Cessna 172 SP, Cessna 172 SP G1000, Cessna 172 SP Seaplane"
        );
    }
}
