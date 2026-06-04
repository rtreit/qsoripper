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

use tokio::io::{AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;

use crate::dialect::{ApplyOutcome, FaceContext};
use crate::model::{Mode, PttSource, StateMutation, Vfo};
use crate::permissions::CommandClass;
use crate::state::Snapshot;

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

/// The VFO whose data this face presents when a client requests `requested`.
///
/// Single-VFO faces (N1MM SO1V, Log4OM) always present the operating (receive) VFO so
/// the radio's physical A/B identity never leaks; dual-VFO faces honor the literal
/// requested VFO. This is the chokepoint that makes single-VFO loggers follow A/B swaps.
fn presented_vfo(ctx: &FaceContext, snapshot: &Snapshot, requested: Vfo) -> Vfo {
    if ctx.single_vfo() {
        snapshot.rx_vfo
    } else {
        requested
    }
}

/// The VFO name this face advertises for the real VFO `real`. Single-VFO faces always
/// claim `VFOA` (the operating VFO is virtualized as A); dual-VFO faces report the real
/// name.
fn presented_vfo_name(ctx: &FaceContext, real: Vfo) -> &'static str {
    if ctx.single_vfo() {
        "VFOA"
    } else {
        vfo_name(real)
    }
}

/// The split flag this face exposes. Single-VFO faces always hide split (they know only
/// the operating VFO), matching the single-VFO virtualization contract.
fn presented_split(ctx: &FaceContext, snapshot: &Snapshot) -> bool {
    !ctx.single_vfo() && snapshot.split
}

/// Parse a requested VFO name argument into a [`Vfo`], defaulting to `VFOA`.
fn parse_vfo_arg(arg: Option<&&str>) -> Vfo {
    match arg {
        Some(v) if v.eq_ignore_ascii_case("VFOB") => Vfo::B,
        _ => Vfo::A,
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

/// Resolve a short or long command token to its Hamlib long name, used to echo the
/// received command as the first record of an Extended Response Protocol reply.
fn long_name(cmd: &str) -> Option<&'static str> {
    Some(match cmd {
        "F" | "\\set_freq" => "set_freq",
        "M" | "\\set_mode" => "set_mode",
        "V" | "\\set_vfo" => "set_vfo",
        "T" | "\\set_ptt" => "set_ptt",
        "S" | "\\set_split_vfo" => "set_split_vfo",
        "J" | "\\set_rit" => "set_rit",
        "Z" | "\\set_xit" => "set_xit",
        "f" | "\\get_freq" => "get_freq",
        "m" | "\\get_mode" => "get_mode",
        "v" | "\\get_vfo" => "get_vfo",
        "t" | "\\get_ptt" => "get_ptt",
        "s" | "\\get_split_vfo" => "get_split_vfo",
        "j" | "\\get_rit" => "get_rit",
        "z" | "\\get_xit" => "get_xit",
        "\\get_vfo_info" => "get_vfo_info",
        "\\get_powerstat" => "get_powerstat",
        "\\chk_vfo" => "chk_vfo",
        "\\dump_state" => "dump_state",
        _ => return None,
    })
}

/// Detect a leading Extended Response Protocol separator. A command prefixed with `+`
/// (newline-joined) or `;` / `|` / `,` (single-line, joined by that char) selects the
/// extended response format. Returns the record separator and the rest of the line.
fn split_erp(line: &str) -> Option<(char, &str)> {
    let first = line.chars().next()?;
    let sep = match first {
        '+' => '\n',
        ';' | '|' | ',' => first,
        _ => return None,
    };
    Some((sep, &line[first.len_utf8()..]))
}

/// Join Extended Response Protocol records (echo header, data records, `RPRT x`) with the
/// chosen separator and terminate the block with a newline, matching real `rigctld`.
fn erp_records(sep: char, records: &[String]) -> Vec<u8> {
    let mut s = records.join(&sep.to_string());
    s.push('\n');
    s.into_bytes()
}

/// Build the extended-response echo header for a command: the long name, a colon, and
/// (if any) the received arguments, e.g. `set_freq: 14200000` or `get_freq:`.
fn ext_echo(cmd: &str, args: &[&str]) -> String {
    let name = long_name(cmd).unwrap_or(cmd);
    if args.is_empty() {
        format!("{name}:")
    } else {
        format!("{name}: {}", args.join(" "))
    }
}

/// Handle one rigctld protocol line against the universal state, dispatching the
/// Extended Response Protocol (separator-prefixed) path used by Log4OM-NG separately
/// from the plain protocol used by WSJT-X / N1MM / the engine.
async fn handle_line(line: &str, ctx: &FaceContext) -> LineResult {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return LineResult::Reply(Vec::new());
    }
    if let Some((sep, rest)) = split_erp(trimmed) {
        return LineResult::Reply(handle_ext(sep, rest, ctx).await);
    }
    let mut parts = trimmed.split_whitespace();
    let Some(cmd) = parts.next() else {
        return LineResult::Reply(Vec::new());
    };
    let args: Vec<&str> = parts.collect();
    dispatch_plain(cmd, &args, ctx).await
}

