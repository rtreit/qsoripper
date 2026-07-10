//! Out-of-process `rigctld` bridge backend (breadth path, uncertified by default).
//!
//! The daemon is the *sole client* of a daemon-private `rigctld` and speaks the rigctld
//! net protocol as a client. It provides modeled control (frequency, mode, PTT, split)
//! across every rig Hamlib supports, using each rig's correct native model. It carries
//! **no** native passthrough (rigctld normalizes the CAT away) and reports
//! `native_command_family: None`, so an `EX`-menu passthrough fails closed (§7.1, §8.8).

use async_trait::async_trait;

use crate::backend::{
    BackendCapabilities, BackendError, Framing, RadioBackend, SplitStyle, TrustTier,
};
use crate::model::{Mode, PttSource, RadioEventSource, StateChange, StateMutation, Vfo};
use crate::radio::{Expect, RadioLink};
use crate::state::StateHandle;

/// Poll commands for the bridge: get-frequency, get-mode, get-split, get-ptt. All are
/// read-only and never retarget a VFO, but the bridge cannot *certify* the radio-side
/// wire, so it stays uncertified.
const POLL_GET_FREQ: &[u8] = b"f\n";
const POLL_GET_MODE: &[u8] = b"m\n";
const POLL_GET_SPLIT: &[u8] = b"s\n";

/// Out-of-process `rigctld` bridge.
#[derive(Clone)]
pub(crate) struct RigctldBackend {
    model: String,
    certified: bool,
}

impl RigctldBackend {
    /// Create a bridge backend. `certified` reflects whether the operator has run the
    /// §10.3 soak for this rigctld version+model+config; it is `false` by default.
    pub(crate) fn new(model: impl Into<String>, certified: bool) -> Self {
        RigctldBackend {
            model: model.into(),
            certified,
        }
    }
}

fn first_line(bytes: &[u8]) -> &[u8] {
    let end = bytes
        .iter()
        .position(|&b| b == b'\n')
        .unwrap_or(bytes.len());
    bytes.get(..end).unwrap_or(&[])
}

fn nth_line(bytes: &[u8], n: usize) -> &[u8] {
    bytes.split(|&b| b == b'\n').nth(n).unwrap_or(&[])
}

/// Check a `RPRT <code>` reply; non-zero is an error.
fn check_rprt(bytes: &[u8]) -> Result<(), BackendError> {
    let line = first_line(bytes);
    let text = std::str::from_utf8(line).unwrap_or("").trim();
    if let Some(code) = text.strip_prefix("RPRT ") {
        if code.trim() == "0" {
            Ok(())
        } else {
            Err(BackendError::Rejected(format!("rigctld {text}")))
        }
    } else {
        // Some setters reply with nothing meaningful; treat absence of error as success.
        Ok(())
    }
}

#[async_trait]
impl RadioBackend for RigctldBackend {
    async fn poll(&self, link: &RadioLink, state: &StateHandle) -> Result<(), BackendError> {
        let f = link
            .submit(POLL_GET_FREQ.to_vec(), Expect::Lines(1))
            .await?;
        if let Ok(hz) = std::str::from_utf8(first_line(&f))
            .unwrap_or("")
            .trim()
            .parse::<u64>()
        {
            state.record(
                StateChange::Freq { vfo: Vfo::A, hz },
                RadioEventSource::PollDiff,
            );
        }
        let m = link
            .submit(POLL_GET_MODE.to_vec(), Expect::Lines(2))
            .await?;
        let mode = Mode::from_hamlib_token(std::str::from_utf8(first_line(&m)).unwrap_or(""));
        state.record(
            StateChange::Mode { vfo: Vfo::A, mode },
            RadioEventSource::PollDiff,
        );
        let s = link
            .submit(POLL_GET_SPLIT.to_vec(), Expect::Lines(2))
            .await?;
        let enabled = std::str::from_utf8(first_line(&s)).unwrap_or("").trim() == "1";
        let tx_vfo = if std::str::from_utf8(nth_line(&s, 1))
            .unwrap_or("")
            .trim()
            .eq_ignore_ascii_case("VFOB")
        {
            Vfo::B
        } else {
            Vfo::A
        };
        state.record(
            StateChange::Split {
                enabled,
                tx_vfo: Some(tx_vfo),
            },
            RadioEventSource::PollDiff,
        );
        Ok(())
    }

