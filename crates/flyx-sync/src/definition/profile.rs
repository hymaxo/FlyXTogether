//! The TOML format shared by the built-in sync list and aircraft profiles.

use serde::Deserialize;

/// The format version this build reads.
pub const FORMAT: u32 = 1;

/// A parsed profile (or the built-in list).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    pub format: u32,
    #[serde(default)]
    pub name: String,
    /// Marks the matched aircraft as verified.
    #[serde(default)]
    pub verified: bool,
    #[serde(default)]
    pub variant: Vec<Variant>,
    #[serde(default)]
    pub shared: Vec<DatarefEntry>,
    #[serde(default)]
    pub command: Vec<CommandEntry>,
    #[serde(default)]
    pub state: Vec<DatarefEntry>,
    /// Flight-control inputs owned by the pilot flying.
    #[serde(default)]
    pub input: Vec<DatarefEntry>,
    /// Overrides the pilot monitoring sets while following.
    #[serde(default)]
    pub monitor_override: Vec<DatarefEntry>,
    #[serde(default)]
    pub local: Vec<LocalEntry>,
}

/// An aircraft a profile applies to.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Variant {
    pub id: String,
    /// Name of the folder holding the `.acf`.
    pub folder: String,
    pub acf: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DatarefEntry {
    pub dataref: String,
    #[serde(default)]
    pub index: Option<Index>,
    /// Smallest change of a float value that counts as a change.
    #[serde(default)]
    pub epsilon: Option<f32>,
    /// Momentary positions of a spring-loaded switch, never synced as a
    /// value (the commands that hold them are forwarded instead).
    #[serde(default)]
    pub transient: Vec<i32>,
    #[serde(default)]
    pub variants: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandEntry {
    pub command: String,
    #[serde(default)]
    pub variants: Vec<String>,
}

/// Commands or datarefs that must never be synced. Names may contain `*`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LocalEntry {
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default)]
    pub dataref: Option<String>,
    #[serde(default)]
    pub variants: Vec<String>,
}

/// Which elements of an array dataref an entry covers.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(untagged)]
pub enum Index {
    One(usize),
    List(Vec<usize>),
    /// `"per_engine"`: one element per engine of the aircraft.
    Word(String),
}

impl Index {
    /// The element indices for an aircraft with `engines` engines.
    pub fn indices(&self, engines: usize) -> Result<Vec<usize>, String> {
        match self {
            Index::One(i) => Ok(vec![*i]),
            Index::List(list) => Ok(list.clone()),
            Index::Word(w) if w == "per_engine" => Ok((0..engines).collect()),
            Index::Word(w) => Err(format!(
                "unknown index \"{w}\"; use a number, a list or \"per_engine\""
            )),
        }
    }
}

/// Why a profile could not be read.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{file}, line {line}: {message}")]
pub struct ProfileError {
    pub file: String,
    pub line: usize,
    pub message: String,
}

impl Profile {
    /// Parses and validates a profile. `file` names it in errors.
    pub fn parse(file: &str, text: &str) -> Result<Profile, ProfileError> {
        let error = |offset: Option<usize>, message: String| ProfileError {
            file: file.to_owned(),
            line: offset.map_or(1, |o| line_of(text, o)),
            message,
        };
        let profile: Profile = toml::from_str(text)
            .map_err(|e| error(e.span().map(|s| s.start), e.message().to_owned()))?;
        if profile.format != FORMAT {
            return Err(error(
                find(text, "format"),
                format!(
                    "format {} is not supported (this FlyXTogether reads format {FORMAT})",
                    profile.format
                ),
            ));
        }
        let ids: Vec<&str> = profile.variant.iter().map(|v| v.id.as_str()).collect();
        let check_variants = |variants: &[String]| -> Result<(), ProfileError> {
            match variants.iter().find(|v| !ids.contains(&v.as_str())) {
                Some(v) => Err(error(
                    find(text, &format!("\"{v}\"")),
                    format!("variant \"{v}\" is not defined in a [[variant]] table"),
                )),
                None => Ok(()),
            }
        };
        let datarefs = profile
            .shared
            .iter()
            .chain(&profile.state)
            .chain(&profile.input)
            .chain(&profile.monitor_override);
        for entry in datarefs {
            check_variants(&entry.variants)?;
            if let Some(index) = &entry.index {
                index
                    .indices(1)
                    .map_err(|m| error(find(text, &entry.dataref), m))?;
            }
            expand(&entry.dataref).map_err(|m| error(find(text, &entry.dataref), m))?;
        }
        for entry in &profile.command {
            check_variants(&entry.variants)?;
            expand(&entry.command).map_err(|m| error(find(text, &entry.command), m))?;
        }
        for entry in &profile.local {
            check_variants(&entry.variants)?;
            if entry.command.is_some() == entry.dataref.is_some() {
                let at = entry.command.as_deref().or(entry.dataref.as_deref());
                return Err(error(
                    at.and_then(|n| find(text, n)),
                    "a [[local]] entry needs exactly one of `command` or `dataref`".into(),
                ));
            }
        }
        Ok(profile)
    }

    /// The variant matching an aircraft, compared case-insensitively.
    pub fn variant_for(&self, folder: &str, acf: &str) -> Option<&Variant> {
        self.variant
            .iter()
            .find(|v| v.folder.eq_ignore_ascii_case(folder) && v.acf.eq_ignore_ascii_case(acf))
    }
}

/// Whether an entry limited to `variants` applies to `variant`.
pub fn applies(variants: &[String], variant: Option<&str>) -> bool {
    variants.is_empty() || variant.is_some_and(|v| variants.iter().any(|x| x == v))
}

fn line_of(text: &str, offset: usize) -> usize {
    text[..offset.min(text.len())].matches('\n').count() + 1
}

