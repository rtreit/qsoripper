//! The single-owner radio task: transport framing + reply matching, and the per-endpoint
//! priority scheduler that serializes every backend operation.
//!
//! Two cooperating layers:
//! * [`run_transport`] owns the byte transport. It writes one command at a time, matches
//!   solicited replies by verb, and routes every unsolicited frame to the backend's
//!   `parse_event` (native push). It exposes [`RadioLink`].
//! * [`spawn_scheduler`] owns per-session FIFO queues and selects the next backend
//!   operation by priority across the ready heads, never reordering one endpoint's stream.

use std::collections::VecDeque;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::{mpsc, oneshot};
use tokio::time::Instant;

use crate::backend::{BackendError, Framing, RadioBackend};
use crate::events::enable_native_push;
use crate::model::{RadioEventSource, StateMutation};
use crate::state::StateHandle;

/// Default per-command reply timeout.
const REPLY_TIMEOUT: Duration = Duration::from_millis(1_000);

/// Priority class for scheduling across endpoints. Lower discriminant wins.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Priority {
    /// PTT (keying) — always preempts everything else at selection time.
    Ptt = 0,
    /// Interactive client writes (frequency, mode, split).
    Write = 1,
    /// Client reads and passthrough.
    Read = 2,
    /// Background baseline poll.
    Poll = 3,
}

/// What the transport should expect after writing a command.
#[derive(Debug, Clone)]
pub(crate) enum Expect {
    /// A reply whose frame begins with one of these verb prefixes (Kenwood/Yaesu, where
    /// unsolicited push frames can interleave and must be told apart by verb).
    Reply(Vec<Vec<u8>>),
    /// The next `n` frames, concatenated (verb-less line protocols like `rigctld`, which
    /// have no unsolicited frames so the next lines are unambiguously the reply).
    Lines(usize),
    /// No reply (set-and-forget write).
    NoReply,
}

/// How a pending command recognizes its reply.
enum Matcher {
    Verb(Vec<Vec<u8>>),
    Lines { remaining: usize, acc: Vec<u8> },
}

/// The reply channel a queued command is answered on.
type ReplyTx = oneshot::Sender<Result<Vec<u8>, BackendError>>;

/// A raw command queued to the transport task.
pub(crate) struct RawCommand {
    bytes: Vec<u8>,
    expect: Expect,
    reply: ReplyTx,
}

/// A clonable handle backends use to submit raw bytes to the serialized transport.
#[derive(Clone)]
pub(crate) struct RadioLink {
    tx: mpsc::Sender<RawCommand>,
}

impl RadioLink {
    /// Submit raw bytes and await the matching reply (or an empty vec for `NoReply`).
    pub(crate) async fn submit(
        &self,
        bytes: Vec<u8>,
        expect: Expect,
    ) -> Result<Vec<u8>, BackendError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(RawCommand {
                bytes,
                expect,
                reply,
            })
            .await
            .map_err(|_| BackendError::Transport("transport task gone".into()))?;
        rx.await
            .map_err(|_| BackendError::Transport("transport dropped reply".into()))?
    }
}

/// Create a transport command channel and its [`RadioLink`].
pub(crate) fn link_channel() -> (RadioLink, mpsc::Receiver<RawCommand>) {
    let (tx, rx) = mpsc::channel(64);
    (RadioLink { tx }, rx)
}

/// A [`RadioLink`] whose transport is never spawned (loopback backend never submits).
#[cfg(test)]
pub(crate) fn detached_link() -> RadioLink {
    let (link, _rx) = link_channel();
    link
}

/// Split a byte stream into frames according to `framing`.
struct Framer {
    framing: Framing,
    buf: Vec<u8>,
}

impl Framer {
    fn new(framing: Framing) -> Self {
        Framer {
            framing,
            buf: Vec::with_capacity(64),
        }
    }

    fn delimiter(&self) -> u8 {
        match self.framing {
            Framing::SemicolonTerminated => b';',
            Framing::LineTerminated => b'\n',
            Framing::CiV => 0xFD,
        }
    }