    async fn apply(
        &self,
        mutation: StateMutation,
        link: &RadioLink,
        state: &StateHandle,
    ) -> Result<(), BackendError> {
        match mutation {
            StateMutation::SetVfoFreq { hz, .. } => {
                let reply = link
                    .submit(format!("F {hz}\n").into_bytes(), Expect::Lines(1))
                    .await?;
                check_rprt(&reply)?;
            }
            StateMutation::SetMode { mode, .. } => {
                let reply = link
                    .submit(
                        format!("M {} 0\n", mode.hamlib_token()).into_bytes(),
                        Expect::Lines(1),
                    )
                    .await?;
                check_rprt(&reply)?;
            }
            // Compose the DATA flag back onto the current base mode and re-assert the mode
            // downstream (the bridge target speaks combined Hamlib mode tokens, not a
            // separate DATA command).
            StateMutation::SetDataMode { vfo, on } => {
                let base = state.snapshot().vfo(vfo).mode;
                let reply = link
                    .submit(
                        format!("M {} 0\n", base.hamlib_token_with_data(on)).into_bytes(),
                        Expect::Lines(1),
                    )
                    .await?;
                check_rprt(&reply)?;
            }
            StateMutation::SetSplit { enabled, tx_vfo } => {
                let vfo = match tx_vfo.unwrap_or(Vfo::B) {
                    Vfo::A => "VFOA",
                    Vfo::B => "VFOB",
                };
                let on = u8::from(enabled);
                let reply = link
                    .submit(format!("S {on} {vfo}\n").into_bytes(), Expect::Lines(1))
                    .await?;
                check_rprt(&reply)?;
            }
            StateMutation::SetPtt { keyed, source } => {
                let on = if keyed {
                    match source {
                        PttSource::Generic => 1,
                        PttSource::Mic => 2,
                        PttSource::Data => 3,
                    }
                } else {
                    0
                };
                let reply = link
                    .submit(format!("T {on}\n").into_bytes(), Expect::Lines(1))
                    .await?;
                check_rprt(&reply)?;
            }
            StateMutation::SetRxVfo { .. }
            | StateMutation::SetRit { .. }
            | StateMutation::SetXit { .. } => {
                return Err(BackendError::Unsupported);
            }
        }
        state.record(mutation.into_change(), RadioEventSource::OptimisticWrite);
        Ok(())
    }

    fn parse_event(&self, _frame: &[u8]) -> Option<StateMutation> {
        // The bridge has no native push; downstream uniformity comes from poll-diff.
        None
    }

    async fn passthrough(&self, _raw: &[u8], _link: &RadioLink) -> Result<Vec<u8>, BackendError> {
        // Fails closed: the bridge normalizes native CAT away (§7.1).
        Err(BackendError::Unsupported)
    }

    fn capabilities(&self) -> BackendCapabilities {
        BackendCapabilities {
            model: format!("rigctld:{}", self.model),
            vfo_count: 2,
            has_rit: false,
            has_xit: false,
            has_smeter: false,
            split: SplitStyle::VfoPair,
            native_push: false,
            native_command_family: None,
            framing: Framing::LineTerminated,
            freq_min_hz: 30_000,
            freq_max_hz: 60_000_000,
            trust: if self.certified {
                TrustTier::CertifiedNative
            } else {
                TrustTier::UncertifiedBridge
            },
        }
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    #[test]
    fn bridge_is_uncertified_by_default() {
        let backend = RigctldBackend::new("TS-590SG", false);
        assert_eq!(backend.capabilities().trust, TrustTier::UncertifiedBridge);
        assert!(backend.capabilities().native_command_family.is_none());
        assert!(!backend.capabilities().supports_passthrough());
    }

    #[tokio::test]
    async fn passthrough_fails_closed() {
        let backend = RigctldBackend::new("IC-7300", false);
        let link = crate::radio::detached_link();
        let result = backend.passthrough(b"EX0050000;", &link).await;
        assert!(matches!(result, Err(BackendError::Unsupported)));
    }

    #[test]
    fn rprt_zero_is_ok_nonzero_is_error() {
        assert!(check_rprt(b"RPRT 0\n").is_ok());
        assert!(check_rprt(b"RPRT -11\n").is_err());
        assert!(check_rprt(b"7030000\n").is_ok());
    }

    #[test]
    fn parses_lines() {
        assert_eq!(first_line(b"USB\n2400\n"), b"USB");
        assert_eq!(nth_line(b"1\nVFOB\n", 1), b"VFOB");
    }
}
