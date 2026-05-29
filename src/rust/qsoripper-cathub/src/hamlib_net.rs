//! Hamlib `rigctld`-compatible TCP server face (design §6/§8, validated against golden
//! transcripts captured from a real `rigctld`, §10.1).
//!
//! This is a thin server-side reimplementation of the rigctld net protocol — it never
//! links Hamlib (§8.8). It serves the QsoRipper engine (read-only endpoint) and WSJT-X
//! (write/PTT endpoint). Modeled reads come from the universal state; writes go through
//! [`FaceContext::apply_modeled`] so they participate in serialization, the PTT lease, and
//! event fan-out. Set commands never emit a VFO-target write (frequency always lands on
//! the active VFO), preserving the no-VFO-retargeting invariant.

use std::sync::Arc;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;

use crate::dialect::{ApplyOutcome, FaceContext};
use crate::model::{Mode, PttSource, StateMutation, Vfo};
use crate::permissions::CommandClass;

/// The capability dump served for `\dump_state`. Captured verbatim from Hamlib 4.7.0
/// `rigctld` (protocol version 1) so the WSJT-X / engine Hamlib clients parse it exactly.
const DUMP_STATE: &str = include_str!("hamlib_dump_state.txt");

const RPRT_OK: &[u8] = b"RPRT 0\n";
/// Generic invalid / not-permitted error.
const RPRT_EINVAL: &[u8] = b"RPRT -1\n";
/// Feature not available on this backend.
const RPRT_ENAVAIL: &[u8] = b"RPRT -11\n";

fn vfo_name(vfo: Vfo) -> &'static str {
    match vfo {
        Vfo::A => "VFOA",
        Vfo::B => "VFOB",
    }
}

/// Parse a `set_freq` argument into whole Hz.
///
/// Hamlib clients (WSJT-X, the engine, Log4OM) format frequencies as a double with
/// `"%f"`, e.g. `F 14040005.000000`, so a plain `u64` parse rejects every real
/// `set_freq` with `RPRT -1`. Accept either a plain integer or a decimal value and
/// keep the integer Hz part (frequencies are always whole Hz, so the fraction is `0`).
fn parse_freq_hz(arg: &str) -> Option<u64> {
    arg.parse::<u64>().ok().or_else(|| {
        arg.split_once('.')
            .and_then(|(whole, _frac)| whole.parse::<u64>().ok())
    })
}

fn outcome_rprt(outcome: ApplyOutcome) -> Vec<u8> {
    match outcome {
        ApplyOutcome::Ok => RPRT_OK.to_vec(),
        _ => RPRT_EINVAL.to_vec(),
    }
}

/// The result of handling one protocol line.
enum LineResult {
    /// Reply bytes to send.
    Reply(Vec<u8>),
    /// The client asked to quit; close the connection.
    Quit,
}

