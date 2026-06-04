//! The native Kenwood TS-590 client dialect (N1MM Logger+, ARCP-590, Log4OM-as-TS590).
//!
//! Modeled reads (`FA`/`FB`/`MD`/`IF`) are served from the universal state cache, so a
//! flurry of client polls never touches the radio. Modeled writes route through
//! [`FaceContext::apply_modeled`] for serialization, permission checks, and the PTT lease.
//! `AI` is virtualized per face. Any unmodeled native command falls through to a
//! permission-gated passthrough so genuine native features still work on a certified rig.

use async_trait::async_trait;

use super::{ai_frame, freq_frame, mode_from_digit, mode_to_digit, parse_command, ERR};
use crate::dialect::{ApplyOutcome, ClientDialect, FaceContext};
use crate::model::{PttSource, StateChange, StateMutation, Vfo};
use crate::permissions::CommandClass;
use crate::state::Snapshot;

/// The native TS-590 dialect.
#[derive(Clone, Default)]
pub(crate) struct Ts590Dialect;

impl Ts590Dialect {
    /// Create the dialect.
    pub(crate) fn new() -> Self {
        Ts590Dialect
    }
}

/// Synthesize a Kenwood `IF;` status answer (38 bytes) from the universal state.
///
/// When `single_vfo` is true (operating-VFO virtualization) the operating VFO is always
/// presented as VFO A with no split, so a single-VFO logger (N1MM SO1V) never sees VFO B
/// in the P10 field and never warns about it.
fn synth_if(snapshot: &Snapshot, single_vfo: bool) -> Vec<u8> {
    let freq = snapshot.vfo(snapshot.rx_vfo).freq_hz;
    let tx = u8::from(snapshot.ptt);
    let mode = mode_to_digit(snapshot.vfo(snapshot.rx_vfo).mode) - b'0';
    let split = if single_vfo {
        0
    } else {
        u8::from(snapshot.split)
    };
    let rx_vfo = if single_vfo {
        0u8
    } else {
        match snapshot.rx_vfo {
            Vfo::A => 0u8,
            Vfo::B => 1u8,
        }
    };
    format!("IF{freq:011}0000+0000000000{tx}{mode}{rx_vfo}0{split}0000;").into_bytes()
}

