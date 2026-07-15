//! Per-endpoint capability sets and command classification.
//!
//! The endpoint flags gate the command classes a dialect assigns to each inbound command.
//! `frequency_write` grants narrow tuning authority without mode or VFO control, while
//! `write` retains full modeled-write authority. Unknown passthrough writes default to
//! denied unless the endpoint opts into unsafe full control.

use crate::model::StateMutation;

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

/// The capability set for one endpoint or Hamlib listener.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(clippy::struct_excessive_bools)] // Each flag is an independent endpoint capability.
pub(crate) struct EndpointPermissions {
    /// May read modeled state.
    pub(crate) read: bool,
    /// May issue modeled writes (frequency, mode, split).
    pub(crate) write: bool,
    /// May tune frequency, without authority to change mode, VFO, split, RIT, or XIT.
    pub(crate) frequency_write: bool,
    /// May key PTT.
    pub(crate) ptt: bool,
    /// May issue persistent/config writes (`EX` menu).
    pub(crate) config_write: bool,
}

impl EndpointPermissions {
    /// A read-only endpoint.
    #[cfg(test)]
    pub(crate) fn read_only() -> Self {
        EndpointPermissions {
            read: true,
            write: false,
            frequency_write: false,
            ptt: false,
            config_write: false,
        }
    }

    /// Parse a permission list from config tokens.
    pub(crate) fn from_tokens<S: AsRef<str>>(tokens: &[S]) -> Self {
        let mut perms = EndpointPermissions {
            read: false,
            write: false,
            frequency_write: false,
            ptt: false,
            config_write: false,
        };
        for token in tokens {
            match token.as_ref() {
                "read" => perms.read = true,
                "write" => perms.write = true,
                "frequency_write" => perms.frequency_write = true,
                "ptt" => perms.ptt = true,
                "config_write" => perms.config_write = true,
                _ => {}
            }
        }
        perms
    }

    /// Whether this endpoint is permitted to run a command of the given class.
    pub(crate) fn allows(self, class: CommandClass) -> bool {
        match class {
            CommandClass::ModeledRead | CommandClass::PassthroughRead => self.read,
            CommandClass::ModeledWrite => self.write,
            CommandClass::PttWrite => self.ptt,
            CommandClass::ConfigWrite => self.config_write,
            // Auto-info toggles are always allowed: they are virtualized per endpoint and
            // never touch the radio.
            CommandClass::AutoInfoToggle => true,
            CommandClass::Denied => false,
        }
    }

    /// Whether this endpoint may apply a specific modeled mutation.
    pub(crate) fn allows_mutation(self, class: CommandClass, mutation: &StateMutation) -> bool {
        if class != CommandClass::ModeledWrite {
            return self.allows(class);
        }
        self.write || (self.frequency_write && matches!(mutation, StateMutation::SetVfoFreq { .. }))
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    #[test]
    fn tokens_parse_into_flags() {
        let perms = EndpointPermissions::from_tokens(&["read", "write", "ptt"]);
        assert!(perms.read && perms.write && perms.ptt);
        assert!(!perms.frequency_write);
        assert!(!perms.config_write);
    }

    #[test]
    fn frequency_write_is_narrowly_scoped() {
        let perms = EndpointPermissions::from_tokens(&["read", "frequency_write"]);
        assert!(perms.frequency_write);
        assert!(!perms.write);
        assert!(perms.allows_mutation(
            CommandClass::ModeledWrite,
            &StateMutation::SetVfoFreq {
                vfo: crate::model::Vfo::A,
                hz: 14_074_000,
            }
        ));
        assert!(!perms.allows_mutation(
            CommandClass::ModeledWrite,
            &StateMutation::SetMode {
                vfo: crate::model::Vfo::A,
                mode: crate::model::Mode::Cw,
            }
        ));
    }

    #[test]
    fn read_only_denies_writes_and_ptt() {
        let perms = EndpointPermissions::read_only();
        assert!(perms.allows(CommandClass::ModeledRead));
        assert!(!perms.allows(CommandClass::ModeledWrite));
        assert!(!perms.allows(CommandClass::PttWrite));
        assert!(!perms.allows(CommandClass::ConfigWrite));
    }

    #[test]
    fn auto_info_toggle_always_allowed() {
        let perms = EndpointPermissions::read_only();
        assert!(perms.allows(CommandClass::AutoInfoToggle));
    }

    #[test]
    fn config_write_requires_flag() {
        let perms = EndpointPermissions::from_tokens(&["read", "write", "ptt", "config_write"]);
        assert!(perms.allows(CommandClass::ConfigWrite));
    }
}