/// Handle one rigctld protocol line against the universal state.
#[allow(clippy::too_many_lines)] // A flat protocol dispatch table reads best as one match.
async fn handle_line(line: &str, ctx: &FaceContext) -> LineResult {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return LineResult::Reply(Vec::new());
    }
    let mut parts = trimmed.split_whitespace();
    let Some(cmd) = parts.next() else {
        return LineResult::Reply(Vec::new());
    };
    let snapshot = ctx.snapshot();

    let reply: Vec<u8> = match cmd {
        "q" | "Q" => return LineResult::Quit,

        // --- reads (require `read`) ---
        "f" | "\\get_freq" => guard_read(ctx, || {
            format!("{}\n", snapshot.vfo(snapshot.rx_vfo).freq_hz).into_bytes()
        }),
        "m" | "\\get_mode" => guard_read(ctx, || {
            let v = snapshot.vfo(snapshot.rx_vfo);
            format!("{}\n{}\n", v.mode.hamlib_token(), v.passband_hz).into_bytes()
        }),
        "v" | "\\get_vfo" => guard_read(ctx, || {
            format!("{}\n", vfo_name(snapshot.rx_vfo)).into_bytes()
        }),
        "s" | "\\get_split_vfo" => guard_read(ctx, || {
            let tx = if snapshot.split {
                snapshot.tx_vfo
            } else {
                snapshot.rx_vfo
            };
            format!("{}\n{}\n", u8::from(snapshot.split), vfo_name(tx)).into_bytes()
        }),
        "t" | "\\get_ptt" => {
            guard_read(ctx, || format!("{}\n", u8::from(snapshot.ptt)).into_bytes())
        }
        "\\get_powerstat" => guard_read(ctx, || {
            format!("{}\n", u8::from(snapshot.power_on)).into_bytes()
        }),
        "\\dump_state" => DUMP_STATE.as_bytes().to_vec(),
        // chk_vfo: matches the captured Hamlib 4.7.0 "no-vfo" handshake reply.
        "\\chk_vfo" => b"0\n".to_vec(),

        // --- writes (require `write`) ---
        "F" | "\\set_freq" => match parts.next().and_then(parse_freq_hz) {
            Some(hz) => outcome_rprt(
                ctx.apply_modeled(
                    StateMutation::SetVfoFreq {
                        vfo: snapshot.rx_vfo,
                        hz,
                    },
                    CommandClass::ModeledWrite,
                )
                .await,
            ),
            None => RPRT_EINVAL.to_vec(),
        },
        "M" | "\\set_mode" => match parts.next() {
            Some(token) => {
                let mode = Mode::from_hamlib_token(token);
                outcome_rprt(
                    ctx.apply_modeled(
                        StateMutation::SetMode {
                            vfo: snapshot.rx_vfo,
                            mode,
                        },
                        CommandClass::ModeledWrite,
                    )
                    .await,
                )
            }
            None => RPRT_EINVAL.to_vec(),
        },
        "S" | "\\set_split_vfo" => {
            let enabled = parts.next().map(str::trim) == Some("1");
            let tx_vfo = match parts.next() {
                Some(v) if v.eq_ignore_ascii_case("VFOB") => Some(Vfo::B),
                _ => Some(Vfo::A),
            };
            outcome_rprt(
                ctx.apply_modeled(
                    StateMutation::SetSplit { enabled, tx_vfo },
                    CommandClass::ModeledWrite,
                )
                .await,
            )
        }
        "T" | "\\set_ptt" => {
            // Hamlib PTT values: 0 = RX, 1 = TX (generic), 2 = TX on mic, 3 = TX on data.
            // WSJT-X sends `T 3` (RIG_PTT_ON_DATA) to transmit in Data/Pkt mode, so the
            // source is honored on the wire (TS-590 `TX1;`) to route the DATA/USB audio
            // and avoid the data beep a bare `TX;` produces. Only 0 (or a missing arg)
            // means unkey.
            let (keyed, source) = match parts.next().map(str::trim) {
                Some("1") => (true, PttSource::Generic),
                Some("2") => (true, PttSource::Mic),
                Some("3") => (true, PttSource::Data),
                _ => (false, PttSource::Generic),
            };
            outcome_rprt(
                ctx.apply_modeled(
                    StateMutation::SetPtt { keyed, source },
                    CommandClass::PttWrite,
                )
                .await,
            )
        }
        // Accepted but unmodeled: selecting the active VFO never retargets on the wire.
        "V" | "\\set_vfo" => {
            if ctx.perms.allows(CommandClass::ModeledWrite) {
                RPRT_OK.to_vec()
            } else {
                RPRT_EINVAL.to_vec()
            }
        }
        // RIT (J/j) and XIT (Z/z): modeled offsets in Hz, gated on backend capability.
        // A zero offset disables the feature, matching rigctld semantics. The offset
        // always lands on the active VFO, so this never retargets a VFO on the wire.
        "j" | "\\get_rit" => guard_read(ctx, || {
            let off = if snapshot.rit_enabled {
                snapshot.rit_offset_hz
            } else {
                0
            };
            format!("{off}\n").into_bytes()
        }),
        "z" | "\\get_xit" => guard_read(ctx, || {
            let off = if snapshot.xit_enabled {
                snapshot.xit_offset_hz
            } else {
                0
            };
            format!("{off}\n").into_bytes()
        }),
        "J" | "\\set_rit" => {
            if ctx.caps.has_rit {
                match parts.next().and_then(|s| s.parse::<i32>().ok()) {
                    Some(offset_hz) => outcome_rprt(
                        ctx.apply_modeled(
                            StateMutation::SetRit {
                                offset_hz,
                                enabled: offset_hz != 0,
                            },
                            CommandClass::ModeledWrite,
                        )
                        .await,
                    ),
                    None => RPRT_EINVAL.to_vec(),
                }
            } else {
                RPRT_ENAVAIL.to_vec()
            }
        }
        "Z" | "\\set_xit" => {
            if ctx.caps.has_xit {
                match parts.next().and_then(|s| s.parse::<i32>().ok()) {
                    Some(offset_hz) => outcome_rprt(
                        ctx.apply_modeled(
                            StateMutation::SetXit {
                                offset_hz,
                                enabled: offset_hz != 0,
                            },
                            CommandClass::ModeledWrite,
                        )
                        .await,
                    ),
                    None => RPRT_EINVAL.to_vec(),
                }
            } else {
                RPRT_ENAVAIL.to_vec()
            }
        }
        _ => RPRT_ENAVAIL.to_vec(),
    };
    LineResult::Reply(reply)
}