#[async_trait]
impl ClientDialect for Ts590Dialect {
    async fn handle(&self, request: &[u8], ctx: &FaceContext) -> Vec<u8> {
        let Some((verb, payload)) = parse_command(request) else {
            return ERR.to_vec();
        };
        let read = payload.is_empty();
        // In operating-VFO virtualization, every VFO-addressed read/write targets the
        // operating (rx) VFO so the face only ever sees a single VFO presented as VFO A.
        let single_vfo = ctx.single_vfo();
        let op_vfo = ctx.snapshot().rx_vfo;
        match verb.as_slice() {
            b"FA" => {
                let target = if single_vfo { op_vfo } else { Vfo::A };
                if read {
                    freq_frame(b"FA", ctx.snapshot().vfo(target).freq_hz)
                } else {
                    set_freq(ctx, target, &payload).await
                }
            }
            b"FB" => {
                // For a single-VFO face the "other" VFO is hidden: FB mirrors the operating
                // VFO so no path exposes physical VFO B. Dual-VFO faces address real B.
                let target = if single_vfo { op_vfo } else { Vfo::B };
                if read {
                    freq_frame(b"FB", ctx.snapshot().vfo(target).freq_hz)
                } else {
                    set_freq(ctx, target, &payload).await
                }
            }
            // A single-VFO face must never see the receive/transmit VFO selectors expose
            // VFO B (they would otherwise fall through to a raw passthrough). Present the
            // operating VFO as VFO A and swallow attempts to select a VFO.
            b"FR" if single_vfo => {
                if read {
                    b"FR0;".to_vec()
                } else {
                    Vec::new()
                }
            }
            b"FT" if single_vfo => {
                if read {
                    b"FT0;".to_vec()
                } else {
                    Vec::new()
                }
            }
            b"MD" => {
                if read {
                    // A real TS-590 `MD;` reports the *active* VFO's mode. Reading VFO A
                    // unconditionally froze N1MM/Log4OM on VFO B's mode display.
                    let snap = ctx.snapshot();
                    let d = mode_to_digit(snap.vfo(snap.rx_vfo).mode);
                    vec![b'M', b'D', d, b';']
                } else {
                    let Some(&d) = payload.first() else {
                        return ERR.to_vec();
                    };
                    // `MD<n>;` writes the *active* VFO's mode, matching real-radio semantics.
                    let vfo = ctx.snapshot().rx_vfo;
                    reply(
                        ctx.apply_modeled(
                            StateMutation::SetMode {
                                vfo,
                                mode: mode_from_digit(d),
                            },
                            CommandClass::ModeledWrite,
                        )
                        .await,
                    )
                }
            }
            b"TX" => reply(
                ctx.apply_modeled(
                    StateMutation::SetPtt {
                        keyed: true,
                        source: PttSource::Generic,
                    },
                    CommandClass::PttWrite,
                )
                .await,
            ),
            b"RX" => reply(
                ctx.apply_modeled(
                    StateMutation::SetPtt {
                        keyed: false,
                        source: PttSource::Generic,
                    },
                    CommandClass::PttWrite,
                )
                .await,
            ),
            b"AI" => {
                // `AI;` is a read: report the current virtualized auto-info state without
                // changing it, so a native client's connection handshake completes. Only an
                // `AI<n>;` write toggles auto-information for this face.
                if read {
                    ai_frame(ctx.ai_on())
                } else {
                    let on = payload.first().is_some_and(|&d| d != b'0');
                    ctx.set_ai(on);
                    Vec::new()
                }
            }
            b"ID" if read => b"ID021;".to_vec(),
            b"PS" if read => b"PS1;".to_vec(),
            b"IF" if read => synth_if(&ctx.snapshot(), single_vfo),
            // Any other native command is forwarded as a permission-gated passthrough.
            _ => {
                let class = if read {
                    CommandClass::PassthroughRead
                } else {
                    CommandClass::ConfigWrite
                };
                ctx.passthrough(request, class).await
            }
        }
    }

    fn format_notification(&self, change: &StateChange, ctx: &FaceContext) -> Option<Vec<u8>> {
        if !ctx.ai_on() {
            return None;
        }
        let snap = ctx.snapshot();
        if ctx.single_vfo() {
            return single_vfo_notification(change, &snap);
        }
        match *change {
            StateChange::Freq { vfo, hz } => {
                // Always emit the explicit per-VFO frame for VFO-specific trackers.
                let label: &[u8] = if vfo == Vfo::A { b"FA" } else { b"FB" };
                let mut frame = freq_frame(label, hz);
                // When the change is on the *active* VFO, also emit the operating-status
                // `IF` frame. Operating-frequency trackers (N1MM Logger+, Log4OM-as-TS590)
                // read the displayed frequency from `IF;`/`FA`, not from a bare `FB`, so an
                // FB-only push made frequency updates silently stop whenever the rig was on
                // VFO B. Appending `IF` makes VFO B behave exactly like VFO A.
                if vfo == snap.rx_vfo {
                    frame.extend_from_slice(&synth_if(&snap, false));
                }
                Some(frame)
            }
            StateChange::Mode { vfo, mode } if vfo == snap.rx_vfo => {
                // `MD` reflects the active VFO on a real TS-590; emit it plus the operating
                // `IF` so mode trackers follow the active VFO regardless of A/B.
                let mut frame = vec![b'M', b'D', mode_to_digit(mode), b';'];
                frame.extend_from_slice(&synth_if(&snap, false));
                Some(frame)
            }
            StateChange::RxVfo { .. } => Some(synth_if(&snap, false)),
            _ => None,
        }
    }

    fn format_passthrough(&self, raw: &[u8], ctx: &FaceContext) -> Option<Vec<u8>> {
        // A single-VFO face sees a curated, virtualized view: never relay the radio's raw
        // CAT stream, which can carry FA/FB/FR/IF frames that would leak physical VFO B and
        // break the "operating VFO is always VFO A" illusion.
        if ctx.single_vfo() {
            return None;
        }
        // A certified-native client that enabled auto-information expects the radio's CAT
        // stream verbatim. Relaying unmodeled frames (NB/NR/AG/front-panel changes, ...)
        // keeps its client-side feature state machines in sync; without this, a client like
        // ARCP-590 never sees the echo of its own NB write and cannot advance the NB cycle.
        if ctx.ai_on() {
            Some(raw.to_vec())
        } else {
            None
        }
    }
}