    /// Feed bytes; return any complete frames (delimiter included).
    fn push(&mut self, bytes: &[u8]) -> Vec<Vec<u8>> {
        let delim = self.delimiter();
        let mut frames = Vec::new();
        for &b in bytes {
            // Bound the partial-frame buffer. A radio (or a noisy line) that streams bytes
            // without ever producing the delimiter must not grow this buffer without limit.
            // Drop the malformed partial frame and resynchronize on the next delimiter.
            if self.buf.len() >= MAX_RADIO_FRAME_LEN {
                tracing::warn!(
                    "radio frame exceeded {MAX_RADIO_FRAME_LEN} bytes without a delimiter; \
                     discarding partial frame"
                );
                self.buf.clear();
            }
            self.buf.push(b);
            if b == delim {
                frames.push(std::mem::take(&mut self.buf));
            }
        }
        frames
    }
}

/// Maximum bytes buffered for a single in-progress radio frame before the partial frame is
/// discarded. Real CAT frames are tens of bytes; this only bounds a stuck or noisy link.
const MAX_RADIO_FRAME_LEN: usize = 4096;

fn frame_matches(frame: &[u8], verbs: &[Vec<u8>]) -> bool {
    verbs.iter().any(|v| frame.starts_with(v))
}

/// Why the byte transport stopped, and whether the link should reconnect.
pub(crate) enum TransportOutcome {
    /// The command channel closed (every [`RadioLink`] was dropped): the daemon is shutting
    /// down, so the supervisor must stop and not reopen the radio.
    Shutdown,
    /// The byte transport closed or errored (serial unplug, radio power-cycle, write error).
    /// The command receiver is handed back so the supervisor can reopen and resume serving
    /// queued commands without dropping the rest of the daemon.
    Disconnected(mpsc::Receiver<RawCommand>),
}

/// Internal reason a [`run_transport`] loop exited, before the receiver is reattached.
enum ExitReason {
    Shutdown,
    Disconnected,
}

/// Default backoff before the first reconnect attempt after a transport drop.
pub(crate) const RECONNECT_INITIAL: Duration = Duration::from_millis(500);
/// Ceiling for the exponential reconnect backoff.
pub(crate) const RECONNECT_MAX: Duration = Duration::from_secs(5);

