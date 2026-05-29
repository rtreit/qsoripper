//! Per-face capability sets and command classification.
//!
//! The coarse face flags (`read`, `write`, `ptt`, `config_write`) gate the command
//! classes a dialect assigns to each inbound command. Unknown passthrough writes default
//! to denied unless the face opts into unsafe full control.

/// How a dialect classifies a single inbound command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CommandClass {
    /// A modeled status read (served from the cache).
    ModeledRead,
    /// A modeled write (frequency, mode, split).
    ModeledWrite,
    /// A passthrough read (raw native query).
    PassthroughRead,
    /// A PTT / TX-affecting write.
    PttWrite,
    /// A persistent/config write (`EX` menu and similar).
    ConfigWrite,
    /// An auto-information toggle (virtualized; never reaches the radio). Reserved for
    /// dialects that route AI toggles through classification.
    #[allow(dead_code)]
    AutoInfoToggle,
    /// Denied or unknown. Reserved for dialects that classify unknown writes as denied.
    #[allow(dead_code)]
    Denied,
}

/// The capability set for one face or Hamlib listener.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(clippy::struct_excessive_bools)] // Each flag is an independent face capability.
pub(crate) struct FacePermissions {
    /// May read modeled state.
    pub(crate) read: bool,
    /// May issue modeled writes (frequency, mode, split).
    pub(crate) write: bool,
    /// May key PTT.
    pub(crate) ptt: bool,
    /// May issue persistent/config writes (`EX` menu).
    pub(crate) config_write: bool,
}

impl FacePermissions {
    /// A read-only face.
    #[cfg(test)]
    pub(crate) fn read_only() -> Self {
        FacePermissions {
            read: true,
            write: false,
            ptt: false,
            config_write: false,
        }
    }

    /// Parse a permission list from config tokens.
    pub(crate) fn from_tokens<S: AsRef<str>>(tokens: &[S]) -> Self {
        let mut perms = FacePermissions {
            read: false,
            write: false,
            ptt: false,
            config_write: false,
        };
        for token in tokens {
            match token.as_ref() {
                "read" => perms.read = true,
                "write" => perms.write = true,
                "ptt" => perms.ptt = true,
                "config_write" => perms.config_write = true,
                _ => {}
            }
        }
        perms
    }

    /// Whether this face is permitted to run a command of the given class.
    pub(crate) fn allows(self, class: CommandClass) -> bool {
        match class {
            CommandClass::ModeledRead | CommandClass::PassthroughRead => self.read,
            CommandClass::ModeledWrite => self.write,
            CommandClass::PttWrite => self.ptt,
            CommandClass::ConfigWrite => self.config_write,
            // Auto-info toggles are always allowed: they are virtualized per face and
            // never touch the radio.
            CommandClass::AutoInfoToggle => true,
            CommandClass::Denied => false,
        }
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    #[test]
    fn tokens_parse_into_flags() {
        let perms = FacePermissions::from_tokens(&["read", "write", "ptt"]);
        assert!(perms.read && perms.write && perms.ptt);
        assert!(!perms.config_write);
    }

    #[test]
    fn read_only_denies_writes_and_ptt() {
        let perms = FacePermissions::read_only();
        assert!(perms.allows(CommandClass::ModeledRead));
        assert!(!perms.allows(CommandClass::ModeledWrite));
        assert!(!perms.allows(CommandClass::PttWrite));
        assert!(!perms.allows(CommandClass::ConfigWrite));
    }

    #[test]
    fn auto_info_toggle_always_allowed() {
        let perms = FacePermissions::read_only();
        assert!(perms.allows(CommandClass::AutoInfoToggle));
    }

    #[test]
    fn config_write_requires_flag() {
        let perms = FacePermissions::from_tokens(&["read", "write", "ptt", "config_write"]);
        assert!(perms.allows(CommandClass::ConfigWrite));
    }
}