/// Build the operating-VFO-virtualized notification: the operating VFO is always presented
/// as VFO A, inactive-VFO churn is suppressed, and an A/B switch re-presents the new
/// operating VFO as VFO A (FA+MD+IF) so a single-VFO logger seamlessly retunes.
fn single_vfo_notification(change: &StateChange, snap: &Snapshot) -> Option<Vec<u8>> {
    match *change {
        StateChange::Freq { vfo, hz } if vfo == snap.rx_vfo => {
            let mut frame = freq_frame(b"FA", hz);
            frame.extend_from_slice(&synth_if(snap, true));
            Some(frame)
        }
        StateChange::Mode { vfo, mode } if vfo == snap.rx_vfo => {
            let mut frame = vec![b'M', b'D', mode_to_digit(mode), b';'];
            frame.extend_from_slice(&synth_if(snap, true));
            Some(frame)
        }
        StateChange::RxVfo { .. } => {
            // The operator pressed A/B: re-present the new operating VFO as VFO A so the
            // logger retunes exactly as if VFO A had jumped to the new frequency and mode.
            let op = snap.vfo(snap.rx_vfo);
            let mut frame = freq_frame(b"FA", op.freq_hz);
            frame.extend_from_slice(&[b'M', b'D', mode_to_digit(op.mode), b';']);
            frame.extend_from_slice(&synth_if(snap, true));
            Some(frame)
        }
        // Inactive-VFO frequency/mode churn is invisible to a single-VFO face.
        _ => None,
    }
}

async fn set_freq(ctx: &FaceContext, vfo: Vfo, payload: &[u8]) -> Vec<u8> {
    let Ok(hz) = std::str::from_utf8(payload)
        .unwrap_or("")
        .trim()
        .parse::<u64>()
    else {
        return ERR.to_vec();
    };
    reply(
        ctx.apply_modeled(
            StateMutation::SetVfoFreq { vfo, hz },
            CommandClass::ModeledWrite,
        )
        .await,
    )
}