/// Run the transport task to completion (until the stream closes or errors).
///
/// `transport` is any duplex byte stream (a serial port, a TCP socket, or an in-memory
/// duplex in tests). Reply matching keeps exactly one solicited command in flight.
#[allow(clippy::too_many_lines)] // The transport read/write/match loop is one cohesive unit.
pub(crate) async fn run_transport<T>(
    transport: T,
    backend: Arc<dyn RadioBackend>,
    state: StateHandle,
    mut raw_rx: mpsc::Receiver<RawCommand>,
) -> TransportOutcome
where
    T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Send + 'static,
{
    let framing = backend.capabilities().framing;
    let (mut reader, mut writer) = tokio::io::split(transport);

    // Reader task: frame the input and forward frames.
    let (frame_tx, mut frame_rx) = mpsc::channel::<Vec<u8>>(256);
    let reader_task = tokio::spawn(async move {
        let mut framer = Framer::new(framing);
        let mut chunk = [0u8; 512];
        loop {
            match reader.read(&mut chunk).await {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    let slice = chunk.get(..n).unwrap_or(&[]);
                    for frame in framer.push(slice) {
                        if frame_tx.send(frame).await.is_err() {
                            return;
                        }
                    }
                }
            }
        }
    });

    let mut pending: Option<(Matcher, ReplyTx, Instant)> = None;
    let reason: ExitReason;

    loop {
        if pending.is_none() {
            tokio::select! {
                cmd = raw_rx.recv() => {
                    let Some(cmd) = cmd else { reason = ExitReason::Shutdown; break };
                    if let Err(e) = writer.write_all(&cmd.bytes).await {
                        let _ = cmd.reply.send(Err(BackendError::Transport(e.to_string())));
                        reason = ExitReason::Disconnected;
                        break;
                    }
                    let _ = writer.flush().await;
                    tracing::trace!(tx = %String::from_utf8_lossy(&cmd.bytes), "radio tx");
                    match cmd.expect {
                        Expect::NoReply => {
                            let _ = cmd.reply.send(Ok(Vec::new()));
                        }
                        Expect::Reply(verbs) => {
                            pending = Some((
                                Matcher::Verb(verbs),
                                cmd.reply,
                                Instant::now() + REPLY_TIMEOUT,
                            ));
                        }
                        Expect::Lines(n) => {
                            if n == 0 {
                                let _ = cmd.reply.send(Ok(Vec::new()));
                            } else {
                                pending = Some((
                                    Matcher::Lines { remaining: n, acc: Vec::new() },
                                    cmd.reply,
                                    Instant::now() + REPLY_TIMEOUT,
                                ));
                            }
                        }
                    }
                }
                frame = frame_rx.recv() => {
                    let Some(frame) = frame else { reason = ExitReason::Disconnected; break };
                    tracing::trace!(rx = %String::from_utf8_lossy(&frame), "radio rx (idle)");
                    route_event(&backend, &state, &frame);
                }
            }
        } else {
            let Some((_, _, deadline)) = pending.as_ref() else {
                continue;
            };
            let deadline = *deadline;
            let recv = tokio::time::timeout_at(deadline, frame_rx.recv()).await;
            match recv {
                Err(_elapsed) => {
                    if let Some((matcher, reply, _)) = pending.take() {
                        if let Matcher::Verb(verbs) = &matcher {
                            let awaited: Vec<String> = verbs
                                .iter()
                                .map(|v| String::from_utf8_lossy(v).into_owned())
                                .collect();
                            tracing::warn!(?awaited, "radio reply timed out");
                        } else {
                            tracing::warn!("radio reply (lines) timed out");
                        }
                        let _ = reply.send(Err(BackendError::Timeout));
                    }
                }
                Ok(None) => {
                    reason = ExitReason::Disconnected;
                    break;
                }
                Ok(Some(frame)) => {
                    tracing::trace!(rx = %String::from_utf8_lossy(&frame), "radio rx (pending)");
                    pending = match pending.take() {
                        Some((Matcher::Verb(verbs), reply, deadline)) => {
                            if frame_matches(&frame, &verbs) {
                                let _ = reply.send(Ok(frame));
                                None
                            } else {
                                route_event(&backend, &state, &frame);
                                Some((Matcher::Verb(verbs), reply, deadline))
                            }
                        }
                        Some((Matcher::Lines { remaining, mut acc }, reply, deadline)) => {
                            acc.extend_from_slice(&frame);
                            let left = remaining.saturating_sub(1);
                            // A rigctld error is a complete one-line response even when the
                            // successful command shape has multiple lines (`m`, `s`, `x`).
                            // Complete immediately so optional capability probes do not wait
                            // for a line that will never arrive.
                            if left == 0 || frame.starts_with(b"RPRT ") {
                                let _ = reply.send(Ok(acc));
                                None
                            } else {
                                Some((
                                    Matcher::Lines {
                                        remaining: left,
                                        acc,
                                    },
                                    reply,
                                    deadline,
                                ))
                            }
                        }
                        None => None,
                    };
                }
            }
        }
    }

    if let Some((_, reply, _)) = pending.take() {
        let _ = reply.send(Err(BackendError::Transport("transport closed".into())));
    }
    reader_task.abort();
    match reason {
        ExitReason::Shutdown => TransportOutcome::Shutdown,
        ExitReason::Disconnected => TransportOutcome::Disconnected(raw_rx),
    }
}

