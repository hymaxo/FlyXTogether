//! The session password, kept out of logs and debug output.

use std::fmt;

/// A session password. `Debug` and `Display` print `***`, so it cannot end
/// up in a log by accident. It is never persisted to disk.
#[derive(Clone, PartialEq, Eq, Default)]
pub struct Password(String);

impl Password {
    pub fn new(secret: impl Into<String>) -> Self {
        Self(secret.into())
    }

    /// The secret itself, for key derivation only.
    pub fn expose(&self) -> &str {
        &self.0
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Debug for Password {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("***")
    }
}

impl fmt::Display for Password {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("***")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_as_stars() {
        let p = Password::new("hunter2");
        assert_eq!(format!("{p}"), "***");
        assert_eq!(format!("{p:?}"), "***");
        assert_eq!(format!("{:?}", Some(p.clone())), "Some(***)");
        assert_eq!(p.expose(), "hunter2");
    }
}