fn reply(outcome: ApplyOutcome) -> Vec<u8> {
    match outcome {
        ApplyOutcome::Ok => Vec::new(),
        _ => ERR.to_vec(),
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;
    use crate::backend::loopback::LoopbackBackend;
    use crate::backend::RadioBackend;
    use crate::model::{Mode, RadioEventSource};
    use crate::permissions::FacePermissions;
    use crate::ptt::PttManager;
    use crate::radio::{detached_link, spawn_scheduler};
    use crate::state::StateHandle;
    use std::sync::Arc;
    use std::time::Duration;

    fn ctx_with(perms: FacePermissions) -> (FaceContext, LoopbackBackend) {
        let backend = LoopbackBackend::new();
        let caps = backend.capabilities();
        let arc: Arc<dyn RadioBackend> = Arc::new(backend.clone());
        let state = StateHandle::new();
        let radio = spawn_scheduler(arc, detached_link(), state.clone());
        let ptt = PttManager::new(Duration::from_secs(300));
        (FaceContext::new(5, perms, state, radio, ptt, caps), backend)
    }

    /// Like [`ctx_with`] but the face uses operating-VFO virtualization (single-VFO
    /// presentation), as configured for N1MM SO1V.
    fn single_vfo_ctx(perms: FacePermissions) -> (FaceContext, LoopbackBackend) {
        let (ctx, backend) = ctx_with(perms);
        (ctx.with_single_vfo(true), backend)
    }

    #[tokio::test]
    async fn reads_frequency_from_cache() {
        let (ctx, _b) = ctx_with(FacePermissions::read_only());
        ctx.state.record(
            StateChange::Freq {
                vfo: Vfo::A,
                hz: 7_074_000,
            },
            RadioEventSource::PollDiff,
        );
        assert_eq!(
            Ts590Dialect::new().handle(b"FA;", &ctx).await,
            b"FA00007074000;".to_vec()
        );
    }

    #[tokio::test]
    async fn write_is_denied_for_read_only_face() {
        let (ctx, backend) = ctx_with(FacePermissions::read_only());
        assert_eq!(
            Ts590Dialect::new().handle(b"FA00007050000;", &ctx).await,
            ERR.to_vec()
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(backend.mutations().is_empty());
    }

    #[tokio::test]
    async fn mode_read_and_write() {
        let (ctx, backend) = ctx_with(FacePermissions::from_tokens(&["read", "write"]));
        assert_eq!(
            Ts590Dialect::new().handle(b"MD;", &ctx).await,
            b"MD2;".to_vec(),
            "default mode is USB digit 2"
        );
        assert_eq!(
            Ts590Dialect::new().handle(b"MD3;", &ctx).await,
            Vec::<u8>::new()
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert_eq!(
            backend.mutations(),
            vec![StateMutation::SetMode {
                vfo: Vfo::A,
                mode: Mode::Cw
            }]
        );
    }

    #[tokio::test]
    async fn ai_read_reports_state_without_changing_it() {
        let (ctx, _b) = ctx_with(FacePermissions::read_only());
        // A fresh face: auto-info off, and reading it must not enable it.
        assert_eq!(
            Ts590Dialect::new().handle(b"AI;", &ctx).await,
            b"AI0;".to_vec()
        );
        assert!(!ctx.ai_on(), "AI; read must not change the flag");

        // After enabling, a read reports 2 and leaves it enabled (regression: a read used
        // to be parsed as a write with an empty payload and silently disabled auto-info,
        // which froze native clients like ARCP-590 that poll AI; as a keepalive).
        assert!(Ts590Dialect::new().handle(b"AI2;", &ctx).await.is_empty());
        assert!(ctx.ai_on());
        assert_eq!(
            Ts590Dialect::new().handle(b"AI;", &ctx).await,
            b"AI2;".to_vec()
        );
        assert!(ctx.ai_on(), "AI; read must not disable auto-info");
    }

    #[tokio::test]
    async fn ai_toggle_is_virtualized_and_drives_notifications() {
        let (ctx, _b) = ctx_with(FacePermissions::read_only());
        assert!(Ts590Dialect::new().handle(b"AI2;", &ctx).await.is_empty());
        assert!(ctx.ai_on());
        let note = Ts590Dialect::new().format_notification(
            &StateChange::Freq {
                vfo: Vfo::A,
                hz: 14_074_000,
            },
            &ctx,
        );
        // The active VFO is A by default, so an active-VFO frequency change must push the
        // per-VFO `FA` frame *and* the operating-status `IF` frame so clients that track the
        // operating frequency (N1MM, Log4OM-as-TS590) follow the change.
        let mut expected = b"FA00014074000;".to_vec();
        expected.extend_from_slice(&synth_if(&ctx.snapshot(), false));
        assert_eq!(note, Some(expected));
    }

    /// Regression repro for the N1MM "frequency stops tracking on VFO B" bug.
    ///
    /// On VFO A, a frequency change pushed an `FA` frame and N1MM updated. On VFO B the hub
    /// pushed only a bare `FB` frame, which operating-frequency trackers ignore, so the
    /// displayed frequency silently froze. The active-VFO change must also push the
    /// operating-status `IF` frame regardless of which VFO is active.
    #[tokio::test]
    async fn notification_for_active_vfo_b_freq_pushes_operating_if() {
        let (ctx, _b) = ctx_with(FacePermissions::read_only());
        ctx.state.record(
            StateChange::RxVfo { vfo: Vfo::B },
            RadioEventSource::PollDiff,
        );
        ctx.state.record(
            StateChange::Freq {
                vfo: Vfo::B,
                hz: 14_034_320,
            },
            RadioEventSource::PollDiff,
        );
        ctx.set_ai(true);
        let note = Ts590Dialect::new()
            .format_notification(
                &StateChange::Freq {
                    vfo: Vfo::B,
                    hz: 14_034_320,
                },
                &ctx,
            )
            .expect("active-VFO freq change must notify");
        let synth = synth_if(&ctx.snapshot(), false);
        assert!(
            note.windows(synth.len()).any(|w| w == synth.as_slice()),
            "VFO B active-freq notification must include the operating IF frame; got {:?}",
            String::from_utf8_lossy(&note)
        );
        // The IF frame must report VFO B (rx_vfo field == '1') and B's frequency.
        assert!(
            note.windows(2).any(|w| w == b"IF"),
            "notification must contain an IF status frame"
        );
        assert!(
            note.windows(b"FB00014034320;".len())
                .any(|w| w == b"FB00014034320;"),
            "notification must still include the per-VFO FB frame"
        );
    }

    #[tokio::test]
    async fn mode_read_follows_active_vfo_b() {
        let (ctx, _b) = ctx_with(FacePermissions::read_only());
        ctx.state.record(
            StateChange::RxVfo { vfo: Vfo::B },
            RadioEventSource::PollDiff,
        );
        ctx.state.record(
            StateChange::Mode {
                vfo: Vfo::B,
                mode: Mode::Cw,
            },
            RadioEventSource::PollDiff,
        );
        // A real TS-590 `MD;` reports the *active* VFO's mode, so with VFO B active and set
        // to CW the read must return the CW digit (3), not VFO A's default USB (2).
        assert_eq!(
            Ts590Dialect::new().handle(b"MD;", &ctx).await,
            b"MD3;".to_vec()
        );
    }

    #[tokio::test]
    async fn mode_write_targets_active_vfo_b() {
        let (ctx, backend) = ctx_with(FacePermissions::from_tokens(&["read", "write"]));
        ctx.state.record(
            StateChange::RxVfo { vfo: Vfo::B },
            RadioEventSource::PollDiff,
        );
        assert_eq!(
            Ts590Dialect::new().handle(b"MD3;", &ctx).await,
            Vec::<u8>::new()
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
        // The write must target the active VFO (B), not be hardcoded to VFO A.
        assert_eq!(
            backend.mutations(),
            vec![StateMutation::SetMode {
                vfo: Vfo::B,
                mode: Mode::Cw
            }]
        );
    }

    #[tokio::test]
    async fn passthrough_frame_is_relayed_only_when_auto_info_on() {
        let (ctx, _b) = ctx_with(FacePermissions::read_only());
        // Auto-info off: the radio's unmodeled echo is suppressed for this face.
        assert_eq!(Ts590Dialect::new().format_passthrough(b"NB1;", &ctx), None);
        // After the client enables auto-info, the echo is relayed verbatim so its NB cycle
        // (and front-panel changes) stay in sync.
        ctx.set_ai(true);
        assert_eq!(
            Ts590Dialect::new().format_passthrough(b"NB1;", &ctx),
            Some(b"NB1;".to_vec())
        );
        assert_eq!(
            Ts590Dialect::new().format_passthrough(b"NB0;", &ctx),
            Some(b"NB0;".to_vec())
        );
    }

    #[tokio::test]
    async fn id_and_ps_identify_as_ts590() {
        let (ctx, _b) = ctx_with(FacePermissions::read_only());
        assert_eq!(
            Ts590Dialect::new().handle(b"ID;", &ctx).await,
            b"ID021;".to_vec()
        );
        assert_eq!(
            Ts590Dialect::new().handle(b"PS;", &ctx).await,
            b"PS1;".to_vec()
        );
    }

    #[tokio::test]
    async fn if_answer_is_38_bytes() {
        let (ctx, _b) = ctx_with(FacePermissions::read_only());
        let reply = Ts590Dialect::new().handle(b"IF;", &ctx).await;
        assert_eq!(reply.len(), 38);
        assert!(reply.starts_with(b"IF"));
    }

    #[tokio::test]
    async fn unmodeled_set_requires_config_write() {
        let (ctx, backend) = ctx_with(FacePermissions::from_tokens(&["read", "write"]));
        // No config_write permission: an EX-menu set is refused, never forwarded.
        assert_eq!(
            Ts590Dialect::new().handle(b"EX0050000;", &ctx).await,
            ERR.to_vec()
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(backend.passthroughs().is_empty());
    }

    #[tokio::test]
    async fn passthrough_read_is_forwarded_when_permitted() {
        let (ctx, backend) = ctx_with(FacePermissions::from_tokens(&["read"]));
        let reply = Ts590Dialect::new().handle(b"RM;", &ctx).await;
        // The loopback backend echoes the raw passthrough.
        assert_eq!(reply, b"RM;".to_vec());
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert_eq!(backend.passthroughs(), vec![b"RM;".to_vec()]);
    }

    // --- Operating-VFO virtualization (single_vfo) -----------------------------------

    /// With the rig on VFO B, a single-VFO face must see the operating frequency presented
    /// as VFO A: `IF;` reports P10 == '0' (not '1') so N1MM SO1V never warns, and the
    /// frequency is VFO B's operating frequency.
    #[tokio::test]
    async fn single_vfo_if_presents_operating_vfo_b_as_vfo_a() {
        let (ctx, _b) = single_vfo_ctx(FacePermissions::read_only());
        ctx.state.record(
            StateChange::RxVfo { vfo: Vfo::B },
            RadioEventSource::PollDiff,
        );
        ctx.state.record(
            StateChange::Freq {
                vfo: Vfo::B,
                hz: 14_034_320,
            },
            RadioEventSource::PollDiff,
        );
        let reply = Ts590Dialect::new().handle(b"IF;", &ctx).await;
        assert_eq!(reply.len(), 38);
        // P10 (the active-VFO digit) is at index 30 in the 38-byte IF answer.
        assert_eq!(reply[30], b'0', "operating VFO must be presented as VFO A");
        assert!(
            reply.windows(11).any(|w| w == b"00014034320"),
            "IF must carry the operating (VFO B) frequency; got {}",
            String::from_utf8_lossy(&reply)
        );
    }

    /// A single-VFO face reads the operating VFO's frequency via `FA;`, even when the rig
    /// is physically on VFO B.
    #[tokio::test]
    async fn single_vfo_fa_read_returns_operating_vfo_b_freq() {
        let (ctx, _b) = single_vfo_ctx(FacePermissions::read_only());
        ctx.state.record(
            StateChange::RxVfo { vfo: Vfo::B },
            RadioEventSource::PollDiff,
        );
        ctx.state.record(
            StateChange::Freq {
                vfo: Vfo::B,
                hz: 14_034_320,
            },
            RadioEventSource::PollDiff,
        );
        assert_eq!(
            Ts590Dialect::new().handle(b"FA;", &ctx).await,
            b"FA00014034320;".to_vec()
        );
    }

    /// An `FA` write from a single-VFO face must tune the operating VFO (B), not VFO A.
    #[tokio::test]
    async fn single_vfo_fa_write_tunes_operating_vfo_b() {
        let (ctx, backend) = single_vfo_ctx(FacePermissions::from_tokens(&["read", "write"]));
        ctx.state.record(
            StateChange::RxVfo { vfo: Vfo::B },
            RadioEventSource::PollDiff,
        );
        assert_eq!(
            Ts590Dialect::new().handle(b"FA00014050000;", &ctx).await,
            Vec::<u8>::new()
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert_eq!(
            backend.mutations(),
            vec![StateMutation::SetVfoFreq {
                vfo: Vfo::B,
                hz: 14_050_000
            }]
        );
    }

    /// The receive-VFO selector never exposes VFO B to a single-VFO face: `FR;` reports
    /// VFO A and a `FR1;` select is swallowed (not forwarded to the radio).
    #[tokio::test]
    async fn single_vfo_fr_is_virtualized_to_vfo_a() {
        let (ctx, backend) = single_vfo_ctx(FacePermissions::from_tokens(&["read", "write"]));
        ctx.state.record(
            StateChange::RxVfo { vfo: Vfo::B },
            RadioEventSource::PollDiff,
        );
        assert_eq!(
            Ts590Dialect::new().handle(b"FR;", &ctx).await,
            b"FR0;".to_vec()
        );
        assert_eq!(
            Ts590Dialect::new().handle(b"FR1;", &ctx).await,
            Vec::<u8>::new()
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(
            backend.passthroughs().is_empty(),
            "FR must never reach the radio as a passthrough on a single-VFO face"
        );
    }

    /// Switching the operating VFO (A->B) must re-present the new operating VFO as VFO A:
    /// the push carries `FA`(new freq) + `MD`(new mode) + an `IF` with P10 == '0', so a
    /// single-VFO logger seamlessly retunes with no SO1V warning.
    #[tokio::test]
    async fn single_vfo_rxvfo_switch_re_presents_operating_as_vfo_a() {
        let (ctx, _b) = single_vfo_ctx(FacePermissions::read_only());
        ctx.state.record(
            StateChange::Freq {
                vfo: Vfo::B,
                hz: 21_205_000,
            },
            RadioEventSource::PollDiff,
        );
        ctx.state.record(
            StateChange::Mode {
                vfo: Vfo::B,
                mode: Mode::Cw,
            },
            RadioEventSource::PollDiff,
        );
        ctx.state.record(
            StateChange::RxVfo { vfo: Vfo::B },
            RadioEventSource::PollDiff,
        );
        ctx.set_ai(true);
        let note = Ts590Dialect::new()
            .format_notification(&StateChange::RxVfo { vfo: Vfo::B }, &ctx)
            .expect("A/B switch must notify a single-VFO face");
        assert!(
            note.windows(b"FA00021205000;".len())
                .any(|w| w == b"FA00021205000;"),
            "switch must push the new operating frequency as FA; got {}",
            String::from_utf8_lossy(&note)
        );
        assert!(
            note.windows(4).any(|w| w == b"MD3;"),
            "switch must push the new operating mode (CW=3); got {}",
            String::from_utf8_lossy(&note)
        );
        // The trailing IF must present VFO A (P10 == '0'), never VFO B.
        let if_pos = note
            .windows(2)
            .position(|w| w == b"IF")
            .expect("notification must contain an IF frame");
        assert_eq!(
            note[if_pos + 30],
            b'0',
            "the operating VFO must be presented as VFO A in the IF frame"
        );
    }

    /// An active-VFO (B) frequency change pushes `FA` (not `FB`) plus an `IF` presenting
    /// VFO A, so N1MM SO1V tracks the change. Inactive-VFO churn is suppressed.
    #[tokio::test]
    async fn single_vfo_active_freq_change_pushes_fa_as_vfo_a() {
        let (ctx, _b) = single_vfo_ctx(FacePermissions::read_only());
        ctx.state.record(
            StateChange::RxVfo { vfo: Vfo::B },
            RadioEventSource::PollDiff,
        );
        ctx.state.record(
            StateChange::Freq {
                vfo: Vfo::B,
                hz: 14_034_320,
            },
            RadioEventSource::PollDiff,
        );
        ctx.set_ai(true);
        let note = Ts590Dialect::new()
            .format_notification(
                &StateChange::Freq {
                    vfo: Vfo::B,
                    hz: 14_034_320,
                },
                &ctx,
            )
            .expect("active-VFO freq change must notify");
        assert!(
            note.starts_with(b"FA00014034320;"),
            "active VFO-B change must be presented as an FA frame; got {}",
            String::from_utf8_lossy(&note)
        );
        assert!(
            !note.windows(2).any(|w| w == b"FB"),
            "a single-VFO face must never receive an FB frame; got {}",
            String::from_utf8_lossy(&note)
        );
        let if_pos = note.windows(2).position(|w| w == b"IF").expect("IF frame");
        assert_eq!(note[if_pos + 30], b'0', "IF must present VFO A");
    }

    /// A frequency change on the *inactive* VFO is invisible to a single-VFO face.
    #[tokio::test]
    async fn single_vfo_inactive_freq_change_is_suppressed() {
        let (ctx, _b) = single_vfo_ctx(FacePermissions::read_only());
        // Operating on VFO A; a change on VFO B must not be pushed.
        ctx.set_ai(true);
        let note = Ts590Dialect::new().format_notification(
            &StateChange::Freq {
                vfo: Vfo::B,
                hz: 7_000_000,
            },
            &ctx,
        );
        assert_eq!(note, None);
    }

    /// A single-VFO face never receives raw passthrough frames (which could leak VFO B).
    #[tokio::test]
    async fn single_vfo_suppresses_raw_passthrough() {
        let (ctx, _b) = single_vfo_ctx(FacePermissions::read_only());
        ctx.set_ai(true);
        assert_eq!(
            Ts590Dialect::new().format_passthrough(b"FB00014000000;", &ctx),
            None
        );
    }
}