/// Handle one Extended Response Protocol line. Log4OM-NG opens every session with
/// `;V ?` (list supported VFOs) and then polls with `+\get_vfo_info VFOA`, so those two
/// shapes are formatted explicitly; the common labeled gets are also supported, and any
/// other command (sets, unknown) is wrapped generically as `echo <sep> <plain reply>`.
async fn handle_ext(sep: char, rest: &str, ctx: &FaceContext) -> Vec<u8> {
    let trimmed = rest.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }
    let mut parts = trimmed.split_whitespace();
    let Some(cmd) = parts.next() else {
        return Vec::new();
    };
    let args: Vec<&str> = parts.collect();
    let snapshot = ctx.snapshot();
    let reads = ctx.perms.allows(CommandClass::ModeledRead);

    // `?` query on set_vfo: list supported VFOs. This is Log4OM-NG's handshake (`;V ?`).
    // The supported-token list line is newline-terminated regardless of the separator,
    // matching real `rigctld`.
    if matches!(cmd, "V" | "\\set_vfo") && args.first() == Some(&"?") {
        let vfos = if ctx.single_vfo() {
            "VFOA "
        } else {
            "VFOA VFOB "
        };
        return format!("set_vfo: ?{sep}{vfos}\nRPRT 0\n").into_bytes();
    }

    // get_vfo_info: Log4OM-NG's poll command (`+\get_vfo_info VFOA`). Reports the named
    // VFO's frequency, mode, passband, split, and (always 0) satellite mode.
    if cmd == "\\get_vfo_info" {
        if !reads {
            return erp_records(sep, &[ext_echo(cmd, &args), "RPRT -1".to_string()]);
        }
        let vfo = presented_vfo(ctx, &snapshot, parse_vfo_arg(args.first()));
        let v = snapshot.vfo(vfo);
        return erp_records(
            sep,
            &[
                format!("get_vfo_info: {}", presented_vfo_name(ctx, vfo)),
                format!("Freq: {}", v.freq_hz),
                format!("Mode: {}", v.mode.hamlib_token_with_data(v.data)),
                format!("Width: {}", v.passband_hz),
                format!("Split: {}", u8::from(presented_split(ctx, &snapshot))),
                "SatMode: 0".to_string(),
                "RPRT 0".to_string(),
            ],
        );
    }

    // Labeled simple gets, formatted with their Hamlib value labels.
    let labeled: Option<Vec<String>> = match cmd {
        "f" | "\\get_freq" if reads => Some(vec![
            "get_freq:".to_string(),
            format!("Frequency: {}", snapshot.vfo(snapshot.rx_vfo).freq_hz),
            "RPRT 0".to_string(),
        ]),
        "m" | "\\get_mode" if reads => {
            let v = snapshot.vfo(snapshot.rx_vfo);
            Some(vec![
                "get_mode:".to_string(),
                format!("Mode: {}", v.mode.hamlib_token_with_data(v.data)),
                format!("Passband: {}", v.passband_hz),
                "RPRT 0".to_string(),
            ])
        }
        "v" | "\\get_vfo" if reads => Some(vec![
            "get_vfo:".to_string(),
            format!("VFO: {}", presented_vfo_name(ctx, snapshot.rx_vfo)),
            "RPRT 0".to_string(),
        ]),
        "t" | "\\get_ptt" if reads => Some(vec![
            "get_ptt:".to_string(),
            format!("PTT: {}", u8::from(snapshot.ptt)),
            "RPRT 0".to_string(),
        ]),
        _ => None,
    };
    if let Some(records) = labeled {
        return erp_records(sep, &records);
    }

    // Generic fallback: sets, permission-denied reads, and unknown commands. Echo the
    // command then append the plain dispatch reply (e.g. `RPRT 0`) as trailing records.
    let echo = ext_echo(cmd, &args);
    match dispatch_plain(cmd, &args, ctx).await {
        LineResult::Quit => Vec::new(),
        LineResult::Reply(bytes) => {
            let mut records = vec![echo];
            for chunk in String::from_utf8_lossy(&bytes).split('\n') {
                if !chunk.is_empty() {
                    records.push(chunk.to_string());
                }
            }
            // An ERP client reads until it sees a terminating `RPRT` record. Plain gets
            // (e.g. `\get_split_vfo`, `\get_powerstat`, `\chk_vfo`, rit/xit) return data
            // only, with no `RPRT`, so a client falling through this generic path would
            // block waiting for a terminator that never arrives. Guarantee one.
            if !records.iter().any(|r| r.starts_with("RPRT")) {
                records.push("RPRT 0".to_string());
            }
            erp_records(sep, &records)
        }
    }
}