/// Supervise a radio transport: run it, and when it drops (serial unplug, radio power-cycle,
/// write error) reopen it with capped exponential backoff and resume serving the same command
/// queue. Without this, a single transport hiccup would leave every client (HDSDR/OmniRig,
/// N1MM, WSJT-X, Log4OM, the engine) connected to a dead radio link until the whole daemon was
/// restarted. The loop ends only when the command channel closes (daemon shutdown).
///
/// `first` is the already-open transport from startup. `reopen` produces a fresh transport of
/// the same kind on each reconnect. When `push_link` is `Some`, the radio's native push stream
/// is re-armed after every (re)connect, because a power-cycled radio forgets its auto-info
/// state (design §8.4: "At startup and on reconnect").
#[expect(
    clippy::too_many_arguments,
    reason = "transport, queue, backend, state, push-link, reopen, and two backoff bounds are all distinct supervision inputs"
)]
pub(crate) async fn run_transport_supervised<T, F, Fut>(
    first: T,
    mut raw_rx: mpsc::Receiver<RawCommand>,
    backend: Arc<dyn RadioBackend>,
    state: StateHandle,
    push_link: Option<RadioLink>,
    mut reopen: F,
    backoff_initial: Duration,
    backoff_max: Duration,
) where
    T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Send + 'static,
    F: FnMut() -> Fut + Send + 'static,
    Fut: Future<Output = Result<T, BackendError>> + Send,
{
    let mut transport = Some(first);
    let mut backoff = backoff_initial;
    loop {
        let t = match transport.take() {
            Some(t) => t,
            None => match reopen().await {
                Ok(t) => {
                    tracing::info!("radio transport reconnected");
                    backoff = backoff_initial;
                    t
                }
                Err(error) => {
                    tracing::warn!(%error, ?backoff, "radio reopen failed; retrying");
                    tokio::time::sleep(backoff).await;
                    backoff = (backoff * 2).min(backoff_max);
                    continue;
                }
            },
        };

        // Re-arm the radio's native push now that a transport is live. This is spawned because
        // it submits through the same command queue that `run_transport` (started just below)
        // services; awaiting it here would deadlock against a not-yet-running transport.
        if let Some(link) = &push_link {
            let backend = backend.clone();
            let link = link.clone();
            tokio::spawn(async move {
                enable_native_push(&backend, &link).await;
            });
        }

        match run_transport(t, backend.clone(), state.clone(), raw_rx).await {
            TransportOutcome::Shutdown => return,
            TransportOutcome::Disconnected(rx) => {
                raw_rx = rx;
                tracing::warn!("radio transport closed; reconnecting");
                tokio::time::sleep(backoff).await;
            }
        }
    }
}

/// Route an unsolicited frame into the universal state as a native push event.
///
/// Modeled frames update the snapshot and broadcast a coalesced change. Frames the backend
/// does not model are relayed verbatim on the same ordered event bus so native pass-through
/// endpoints (which consume the CAT stream directly) keep features like the radio's noise
/// blanker and front-panel changes in sync.
fn route_event(backend: &Arc<dyn RadioBackend>, state: &StateHandle, frame: &[u8]) {
    if backend.record_event(frame, state, RadioEventSource::NativePush) {
        // The frame was modeled: the snapshot is updated and a coalesced `Change` is
        // broadcast for virtualizing endpoints. Also relay the verbatim frame so transparent
        // mirror endpoints (ARCP-590) track the radio's real CAT stream instead of a synthesis.
        state.record_raw_native(frame);
    } else {
        tracing::trace!(
            frame = %String::from_utf8_lossy(frame),
            "relaying unmodeled unsolicited radio frame to native pass-through endpoints"
        );
        state.record_raw(frame);
    }
}

// --- Operation scheduler -------------------------------------------------------------

/// The kind of backend operation a client session requests.
pub(crate) enum OpKind {
    /// Run one baseline poll cycle.
    Poll,
    /// Apply a modeled mutation.
    Apply(StateMutation),
    /// Forward a raw native command (passthrough), returning the raw reply.
    Passthrough(Vec<u8>),
}

struct Operation {
    session_id: u64,
    priority: Priority,
    kind: OpKind,
    reply: oneshot::Sender<Result<Vec<u8>, BackendError>>,
}

/// A clonable handle client sessions and the poller use to submit operations.
#[derive(Clone)]
pub(crate) struct RadioHandle {
    tx: mpsc::Sender<Operation>,
}

impl RadioHandle {
    /// Submit an operation tagged with the calling session's id and priority.
    pub(crate) async fn submit(
        &self,
        session_id: u64,
        priority: Priority,
        kind: OpKind,
    ) -> Result<Vec<u8>, BackendError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(Operation {
                session_id,
                priority,
                kind,
                reply,
            })
            .await
            .map_err(|_| BackendError::Transport("scheduler gone".into()))?;
        rx.await
            .map_err(|_| BackendError::Transport("scheduler dropped reply".into()))?
    }
}