fn find(text: &str, needle: &str) -> Option<usize> {
    text.find(needle)
}

/// Expands `{a,b}` alternatives and `{1..12}` ranges, left to right.
pub fn expand(pattern: &str) -> Result<Vec<String>, String> {
    let Some(open) = pattern.find('{') else {
        if pattern.contains('}') {
            return Err(format!("unmatched `}}` in \"{pattern}\""));
        }
        return Ok(vec![pattern.to_owned()]);
    };
    let close = pattern[open..]
        .find('}')
        .map(|c| open + c)
        .ok_or_else(|| format!("unmatched `{{` in \"{pattern}\""))?;
    let (head, body, tail) = (
        &pattern[..open],
        &pattern[open + 1..close],
        &pattern[close + 1..],
    );
    let options: Vec<String> = match body.split_once("..") {
        Some((from, to)) => {
            let bad = || format!("bad range `{{{body}}}` in \"{pattern}\"");
            let from: u32 = from.parse().map_err(|_| bad())?;
            let to: u32 = to.parse().map_err(|_| bad())?;
            if from > to {
                return Err(bad());
            }
            (from..=to).map(|n| n.to_string()).collect()
        }
        None => body.split(',').map(str::to_owned).collect(),
    };
    let tails = expand(tail)?;
    Ok(options
        .iter()
        .flat_map(|o| tails.iter().map(move |t| format!("{head}{o}{t}")))
        .collect())
}

/// Matches `name` against a pattern where `*` stands for any text.
pub fn matches(pattern: &str, name: &str) -> bool {
    let parts: Vec<&str> = pattern.split('*').collect();
    if parts.len() == 1 {
        return pattern == name;
    }
    let (first, last) = (parts[0], parts[parts.len() - 1]);
    if !name.starts_with(first) || name.len() < first.len() + last.len() || !name.ends_with(last) {
        return false;
    }
    let mut rest = &name[first.len()..name.len() - last.len()];
    for part in &parts[1..parts.len() - 1] {
        match rest.find(part) {
            Some(at) => rest = &rest[at + part.len()..],
            None => return false,
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXAMPLE: &str = r#"
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
command = "sim/operation/slider_{20,24}"

[[command]]
command = "sim/GPS/g1000n{1,3}_softkey{1..12}"
variants = ["g1000"]

[[state]]
dataref = "sim/flightmodel/engine/ENGN_CHT_c"
index = "per_engine"
"#;

    #[test]
    fn parses_a_profile() {
        let p = Profile::parse("c172.toml", EXAMPLE).unwrap();
        assert!(p.verified);
        assert_eq!(p.variant.len(), 2);
        assert_eq!(
            p.variant_for("cessna 172 sp", "CESSNA_172SP_G1000.ACF")
                .map(|v| v.id.as_str()),
            Some("g1000")
        );
        assert_eq!(
            p.state[0].index.as_ref().unwrap().indices(2).unwrap(),
            [0, 1]
        );
    }

    #[test]
    fn expands_braces_in_order() {
        assert_eq!(
            expand("a{1..3}b{x,y}").unwrap(),
            ["a1bx", "a1by", "a2bx", "a2by", "a3bx", "a3by"]
        );
        assert_eq!(expand("plain").unwrap(), ["plain"]);
        assert_eq!(
            expand("sim/GPS/g1000n{1,3}_softkey{1..12}").unwrap().len(),
            24
        );
        assert!(expand("a{1..").is_err());
        assert!(expand("a{3..1}").is_err());
        assert!(expand("a}").is_err());
    }

    #[test]
    fn wildcards_match() {
        assert!(matches("sim/view/*", "sim/view/forward"));
        assert!(matches("*_popup", "sim/GPS/g430n1_popup"));
        assert!(matches("*popup*", "sim/GPS/g1000n1_popup_toggle"));
        assert!(matches("sim/*/x_*_y", "sim/a/x_1_y"));
        assert!(!matches("sim/view/*", "sim/views"));
        assert!(!matches("*_popup", "sim/GPS/popup_x"));
        assert!(matches("exact", "exact"));
    }

    #[test]
    fn filters_by_variant() {
        assert!(applies(&[], None));
        assert!(applies(&["g1000".into()], Some("g1000")));
        assert!(!applies(&["g1000".into()], Some("standard")));
        assert!(!applies(&["g1000".into()], None));
    }

    #[test]
    fn syntax_errors_name_the_line() {
        let text = "format = 1\nname = \"x\"\n\n[[shared]]\ndataref = \n";
        let e = Profile::parse("bad.toml", text).unwrap_err();
        assert_eq!(e.file, "bad.toml");
        assert_eq!(e.line, 5);
    }

    #[test]
    fn unknown_keys_are_rejected() {
        let e = Profile::parse("x.toml", "format = 1\n[[shared]]\ndatarf = \"a/b\"\n").unwrap_err();
        assert_eq!(e.line, 3);
        assert!(e.message.contains("datarf"), "{}", e.message);
    }

    #[test]
    fn other_checks() {
        assert!(
            Profile::parse("x", "format = 2\n")
                .unwrap_err()
                .message
                .contains("format 2")
        );
        let undefined = "format = 1\n[[command]]\ncommand = \"a/b\"\nvariants = [\"nope\"]\n";
        assert_eq!(Profile::parse("x", undefined).unwrap_err().line, 4);
        let both = "format = 1\n[[local]]\ncommand = \"a/b\"\ndataref = \"c/d\"\n";
        assert!(Profile::parse("x", both).is_err());
        let bad_index = "format = 1\n[[state]]\ndataref = \"a/b\"\nindex = \"each\"\n";
        assert_eq!(Profile::parse("x", bad_index).unwrap_err().line, 3);
    }
}