/// Dispatch one plain (non-extended) rigctld protocol command against the universal state.
#[allow(clippy::too_many_lines)] // A flat protocol dispatch table reads best as one match.
async fn dispatch_plain(cmd: &str, args: &[&str], ctx: &FaceContext) -> LineResult {
    let snapshot = ctx.snapshot();

    let reply: Vec<u8> = match cmd {
        "q" | "Q" => return LineResult::Quit,

        // --- reads (require `read`) ---
        "f" | "\\get_freq" => guard_read(ctx, || {
            format!("{}\n", snapshot.vfo(snapshot.rx_vfo).freq_hz).into_bytes()
        }),
        "m" | "\\get_mode" => guard_read(ctx, || {
            let v = snapshot.vfo(snapshot.rx_vfo);
            format!(
                "{}\n{}\n",
                v.mode.hamlib_token_with_data(v.data),
                v.passband_hz
            )
            .into_bytes()
        }),
        "v" | "\\get_vfo" => guard_read(ctx, || {
            format!("{}\n", presented_vfo_name(ctx, snapshot.rx_vfo)).into_bytes()
        }),
        // get_vfo_info: report the named VFO's freq/mode/width/split/satmode as bare
        // values (Freq, Mode, Width, Split, SatMode), matching real `rigctld`.
        "\\get_vfo_info" => guard_read(ctx, || {
            let vfo = presented_vfo(ctx, &snapshot, parse_vfo_arg(args.first()));
            let v = snapshot.vfo(vfo);
            format!(
                "{}\n{}\n{}\n{}\n0\n",
                v.freq_hz,
                v.mode.hamlib_token_with_data(v.data),
                v.passband_hz,
                u8::from(presented_split(ctx, &snapshot)),
            )
            .into_bytes()
        }),
        "s" | "\\get_split_vfo" => guard_read(ctx, || {
            let split = presented_split(ctx, &snapshot);
            let tx = if split {
                snapshot.tx_vfo
            } else {
                snapshot.rx_vfo
            };
            format!("{}\n{}\n", u8::from(split), presented_vfo_name(ctx, tx)).into_bytes()
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
        "F" | "\\set_freq" => match args.first().copied().and_then(parse_freq_hz) {
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
        "M" | "\\set_mode" => match args.first().copied() {
            Some(token) => {
                // The TS-590 splits data operation into a base mode (`MD`) and an
                // independent DATA flag (`DA`), so a `PKTUSB` request becomes two modeled
                // writes: the base mode, then the DATA flag. Each is deduped independently,
                // so re-asserting PKTUSB writes nothing and switching PKTUSB→USB only emits
                // `DA0`. The base write is applied first; if it fails the data write is
                // skipped and its error is reported.
                let (base, data) = Mode::decompose_hamlib_token(token);
                // An unrecognized mode token decodes to `Mode::Unknown`, which renders as
                // USB. Silently applying it would retune the radio to USB while reporting
                // `RPRT 0` (false success). Reject it so the client sees the error.
                if base == Mode::Unknown {
                    return LineResult::Reply(RPRT_EINVAL.to_vec());
                }
                let mode_outcome = ctx
                    .apply_modeled(
                        StateMutation::SetMode {
                            vfo: snapshot.rx_vfo,
                            mode: base,
                        },
                        CommandClass::ModeledWrite,
                    )
                    .await;
                if mode_outcome == ApplyOutcome::Ok {
                    outcome_rprt(
                        ctx.apply_modeled(
                            StateMutation::SetDataMode {
                                vfo: snapshot.rx_vfo,
                                on: data,
                            },
                            CommandClass::ModeledWrite,
                        )
                        .await,
                    )
                } else {
                    outcome_rprt(mode_outcome)
                }
            }
            None => RPRT_EINVAL.to_vec(),
        },
        "S" | "\\set_split_vfo" => {
            if !ctx.perms.allows(CommandClass::ModeledWrite) {
                RPRT_EINVAL.to_vec()
            } else if ctx.single_vfo() {
                // Single-VFO faces virtualize away split entirely. Absorb the request
                // without mutating real radio split so reads stay consistent (Split: 0)
                // and a single-VFO client never desyncs the radio's true split state.
                RPRT_OK.to_vec()
            } else {
                let enabled = args.first().copied() == Some("1");
                let tx_vfo = match args.get(1).copied() {
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
        }
        "T" | "\\set_ptt" => {
            // Hamlib PTT values: 0 = RX, 1 = TX (generic), 2 = TX on mic, 3 = TX on data.
            // WSJT-X sends `T 3` (RIG_PTT_ON_DATA) to transmit in Data/Pkt mode, so the
            // source is honored on the wire (TS-590 `TX1;`) to route the DATA/USB audio
            // and avoid the data beep a bare `TX;` produces. Only 0 (or a missing arg)
            // means unkey.
            let (keyed, source) = match args.first().copied() {
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
                match args.first().and_then(|s| s.parse::<i32>().ok()) {
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
                match args.first().and_then(|s| s.parse::<i32>().ok()) {
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
    let mut reader = BufReader::new(reader);
    let mut line: Vec<u8> = Vec::with_capacity(64);
    let mut byte = [0u8; 1];

    'serve: loop {
        // Read one line manually with a hard length cap so a client that never sends a
        // newline cannot grow the buffer without bound (OOM/DoS via `BufReader::lines`,
        // which has no upper limit). An over-long line is discarded and the connection
        // closed.
        line.clear();
        let eof = loop {
            match reader.read(&mut byte).await {
                Ok(0) | Err(_) => break true,
                Ok(_) => {
                    if byte[0] == b'\n' {
                        break false;
                    }
                    if line.len() >= MAX_LINE_LEN {
                        tracing::warn!(
                            face_id,
                            "hamlib_net request exceeded {MAX_LINE_LEN} bytes without a \
                             newline; closing connection"
                        );
                        break 'serve;
                    }
                    line.push(byte[0]);
                }
            }
        };
        if eof {
            break 'serve;
        }

        // Trim a trailing CR so CRLF clients are handled.
        if line.last() == Some(&b'\r') {
            line.pop();
        }
        let text = String::from_utf8_lossy(&line).into_owned();
        tracing::trace!(face_id, req = %text.trim(), "hamlib_net request");
        match handle_line(&text, &ctx).await {
            LineResult::Reply(bytes) => {
                tracing::trace!(
                    face_id,
                    reply = %String::from_utf8_lossy(&bytes).trim_end(),
                    "hamlib_net reply"
                );
                if !bytes.is_empty() && writer.write_all(&bytes).await.is_err() {
                    break 'serve;
                }
                let _ = writer.flush().await;
            }
            LineResult::Quit => {
                tracing::trace!(face_id, "hamlib_net client quit");
                break 'serve;
            }
        }
    }

    // The connection closed: never leave the radio keyed on behalf of a vanished client.
    ctx.release_ptt_on_disconnect().await;
}

/// Maximum bytes buffered for a single rigctld request line before the connection is
/// closed. Real rigctld commands are short; this only bounds a misbehaving client.
const MAX_LINE_LEN: usize = 4096;

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

    /// A single-VFO-virtualized face: the operating VFO is always presented as `VFOA`,
    /// split is hidden, and physical A/B identity never leaks (N1MM SO1V, Log4OM).
    fn ctx_single_vfo(perms: FacePermissions) -> (FaceContext, LoopbackBackend, StateHandle) {
        let (ctx, backend, state) = ctx_with(perms);
        (ctx.with_single_vfo(true), backend, state)
    }

    /// Put the radio on physical VFO B (14.074 USB) with VFO A on 14.035 CW, split on
    /// with TX on the inactive VFO — the classic state a single-VFO logger must collapse
    /// to "operating VFO is VFOA".
    fn operating_on_b(state: &StateHandle) {
        state.record(
            StateChange::Freq {
                vfo: Vfo::A,
                hz: 14_035_000,
            },
            RadioEventSource::PollDiff,
        );
        state.record(
            StateChange::Mode {
                vfo: Vfo::A,
                mode: Mode::Cw,
            },
            RadioEventSource::PollDiff,
        );
        state.record(
            StateChange::Freq {
                vfo: Vfo::B,
                hz: 14_074_000,
            },
            RadioEventSource::PollDiff,
        );
        state.record(
            StateChange::Mode {
                vfo: Vfo::B,
                mode: Mode::Usb,
            },
            RadioEventSource::PollDiff,
        );
        state.record(
            StateChange::RxVfo { vfo: Vfo::B },
            RadioEventSource::PollDiff,
        );
        state.record(
            StateChange::Split {
                enabled: true,
                tx_vfo: Some(Vfo::A),
            },
            RadioEventSource::PollDiff,
        );
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
    async fn get_freq_and_mode_follow_active_vfo_b() {
        let (ctx, _b, state) = ctx_with(FacePermissions::read_only());
        state.record(
            StateChange::Freq {
                vfo: Vfo::B,
                hz: 14_034_320,
            },
            RadioEventSource::PollDiff,
        );
        state.record(
            StateChange::Mode {
                vfo: Vfo::B,
                mode: Mode::Cw,
            },
            RadioEventSource::PollDiff,
        );
        state.record(
            StateChange::RxVfo { vfo: Vfo::B },
            RadioEventSource::PollDiff,
        );

        assert_eq!(reply_of("f", &ctx).await, b"14034320\n".to_vec());
        assert_eq!(reply_of("m", &ctx).await, b"CW\n2400\n".to_vec());
        assert_eq!(reply_of("v", &ctx).await, b"VFOB\n".to_vec());
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
    async fn set_mode_pktusb_writes_base_mode_then_data_flag() {
        // WSJT-X sends PKTUSB/PKTLSB for digital modes. The hub must split it into a base
        // mode write plus the independent DATA flag. Starting from the default USB/no-data,
        // PKTLSB is a genuine change on both axes, so both mutations reach the radio in order.
        let (ctx, backend, _s) = ctx_with(FacePermissions::from_tokens(&["read", "write"]));
        assert_eq!(reply_of("M PKTLSB 0", &ctx).await, RPRT_OK.to_vec());
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert_eq!(
            backend.mutations(),
            vec![
                StateMutation::SetMode {
                    vfo: Vfo::A,
                    mode: Mode::Lsb,
                },
                StateMutation::SetDataMode {
                    vfo: Vfo::A,
                    on: true,
                },
            ]
        );
    }

    #[tokio::test]
    async fn set_mode_plain_clears_data_flag() {
        // Switching from a data mode to a plain mode must emit DA0. Prime DATA on, then send
        // plain USB: the base mode is unchanged (deduped) but the DATA-off write lands.
        let (ctx, backend, state) = ctx_with(FacePermissions::from_tokens(&["read", "write"]));
        state.record(
            StateChange::DataMode {
                vfo: Vfo::A,
                on: true,
            },
            RadioEventSource::PollDiff,
        );
        assert_eq!(reply_of("M USB 0", &ctx).await, RPRT_OK.to_vec());
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert_eq!(
            backend.mutations(),
            vec![StateMutation::SetDataMode {
                vfo: Vfo::A,
                on: false,
            }]
        );
    }

    #[tokio::test]
    async fn set_freq_and_mode_target_active_vfo_b() {
        // WSJT-X / Log4OM write through the hamlib_net face. When the rig is on VFO B, a
        // set_freq / set_mode must land on VFO B (the active VFO), never be forced to A.
        let (ctx, backend, state) = ctx_with(FacePermissions::from_tokens(&["read", "write"]));
        state.record(
            StateChange::RxVfo { vfo: Vfo::B },
            RadioEventSource::PollDiff,
        );
        assert_eq!(reply_of("F 14034320", &ctx).await, RPRT_OK.to_vec());
        assert_eq!(reply_of("M CW 0", &ctx).await, RPRT_OK.to_vec());
        tokio::time::sleep(Duration::from_millis(20)).await;
        let muts = backend.mutations();
        assert!(
            muts.contains(&StateMutation::SetVfoFreq {
                vfo: Vfo::B,
                hz: 14_034_320,
            }),
            "set_freq must target active VFO B, got {muts:?}"
        );
        assert!(
            muts.contains(&StateMutation::SetMode {
                vfo: Vfo::B,
                mode: Mode::Cw,
            }),
            "set_mode must target active VFO B, got {muts:?}"
        );
    }

    #[tokio::test]
    async fn get_mode_composes_pkt_token_when_data_on() {
        let (ctx, _b, state) = ctx_with(FacePermissions::read_only());
        state.record(
            StateChange::DataMode {
                vfo: Vfo::A,
                on: true,
            },
            RadioEventSource::PollDiff,
        );
        // Default base mode is USB; with DATA on a Hamlib client must read PKTUSB.
        assert_eq!(reply_of("m", &ctx).await, b"PKTUSB\n2400\n".to_vec());
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

    // --- Extended Response Protocol (Log4OM-NG) ---

    #[tokio::test]
    async fn erp_set_vfo_query_lists_supported_vfos() {
        // Log4OM-NG opens every session with `;V ?` (extended set_vfo query). The reply
        // must echo the command, list the supported VFOs (newline before RPRT, matching
        // real rigctld), and end with RPRT 0 — all `;`-separated on one logical block.
        let (ctx, _b, _s) = ctx_with(FacePermissions::from_tokens(&["read", "write"]));
        assert_eq!(
            reply_of(";V ?", &ctx).await,
            b"set_vfo: ?;VFOA VFOB \nRPRT 0\n".to_vec()
        );
    }

    #[tokio::test]
    async fn erp_get_vfo_info_reports_active_vfo_block() {
        // Log4OM-NG polls with `+\get_vfo_info VFOA` (newline-separated extended form).
        let (ctx, _b, state) = ctx_with(FacePermissions::from_tokens(&["read", "write"]));
        state.record(
            StateChange::Freq {
                vfo: Vfo::A,
                hz: 14_074_000,
            },
            RadioEventSource::PollDiff,
        );
        state.record(
            StateChange::Mode {
                vfo: Vfo::A,
                mode: Mode::Cw,
            },
            RadioEventSource::PollDiff,
        );
        assert_eq!(
            reply_of("+\\get_vfo_info VFOA", &ctx).await,
            b"get_vfo_info: VFOA\nFreq: 14074000\nMode: CW\nWidth: 2400\nSplit: 0\nSatMode: 0\nRPRT 0\n"
                .to_vec()
        );
    }

    #[tokio::test]
    async fn plain_get_vfo_info_reports_bare_values() {
        // The non-extended form returns just the values (Freq, Mode, Width, Split, SatMode).
        let (ctx, _b, state) = ctx_with(FacePermissions::read_only());
        state.record(
            StateChange::Freq {
                vfo: Vfo::A,
                hz: 7_030_000,
            },
            RadioEventSource::PollDiff,
        );
        state.record(
            StateChange::Mode {
                vfo: Vfo::A,
                mode: Mode::Cw,
            },
            RadioEventSource::PollDiff,
        );
        assert_eq!(
            reply_of("\\get_vfo_info VFOA", &ctx).await,
            b"7030000\nCW\n2400\n0\n0\n".to_vec()
        );
    }

    #[tokio::test]
    async fn erp_generic_set_echoes_command_then_rprt() {
        // A generic extended set (e.g. `+F`) echoes `set_freq: <arg>` then `RPRT 0`,
        // newline-separated for the `+` separator.
        let (ctx, _b, _s) = ctx_with(FacePermissions::from_tokens(&["read", "write"]));
        assert_eq!(
            reply_of("+F 14200000", &ctx).await,
            b"set_freq: 14200000\nRPRT 0\n".to_vec()
        );
    }

    #[tokio::test]
    async fn erp_set_vfo_with_arg_echoes_on_one_line() {
        // `;V VFOA` selects the active VFO (never retargets) and replies on one line.
        let (ctx, _b, _s) = ctx_with(FacePermissions::from_tokens(&["read", "write"]));
        assert_eq!(
            reply_of(";V VFOA", &ctx).await,
            b"set_vfo: VFOA;RPRT 0\n".to_vec()
        );
    }

    #[tokio::test]
    async fn erp_labeled_get_freq_single_line() {
        // `;f` returns a single-line labeled extended response.
        let (ctx, _b, state) = ctx_with(FacePermissions::read_only());
        state.record(
            StateChange::Freq {
                vfo: Vfo::A,
                hz: 14_074_000,
            },
            RadioEventSource::PollDiff,
        );
        assert_eq!(
            reply_of(";f", &ctx).await,
            b"get_freq:;Frequency: 14074000;RPRT 0\n".to_vec()
        );
    }

    #[tokio::test]
    async fn erp_get_split_terminates_with_rprt() {
        // `\get_split_vfo` returns data only in the plain protocol. Through the ERP generic
        // fallback it must still end with an `RPRT` record so an ERP client (Log4OM-NG)
        // reading until the terminator does not block.
        let (ctx, _b, _s) = ctx_with(FacePermissions::read_only());
        assert_eq!(
            reply_of("+\\get_split_vfo", &ctx).await,
            b"get_split_vfo:\n0\nVFOA\nRPRT 0\n".to_vec()
        );
    }

    #[tokio::test]
    async fn set_mode_rejects_unknown_token() {
        // An unrecognized mode token must be rejected (RPRT -1), never silently applied as
        // USB with a false `RPRT 0`.
        let (ctx, backend, _s) = ctx_with(FacePermissions::from_tokens(&["read", "write"]));
        assert_eq!(reply_of("M WAT", &ctx).await, RPRT_EINVAL.to_vec());
        // And nothing was written to the radio.
        assert!(
            backend.mutations().is_empty(),
            "an unknown mode must not mutate the radio"
        );
    }

    #[tokio::test]
    async fn dropping_a_keyed_hamlib_client_releases_the_ptt_lease() {
        // A rigctld client keys PTT then its TCP connection drops. The hub must release the
        // lease immediately, not hold the transmitter up until the safety ceiling.
        let backend = LoopbackBackend::new();
        let caps = backend.capabilities();
        let arc: Arc<dyn RadioBackend> = Arc::new(backend);
        let state = StateHandle::new();
        let radio = spawn_scheduler(arc, detached_link(), state.clone());
        let ptt = PttManager::new(Duration::from_secs(300));
        let perms = FacePermissions::from_tokens(&["read", "ptt"]);
        let ctx = FaceContext::new(7, perms, state.clone(), radio, ptt.clone(), caps);
        let (client, server) = tokio::io::duplex(1024);
        let handle = tokio::spawn(serve_conn(server, ctx));

        let (mut cr, mut cw) = tokio::io::split(client);
        cw.write_all(b"T 1\n").await.expect("write");
        // Drain the RPRT reply so the key has been processed.
        let mut buf = [0u8; 32];
        let _ = tokio::time::timeout(Duration::from_secs(2), cr.read(&mut buf)).await;
        for _ in 0..50 {
            if ptt.owner() == Some(7) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(ptt.owner(), Some(7), "client should hold the PTT lease");

        // Drop the transport.
        drop(cr);
        drop(cw);
        let _ = tokio::time::timeout(Duration::from_secs(2), handle).await;
        assert_eq!(
            ptt.owner(),
            None,
            "PTT lease must release when a keyed rigctld client disconnects"
        );
    }

    #[tokio::test]
    async fn overlong_request_without_newline_closes_the_connection() {
        // A client that streams bytes without ever sending a newline must not grow the
        // line buffer without bound; the hub caps it and closes the connection.
        let backend = LoopbackBackend::new();
        let caps = backend.capabilities();
        let arc: Arc<dyn RadioBackend> = Arc::new(backend);
        let state = StateHandle::new();
        let radio = spawn_scheduler(arc, detached_link(), state.clone());
        let ptt = PttManager::new(Duration::from_secs(300));
        let perms = FacePermissions::from_tokens(&["read"]);
        let ctx = FaceContext::new(9, perms, state.clone(), radio, ptt.clone(), caps);
        let (client, server) = tokio::io::duplex(8192);
        let handle = tokio::spawn(serve_conn(server, ctx));

        let (_cr, mut cw) = tokio::io::split(client);
        // Send well over the cap with no newline. Ignore write errors that occur once the
        // server side has already closed.
        let flood = vec![b'f'; MAX_LINE_LEN + 256];
        let _ = cw.write_all(&flood).await;

        // The serve task must terminate (connection closed) rather than hang or OOM.
        let joined = tokio::time::timeout(Duration::from_secs(2), handle).await;
        assert!(
            joined.is_ok(),
            "serve_conn must close the connection on an overlong line"
        );
    }

    // --- single-VFO virtualization (N1MM SO1V, Log4OM) ---

    #[tokio::test]
    async fn single_vfo_erp_get_vfo_info_follows_operating_vfo_as_vfoa() {
        // Log4OM bug: it polls `+\get_vfo_info VFOA` and must see the OPERATING VFO (B),
        // presented as VFOA, with split hidden — not stale physical VFO A.
        let (ctx, _b, state) = ctx_single_vfo(FacePermissions::from_tokens(&["read", "write"]));
        operating_on_b(&state);
        assert_eq!(
            reply_of("+\\get_vfo_info VFOA", &ctx).await,
            b"get_vfo_info: VFOA\nFreq: 14074000\nMode: USB\nWidth: 2400\nSplit: 0\nSatMode: 0\nRPRT 0\n"
                .to_vec()
        );
    }

    #[tokio::test]
    async fn single_vfo_plain_get_vfo_info_follows_operating_vfo() {
        let (ctx, _b, state) = ctx_single_vfo(FacePermissions::read_only());
        operating_on_b(&state);
        // Bare values: operating VFO B's freq/mode/width, split forced to 0.
        assert_eq!(
            reply_of("\\get_vfo_info VFOA", &ctx).await,
            b"14074000\nUSB\n2400\n0\n0\n".to_vec()
        );
    }

    #[tokio::test]
    async fn single_vfo_get_vfo_always_reports_vfoa() {
        let (ctx, _b, state) = ctx_single_vfo(FacePermissions::read_only());
        operating_on_b(&state);
        // Even operating on physical B, a single-VFO face claims VFOA.
        assert_eq!(reply_of("v", &ctx).await, b"VFOA\n".to_vec());
    }

    #[tokio::test]
    async fn single_vfo_get_split_vfo_hides_split() {
        let (ctx, _b, state) = ctx_single_vfo(FacePermissions::read_only());
        operating_on_b(&state);
        // Radio is split, but a single-VFO face reports no split and TX on VFOA.
        assert_eq!(reply_of("s", &ctx).await, b"0\nVFOA\n".to_vec());
    }

    #[tokio::test]
    async fn single_vfo_erp_set_vfo_query_lists_only_vfoa() {
        let (ctx, _b, _s) = ctx_single_vfo(FacePermissions::from_tokens(&["read", "write"]));
        assert_eq!(
            reply_of(";V ?", &ctx).await,
            b"set_vfo: ?;VFOA \nRPRT 0\n".to_vec()
        );
    }

    #[tokio::test]
    async fn single_vfo_set_split_is_absorbed_without_mutating_radio() {
        let (ctx, backend, _s) = ctx_single_vfo(FacePermissions::from_tokens(&["read", "write"]));
        // A single-VFO client must never desync the radio's real split state.
        assert_eq!(reply_of("S 1 VFOB", &ctx).await, RPRT_OK.to_vec());
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(
            backend.mutations().is_empty(),
            "single-VFO set_split_vfo must not mutate real radio split"
        );
    }

    #[tokio::test]
    async fn single_vfo_read_only_set_split_is_denied() {
        let (ctx, _b, _s) = ctx_single_vfo(FacePermissions::read_only());
        assert_eq!(reply_of("S 1 VFOB", &ctx).await, RPRT_EINVAL.to_vec());
    }

    #[tokio::test]
    async fn dual_vfo_get_vfo_info_still_reports_literal_physical_vfo() {
        // Regression guard: a non-single-VFO face (e.g. the engine endpoint) keeps the
        // faithful dual-VFO view — `get_vfo_info VFOA` returns physical VFO A even when
        // operating on B, and real split is reported.
        let (ctx, _b, state) = ctx_with(FacePermissions::from_tokens(&["read", "write"]));
        operating_on_b(&state);
        assert_eq!(
            reply_of("+\\get_vfo_info VFOA", &ctx).await,
            b"get_vfo_info: VFOA\nFreq: 14035000\nMode: CW\nWidth: 2400\nSplit: 1\nSatMode: 0\nRPRT 0\n"
                .to_vec()
        );
        // And VFOB explicitly is still addressable.
        assert_eq!(
            reply_of("+\\get_vfo_info VFOB", &ctx).await,
            b"get_vfo_info: VFOB\nFreq: 14074000\nMode: USB\nWidth: 2400\nSplit: 1\nSatMode: 0\nRPRT 0\n"
                .to_vec()
        );
    }
}