/// Spawn the per-session priority scheduler. Returns a clonable [`RadioHandle`].
pub(crate) fn spawn_scheduler(
    backend: Arc<dyn RadioBackend>,
    link: RadioLink,
    state: StateHandle,
) -> RadioHandle {
    let (tx, mut rx) = mpsc::channel::<Operation>(128);
    tokio::spawn(async move {
        let mut queues: Vec<(u64, VecDeque<Operation>)> = Vec::new();
        loop {
            if queues.iter().all(|(_, q)| q.is_empty()) {
                match rx.recv().await {
                    Some(op) => enqueue(&mut queues, op),
                    None => break,
                }
            }
            while let Ok(op) = rx.try_recv() {
                enqueue(&mut queues, op);
            }
            let Some(op) = select_next(&mut queues) else {
                continue;
            };
            let result = execute(&backend, &link, &state, &op.kind).await;
            let _ = op.reply.send(result);
        }
    });
    RadioHandle { tx }
}

fn enqueue(queues: &mut Vec<(u64, VecDeque<Operation>)>, op: Operation) {
    if let Some((_, q)) = queues.iter_mut().find(|(id, _)| *id == op.session_id) {
        q.push_back(op);
    } else {
        let mut q = VecDeque::new();
        let session_id = op.session_id;
        q.push_back(op);
        queues.push((session_id, q));
    }
}

/// Pick the highest-priority ready head across all per-session queues (FIFO within a session).
fn select_next(queues: &mut [(u64, VecDeque<Operation>)]) -> Option<Operation> {
    let mut best: Option<usize> = None;
    let mut best_priority = Priority::Poll;
    for (idx, (_, q)) in queues.iter().enumerate() {
        if let Some(head) = q.front() {
            if best.is_none() || head.priority < best_priority {
                best = Some(idx);
                best_priority = head.priority;
            }
        }
    }
    best.and_then(|idx| queues.get_mut(idx).and_then(|(_, q)| q.pop_front()))
}

async fn execute(
    backend: &Arc<dyn RadioBackend>,
    link: &RadioLink,
    state: &StateHandle,
    kind: &OpKind,
) -> Result<Vec<u8>, BackendError> {
    match kind {
        OpKind::Poll => backend.poll(link, state).await.map(|()| Vec::new()),
        OpKind::Apply(m) => backend.apply(*m, link, state).await.map(|()| Vec::new()),
        OpKind::Passthrough(bytes) => backend.passthrough(bytes, link).await,
    }
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::similar_names
)]
mod tests {
    use super::*;
    use crate::backend::kenwood::ts590::Ts590Backend;
    use crate::backend::rigctld::RigctldBackend;
    use crate::model::{Mode, Vfo};
    use crate::state::RadioEvent;