/// Run a read-only command, enforcing the `read` permission.
fn guard_read(ctx: &FaceContext, f: impl FnOnce() -> Vec<u8>) -> Vec<u8> {
    if ctx.perms.allows(CommandClass::ModeledRead) {
        f()
    } else {
        RPRT_EINVAL.to_vec()
    }
}

/// Serve one accepted connection until it closes or quits.
pub(crate) async fn serve_conn<S>(stream: S, ctx: FaceContext)
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Send + 'static,
{
    let face_id = ctx.face_id;
    let (reader, mut writer) = tokio::io::split(stream);
    let mut lines = BufReader::new(reader).lines();
    loop {
        match lines.next_line().await {
            Ok(Some(line)) => {
                tracing::trace!(face_id, req = %line.trim(), "hamlib_net request");
                match handle_line(&line, &ctx).await {
                    LineResult::Reply(bytes) => {
                        tracing::trace!(
                            face_id,
                            reply = %String::from_utf8_lossy(&bytes).trim_end(),
                            "hamlib_net reply"
                        );
                        if !bytes.is_empty() && writer.write_all(&bytes).await.is_err() {
                            return;
                        }
                        let _ = writer.flush().await;
                    }
                    LineResult::Quit => {
                        tracing::trace!(face_id, "hamlib_net client quit");
                        return;
                    }
                }
            }
            Ok(None) | Err(_) => return,
        }
    }
}