    #[test]
    fn framer_splits_on_semicolons() {
        let mut framer = Framer::new(Framing::SemicolonTerminated);
        let frames = framer.push(b"FA00007030000;MD3;");
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0], b"FA00007030000;");
        assert_eq!(frames[1], b"MD3;");
    }

    #[test]
    fn framer_holds_partial_frames() {
        let mut framer = Framer::new(Framing::SemicolonTerminated);
        assert!(framer.push(b"FA0000703").is_empty());
        let frames = framer.push(b"0000;");
        assert_eq!(frames, vec![b"FA00007030000;".to_vec()]);
    }

    #[test]
    fn framer_discards_overlong_partial_frame_and_resyncs() {
        let mut framer = Framer::new(Framing::SemicolonTerminated);
        // Stream more than the cap without ever sending a delimiter.
        let junk = vec![b'X'; MAX_RADIO_FRAME_LEN + 64];
        assert!(framer.push(&junk).is_empty());
        // The partial buffer must have been bounded, not grown unbounded.
        assert!(framer.buf.len() <= MAX_RADIO_FRAME_LEN);
        // Flush whatever junk remains with a delimiter, then a clean frame parses correctly.
        framer.push(b";");
        let frames = framer.push(b"FA00007030000;");
        assert_eq!(frames, vec![b"FA00007030000;".to_vec()]);
    }

    #[test]
    fn verb_matching_uses_prefix() {
        assert!(frame_matches(b"FA00007030000;", &[b"FA".to_vec()]));
        assert!(!frame_matches(b"MD3;", &[b"FA".to_vec()]));
    }

    #[test]
    fn route_event_records_full_ts590_if_status() {
        let backend: Arc<dyn RadioBackend> = Arc::new(Ts590Backend::new());
        let state = StateHandle::new();

        route_event(&backend, &state, b"IF000140343201234-0000012345121019999;");

        let snap = state.snapshot();
        assert_eq!(snap.rx_vfo, Vfo::B);
        assert_eq!(snap.vfo(Vfo::B).freq_hz, 14_034_320);
        assert_eq!(snap.vfo(Vfo::B).mode, Mode::Usb);
        assert!(snap.ptt);
        assert!(snap.split);
    }

    #[test]
    fn route_event_relays_modeled_frame_as_raw_native_for_mirror_endpoints() {
        let backend: Arc<dyn RadioBackend> = Arc::new(Ts590Backend::new());
        let state = StateHandle::new();
        let mut rx = state.subscribe();

        route_event(&backend, &state, b"FA00014035000;");

        // A modeled frame must broadcast BOTH the coalesced Change (for virtualizing endpoints)
        // and the verbatim RawNative (for transparent mirror endpoints like ARCP-590).
        let mut saw_change = false;
        let mut saw_raw_native = false;
        while let Ok(evt) = rx.try_recv() {
            match evt {
                RadioEvent::Change(_) => saw_change = true,
                RadioEvent::RawNative(bytes) => {
                    assert_eq!(&*bytes, b"FA00014035000;");
                    saw_raw_native = true;
                }
                RadioEvent::Raw(_) => panic!("a modeled frame must not broadcast as Raw"),
            }
        }
        assert!(
            saw_change,
            "modeled frame must broadcast a coalesced Change"
        );
        assert!(
            saw_raw_native,
            "modeled frame must also relay verbatim as RawNative"
        );
    }

    #[test]
    fn route_event_relays_unmodeled_frame_as_raw_only() {
        let backend: Arc<dyn RadioBackend> = Arc::new(Ts590Backend::new());
        let state = StateHandle::new();
        let mut rx = state.subscribe();

        route_event(&backend, &state, b"NB1;");

        let evt = rx.try_recv().expect("one event");
        assert!(
            matches!(&evt, RadioEvent::Raw(bytes) if &**bytes == b"NB1;"),
            "an unmodeled frame must relay as Raw only, got {evt:?}"
        );
        assert!(
            rx.try_recv().is_err(),
            "no second event for an unmodeled frame"
        );
    }

    #[test]
    fn priority_orders_ptt_first() {
        assert!(Priority::Ptt < Priority::Write);
        assert!(Priority::Write < Priority::Read);
        assert!(Priority::Read < Priority::Poll);
    }

    #[tokio::test]
    async fn unsolicited_frames_do_not_extend_reply_deadline() {
        let backend: Arc<dyn RadioBackend> = Arc::new(Ts590Backend::new());
        let state = StateHandle::new();
        let (link, raw_rx) = link_channel();
        let (radio, server) = tokio::io::duplex(1024);

        let transport_task = tokio::spawn(run_transport(server, backend, state, raw_rx));
        let radio_task = tokio::spawn(async move {
            let (mut rd, mut wr) = tokio::io::split(radio);
            let mut command = [0u8; 3];
            rd.read_exact(&mut command)
                .await
                .expect("transport should send a command");

            loop {
                wr.write_all(b"NB0;")
                    .await
                    .expect("transport should accept unsolicited frames");
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        });

        let result = tokio::time::timeout(
            REPLY_TIMEOUT + Duration::from_millis(300),
            link.submit(b"FA;".to_vec(), Expect::Reply(vec![b"FA".to_vec()])),
        )
        .await
        .expect("an absolute reply deadline must not be extended by unsolicited frames");

        assert!(matches!(result, Err(BackendError::Timeout)));
        radio_task.abort();
        transport_task.abort();
    }

    #[tokio::test]
    async fn line_matcher_completes_multi_line_request_on_rprt_error() {
        let backend: Arc<dyn RadioBackend> = Arc::new(RigctldBackend::new("test", false));
        let state = StateHandle::new();
        let (link, raw_rx) = link_channel();
        let (radio, server) = tokio::io::duplex(128);
        let transport_task = tokio::spawn(run_transport(server, backend, state, raw_rx));

        let radio_task = tokio::spawn(async move {
            let (mut rd, mut wr) = tokio::io::split(radio);
            let mut command = [0u8; 2];
            rd.read_exact(&mut command).await.expect("read command");
            assert_eq!(&command, b"x\n");
            wr.write_all(b"RPRT -11\n").await.expect("write error");
        });

        let reply = tokio::time::timeout(
            Duration::from_millis(200),
            link.submit(b"x\n".to_vec(), Expect::Lines(2)),
        )
        .await
        .expect("RPRT should complete before the normal reply timeout")
        .expect("transport reply");
        assert_eq!(reply, b"RPRT -11\n");

        radio_task.await.expect("fake radio");
        transport_task.abort();
    }

    #[tokio::test]
    async fn supervisor_reconnects_after_transport_drop() {
        use crate::backend::kenwood::ts590::Ts590Backend;

        let backend: Arc<dyn RadioBackend> = Arc::new(Ts590Backend::new());
        let state = StateHandle::new();
        let (link, raw_rx) = link_channel();

        // First transport: a duplex whose client end we drop to force a disconnect.
        let (radio1, server1) = tokio::io::duplex(1024);
        // Second transport: handed out by `reopen` on the first reconnect.
        let (radio2, server2) = tokio::io::duplex(1024);

        let pending = Arc::new(tokio::sync::Mutex::new(Some(server2)));
        let pending_for_reopen = pending.clone();
        let reopen = move || {
            let pending = pending_for_reopen.clone();
            async move {
                pending
                    .lock()
                    .await
                    .take()
                    .ok_or_else(|| BackendError::Transport("no more transports".into()))
            }
        };

        tokio::spawn(run_transport_supervised(
            server1,
            raw_rx,
            backend.clone(),
            state.clone(),
            None,
            reopen,
            Duration::from_millis(10),
            Duration::from_millis(50),
        ));

        // Kill the first transport so the supervisor must reconnect.
        drop(radio1);

        // A fake radio on the reconnected transport answers an FA read.
        tokio::spawn(async move {
            let (mut rd, mut wr) = tokio::io::split(radio2);
            let mut buf = [0u8; 64];
            loop {
                let n = match rd.read(&mut buf).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => n,
                };
                if buf.get(..n).unwrap_or(&[]).starts_with(b"FA") {
                    let _ = wr.write_all(b"FA00014047470;").await;
                }
            }
        });

        // After reconnection, the same link must serve commands again.
        let reply = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if let Ok(reply) = link
                    .submit(b"FA;".to_vec(), Expect::Reply(vec![b"FA".to_vec()]))
                    .await
                {
                    return reply;
                }
                tokio::time::sleep(Duration::from_millis(15)).await;
            }
        })
        .await
        .expect("reconnected transport should answer an FA read");

        assert!(reply.starts_with(b"FA"), "reply should be an FA frame");
    }

    #[tokio::test]
    async fn supervisor_stops_when_command_channel_closes() {
        use crate::backend::kenwood::ts590::Ts590Backend;

        let backend: Arc<dyn RadioBackend> = Arc::new(Ts590Backend::new());
        let state = StateHandle::new();
        let (link, raw_rx) = link_channel();

        // Keep the radio side alive so the only way out is the command channel closing.
        let (radio1, server1) = tokio::io::duplex(1024);

        // `reopen` should never be called on this path.
        let reopen = || async {
            Err::<tokio::io::DuplexStream, _>(BackendError::Transport("unexpected reopen".into()))
        };

        let handle = tokio::spawn(run_transport_supervised(
            server1,
            raw_rx,
            backend,
            state,
            None,
            reopen,
            Duration::from_millis(10),
            Duration::from_millis(50),
        ));

        // Dropping the last link closes the command channel: the supervisor must exit.
        drop(link);

        tokio::time::timeout(Duration::from_secs(2), handle)
            .await
            .expect("supervisor should exit when the command channel closes")
            .expect("supervisor task should not panic");

        drop(radio1);
    }
}