/// Bind a Hamlib net endpoint and serve connections. Each connection gets a fresh
/// [`FaceContext`] sharing the same state/radio/ptt but its own face id and the
/// endpoint's permissions.
pub(crate) async fn run_listener(
    bind: &str,
    next_face_id: Arc<std::sync::atomic::AtomicU64>,
    template: FaceContext,
) -> std::io::Result<()> {
    let listener = TcpListener::bind(bind).await?;
    tracing::info!(bind, "hamlib_net endpoint listening");
    loop {
        let (stream, peer) = listener.accept().await?;
        let face_id = next_face_id.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let ctx = template.clone_with_face(face_id);
        tracing::debug!(%peer, face_id, "hamlib_net client connected");
        tokio::spawn(serve_conn(stream, ctx));
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;
    use crate::backend::loopback::LoopbackBackend;
    use crate::backend::RadioBackend;
    use crate::model::{RadioEventSource, StateChange};
    use crate::permissions::FacePermissions;
    use crate::ptt::PttManager;
    use crate::radio::{detached_link, spawn_scheduler};
    use crate::state::StateHandle;
    use std::time::Duration;

    fn ctx_with(perms: FacePermissions) -> (FaceContext, LoopbackBackend, StateHandle) {
        let backend = LoopbackBackend::new();
        let caps = backend.capabilities();
        let arc: Arc<dyn RadioBackend> = Arc::new(backend.clone());
        let state = StateHandle::new();
        let radio = spawn_scheduler(arc, detached_link(), state.clone());
        let ptt = PttManager::new(Duration::from_secs(300));
        let ctx = FaceContext::new(1, perms, state.clone(), radio, ptt, caps);
        (ctx, backend, state)
    }

    async fn reply_of(line: &str, ctx: &FaceContext) -> Vec<u8> {
        match handle_line(line, ctx).await {
            LineResult::Reply(b) => b,
            LineResult::Quit => b"<quit>".to_vec(),
        }
    }

    #[tokio::test]
    async fn get_freq_reads_from_state() {
        let (ctx, _b, state) = ctx_with(FacePermissions::read_only());
        state.record(
            StateChange::Freq {
                vfo: Vfo::A,
                hz: 7_030_000,
            },
            RadioEventSource::PollDiff,
        );
        assert_eq!(reply_of("f", &ctx).await, b"7030000\n".to_vec());
    }

    #[tokio::test]
    async fn get_mode_returns_token_and_passband() {
        let (ctx, _b, state) = ctx_with(FacePermissions::read_only());
        state.record(
            StateChange::Mode {
                vfo: Vfo::A,
                mode: Mode::Cw,
            },
            RadioEventSource::PollDiff,
        );
        assert_eq!(reply_of("m", &ctx).await, b"CW\n2400\n".to_vec());
    }

    #[tokio::test]
    async fn read_only_endpoint_rejects_set_freq() {
        let (ctx, backend, _s) = ctx_with(FacePermissions::read_only());
        assert_eq!(reply_of("F 14074000", &ctx).await, RPRT_EINVAL.to_vec());
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(backend.mutations().is_empty());
    }

    #[tokio::test]
    async fn read_only_endpoint_rejects_set_mode_and_ptt() {
        let (ctx, _b, _s) = ctx_with(FacePermissions::read_only());
        assert_eq!(reply_of("M USB 0", &ctx).await, RPRT_EINVAL.to_vec());
        assert_eq!(reply_of("T 1", &ctx).await, RPRT_EINVAL.to_vec());
    }

    #[tokio::test]
    async fn write_endpoint_sets_frequency() {
        let (ctx, backend, _s) = ctx_with(FacePermissions::from_tokens(&["read", "write", "ptt"]));
        assert_eq!(reply_of("F 14074000", &ctx).await, RPRT_OK.to_vec());
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert_eq!(
            backend.mutations(),
            vec![StateMutation::SetVfoFreq {
                vfo: Vfo::A,
                hz: 14_074_000
            }]
        );
    }

    #[tokio::test]
    async fn set_freq_accepts_hamlib_decimal_format() {
        // Hamlib (WSJT-X, engine, Log4OM) sends set_freq as a "%f" double, e.g.
        // "F 14040005.000000". Earlier this was rejected with RPRT -1.
        let (ctx, backend, _s) = ctx_with(FacePermissions::from_tokens(&["read", "write"]));
        assert_eq!(reply_of("F 14040005.000000", &ctx).await, RPRT_OK.to_vec());
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert_eq!(
            backend.mutations(),
            vec![StateMutation::SetVfoFreq {
                vfo: Vfo::A,
                hz: 14_040_005
            }]
        );
    }

    #[test]
    fn parse_freq_hz_handles_integer_and_decimal() {
        assert_eq!(parse_freq_hz("14040000"), Some(14_040_000));
        assert_eq!(parse_freq_hz("14040005.000000"), Some(14_040_005));
        assert_eq!(parse_freq_hz("0.000000"), Some(0));
        assert_eq!(parse_freq_hz("abc"), None);
        assert_eq!(parse_freq_hz(""), None);
    }

    #[tokio::test]
    async fn set_freq_never_retargets_vfo() {
        let (ctx, backend, _s) = ctx_with(FacePermissions::from_tokens(&["read", "write"]));
        let _ = reply_of("F 14074000", &ctx).await;
        tokio::time::sleep(Duration::from_millis(20)).await;
        // No SetSplit (FR/FT) mutation: frequency landed on the active VFO only.
        assert!(backend
            .mutations()
            .iter()
            .all(|m| matches!(m, StateMutation::SetVfoFreq { .. })));
    }

    #[tokio::test]
    async fn ptt_requires_ptt_permission() {
        let (ctx, _b, _s) = ctx_with(FacePermissions::from_tokens(&["read", "write"]));
        // Has write but not ptt.
        assert_eq!(reply_of("T 1", &ctx).await, RPRT_EINVAL.to_vec());
    }

    #[tokio::test]
    async fn set_ptt_keys_on_any_nonzero_value() {
        // WSJT-X in Data/Pkt mode sends `T 3` (RIG_PTT_ON_DATA), N1MM/others may send
        // `T 2` (on mic) or `T 1`; all must key. Only `T 0` unkeys.
        // `T 1` (generic), `T 2` (on mic), and `T 3` (on data) all key, but each selects
        // the matching transmit audio path on the wire. Only `T 0` unkeys.
        for (arg, source) in [
            ("1", PttSource::Generic),
            ("2", PttSource::Mic),
            ("3", PttSource::Data),
        ] {
            let (ctx, backend, _s) =
                ctx_with(FacePermissions::from_tokens(&["read", "write", "ptt"]));
            assert_eq!(
                reply_of(&format!("T {arg}"), &ctx).await,
                RPRT_OK.to_vec(),
                "T {arg} should be accepted"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
            assert_eq!(
                backend.mutations(),
                vec![StateMutation::SetPtt {
                    keyed: true,
                    source
                }],
                "T {arg} should key the radio with the matching source"
            );
        }

        let (ctx, backend, _s) = ctx_with(FacePermissions::from_tokens(&["read", "write", "ptt"]));
        assert_eq!(reply_of("T 0", &ctx).await, RPRT_OK.to_vec());
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert_eq!(
            backend.mutations(),
            vec![StateMutation::SetPtt {
                keyed: false,
                source: PttSource::Generic
            }]
        );
    }

    #[tokio::test]
    async fn dump_state_and_chk_vfo_match_golden() {
        let (ctx, _b, _s) = ctx_with(FacePermissions::read_only());
        let dump = reply_of("\\dump_state", &ctx).await;
        assert!(dump.starts_with(b"1\n"), "protocol version 1 first line");
        assert!(dump.ends_with(b"done\n"));
        assert_eq!(reply_of("\\chk_vfo", &ctx).await, b"0\n".to_vec());
        assert_eq!(reply_of("\\get_powerstat", &ctx).await, b"1\n".to_vec());
    }

    #[tokio::test]
    async fn quit_closes() {
        let (ctx, _b, _s) = ctx_with(FacePermissions::read_only());
        assert!(matches!(handle_line("q", &ctx).await, LineResult::Quit));
    }

    #[tokio::test]
    async fn set_rit_applies_offset_when_capable() {
        let (ctx, backend, state) = ctx_with(FacePermissions::from_tokens(&["read", "write"]));
        assert_eq!(reply_of("\\set_rit 100", &ctx).await, RPRT_OK.to_vec());
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert_eq!(
            backend.mutations(),
            vec![StateMutation::SetRit {
                offset_hz: 100,
                enabled: true,
            }]
        );
        let snap = state.snapshot();
        assert!(snap.rit_enabled);
        assert_eq!(snap.rit_offset_hz, 100);
    }

    #[tokio::test]
    async fn set_xit_applies_offset_when_capable() {
        let (ctx, backend, state) = ctx_with(FacePermissions::from_tokens(&["read", "write"]));
        assert_eq!(reply_of("Z -250", &ctx).await, RPRT_OK.to_vec());
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert_eq!(
            backend.mutations(),
            vec![StateMutation::SetXit {
                offset_hz: -250,
                enabled: true,
            }]
        );
        assert_eq!(state.snapshot().xit_offset_hz, -250);
    }

    #[tokio::test]
    async fn set_rit_zero_offset_disables_rit() {
        let (ctx, _b, state) = ctx_with(FacePermissions::from_tokens(&["read", "write"]));
        assert_eq!(reply_of("\\set_rit 0", &ctx).await, RPRT_OK.to_vec());
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(!state.snapshot().rit_enabled);
    }

    #[tokio::test]
    async fn get_rit_and_xit_report_offsets() {
        let (ctx, _b, state) = ctx_with(FacePermissions::read_only());
        assert_eq!(reply_of("\\get_rit", &ctx).await, b"0\n".to_vec());
        state.record(
            StateChange::Rit {
                enabled: true,
                offset_hz: 250,
            },
            RadioEventSource::PollDiff,
        );
        state.record(
            StateChange::Xit {
                enabled: true,
                offset_hz: -50,
            },
            RadioEventSource::PollDiff,
        );
        assert_eq!(reply_of("j", &ctx).await, b"250\n".to_vec());
        assert_eq!(reply_of("z", &ctx).await, b"-50\n".to_vec());
    }

    #[tokio::test]
    async fn rit_and_xit_report_not_available_without_capability() {
        // A backend that does not model RIT/XIT must answer not-available.
        let backend = LoopbackBackend::new();
        let mut caps = backend.capabilities();
        caps.has_rit = false;
        caps.has_xit = false;
        let arc: Arc<dyn RadioBackend> = Arc::new(backend);
        let state = StateHandle::new();
        let radio = spawn_scheduler(arc, detached_link(), state.clone());
        let ptt = PttManager::new(Duration::from_secs(300));
        let ctx = FaceContext::new(
            1,
            FacePermissions::from_tokens(&["read", "write"]),
            state,
            radio,
            ptt,
            caps,
        );
        assert_eq!(reply_of("\\set_rit 100", &ctx).await, RPRT_ENAVAIL.to_vec());
        assert_eq!(reply_of("\\set_xit 100", &ctx).await, RPRT_ENAVAIL.to_vec());
    }
}
