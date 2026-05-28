# Design: `qsoripper-cathub` multi-client CAT hub

> **Status:** Proposed. This document is the source of truth for implementing the hub. Once implementation lands, the contract sections graduate into `docs/architecture/` and this file becomes a historical record.
>
> **Audience:** An implementer (human or AI agent) with access to this repository, the Hamlib `rigctld` net protocol reference, the Kenwood TS-590 CAT command reference (including the `AI` auto-information command), the Kenwood ARCP-590 / ARHP-590 documentation, the WSJT-X / Hamlib rig-control documentation, the Icom CI-V reference, and the com0com virtual serial port documentation.

## 1. Problem

A modern ham radio station runs several pieces of software against one transceiver at the same time. A typical TS-590 station wants any combination of:

- A panadapter or SDR receiver such as HDSDR with click-to-tune on the waterfall (typically through OmniRig speaking the TS-2000 profile).
- A contest logger such as N1MM Logger+ driving the rig directly for band changes, mode switches, and CAT PTT (native TS-590 CAT).
- The manufacturer's own control software, **Kenwood ARCP-590**, which presents a full software faceplate for the radio: VFOs, modes, DSP and filter settings, the antenna tuner, meters, the CW/voice keyer, and the entire `EX` menu. ARCP-590 speaks the **native TS-590 CAT command set** and exercises a far larger command surface, and polls far more aggressively, than a logger does.
- A digital-mode application such as **WSJT-X**, which controls the rig through **Hamlib** — either Hamlib's built-in TS-590S/SG backend on a serial port or, more cleanly for a shared station, Hamlib's **NET rigctl** transport pointed at a `rigctld`-compatible TCP endpoint. WSJT-X sets frequency, mode, and CAT PTT during normal operation.
- The QsoRipper engine reading rig state for QSO enrichment (today via its `rigctld` provider).

Each of these wants its own CAT link to the radio. The radio exposes a single physical (or USB virtual) serial port. Only one host process can own that port at a time. The result is a forced choice between contest logging, panadapter tuning, manufacturer control, digital modes, and engine awareness. Worse, the clients have **incompatible expectations**: ARCP-590 and WSJT-X both want to *write* to the radio and both want PTT; ARCP-590 and any AI-aware client expect the radio to *push* unsolicited status updates; OmniRig speaks a different dialect (TS-2000) than the radio actually is (TS-590).

The existing workaround uses Hamlib's `rigctld` to multiplex the radio over TCP and a Python "safe bridge" to translate OmniRig's serial CAT into raw Hamlib calls. The full report is at <https://mtreit.com/misc/radio/ts590-vfob-rigbridge-fix-report.html>. That stack solves a real bug (the TS-590 oscillating between VFO A and VFO B because Hamlib's TS-2000 backend retargets VFOs on every status poll), but it is fragile in several ways:

- `rigctlcom` and the Python bridge each occupy a process and a com0com endpoint, and both must agree about which one owns the radio.
- The fix depends on never letting Hamlib invoke its VFO-targeting APIs on the TS-590. Any future Hamlib backend change can reintroduce the oscillation.
- Adding a second serial client (N1MM, ARCP-590, on its own virtual COM port) requires the bridge to multiplex serial faces, which the current Python script does not do.
- There is no story for the radio's native auto-information push (`AI2;`), so every client polls independently and the real-radio command rate scales with the number of clients.

This design replaces the bridge, `rigctlcom`, and `rigctld` with one Rust daemon that owns the radio and fans it out to as many clients as the operator needs, regardless of whether a given client speaks native TS-590 CAT, a foreign Kenwood dialect (TS-2000), or the Hamlib net protocol.

## 2. Goals

- One daemon owns the real radio port. All client traffic is serialized through it.
- Multiple client apps run simultaneously against one radio with full read and write parity. This explicitly includes a heavy native controller (ARCP-590), one or more loggers (N1MM), a foreign-dialect panadapter client (OmniRig/HDSDR), a Hamlib-net digital-mode client (WSJT-X), and the QsoRipper engine, all at once.
- State changes from any client — or from the radio's front panel — propagate to all the other clients through a shared in-memory cache, so HDSDR's waterfall follows N1MM's band changes, ARCP-590 reflects a knob turn made on the physical radio, and the engine's view stays current.
- The radio path never invokes Hamlib's VFO-targeting APIs. The oscillation bug is structurally impossible.
- The daemon is radio-agnostic by construction. Adding a new transceiver model is a single new `RadioBackend` implementation with no changes to dialects, faces, or the state cache.
- The daemon is client-agnostic by construction. Adding a new client app means picking an existing CAT dialect, or writing one new `ClientDialect` implementation, or pointing the client at the Hamlib net face.
- The Hamlib net face supports the **read and write** subset that real Hamlib clients use, not reads only, so WSJT-X (set frequency, set mode, set PTT, plus `\dump_state` and `\chk_vfo` at connect time) works unmodified.
- Native rich controllers such as ARCP-590 work without the daemon having to model every CAT command. Commands the universal state does not model are forwarded transparently to the radio through the same serialized path (passthrough), with state-mutating commands additionally captured into the universal state.
- The radio's native auto-information mode (`AI2;`) is owned centrally by the daemon and fanned out to the clients that want it, so a single real-radio AI stream feeds every AI-aware client without each client polling.
- The QsoRipper engine continues to consume rig state without code change by speaking the Hamlib net protocol to a built-in server in the daemon.

## 3. Non-goals

- Replacing OmniRig, HDSDR, N1MM, ARCP-590, WSJT-X, or any other client app. The daemon brokers them; it does not reimplement them.
- Reimplementing Hamlib in full. The daemon implements only the subset of the `rigctld` net protocol that QsoRipper, WSJT-X, and similar clients actually use.
- Modeling the full TS-590 CAT command set in the universal state. The state models the hot, cross-client fields (frequency, mode, split, RIT/XIT, S-meter, power, PTT). Everything else (the `EX` menu, filter/DSP detail, keyer memories, tuner state) flows through passthrough.
- Supporting every transceiver in v1. v1 ships the Kenwood TS-590. The Icom, Yaesu, and FlexRadio backends are designed for but not shipped in v1.
- Remote operation across hosts. The daemon binds loopback only. Cross-host operation is a future extension (ARHP-590-style remote heads are explicitly out of scope for v1).
- A GUI control panel. v1 ships configuration via TOML and observability via structured logs.

## 4. Current architecture, for reference

```
HDSDR
  |
  v
OmniRig (TS-2000 profile, COM11, 115200 baud, 500 ms poll)
  |
  v
com0com pair (COM11 <-> COM10)
  |
  v
safe_ts590_omnirig_bridge.py on COM10
  |
  v
rigctld (TCP 127.0.0.1:4532)
  |
  v
TS-590 on COM3 (single owner)
```

N1MM, ARCP-590, and WSJT-X cannot insert themselves anywhere in this chain without taking COM3 away from `rigctld`. The QsoRipper engine consumes state by speaking the Hamlib net protocol to `rigctld`.

## 5. Proposed architecture

The daemon (`qsoripper-cathub`) is one Rust binary. It owns the radio port. It exposes several client faces. Each client face is either a virtual COM port (for client apps that expect serial CAT) or a TCP server (for clients that speak the Hamlib net protocol).

```
                                                                 +-----------------+
HDSDR --> OmniRig (TS-2000) --> com0com --> COM_OMNIRIG_SIDE ---->|                 |
                                                                 |                 |
N1MM (native TS-590) ------------> com0com --> COM_N1MM_SIDE ---->|                 |
                                                                 |   qsoripper-    |
ARCP-590 (native TS-590) --------> com0com --> COM_ARCP_SIDE ---->|     cathub      |--> COM3 --> TS-590
                                                                 |   (Rust)        |
WSJT-X -- Hamlib NET rigctl ------> 127.0.0.1:4533 (rw) -------->|                 |
                                                                 |  shared state   |
QsoRipper engine -- Hamlib net ---> 127.0.0.1:4532 (ro) -------->|  cache + AI     |
                                                                 |  fan-out        |
                                                                 +-----------------+
```

Each serial client gets its own virtual serial pair on the operator's machine. OmniRig, N1MM, and ARCP-590 each bind one end of a dedicated pair; the daemon binds the other end. WSJT-X and the QsoRipper engine connect over the loopback Hamlib net face; because they have different privilege needs (WSJT-X writes and keys PTT, the engine only reads) and a single TCP port cannot express per-client permissions, the daemon exposes two loopback Hamlib listeners — a read-only one for the engine and a write/PTT one for WSJT-X (each accepts multiple connections). Neither client app needs any reconfiguration beyond pointing at its assigned virtual COM port or Hamlib endpoint.

On Windows the virtual serial pairs are com0com pairs. On Linux the equivalent is a PTY pair (for example via `socat -d -d pty,raw,echo=0 pty,raw,echo=0` or `tty0tty`); the daemon binds one node and the client binds the other. The daemon treats both identically — it opens a serial-style endpoint by path and does not depend on the pairing mechanism.

## 6. Component layout

The crate lives at `src/rust/qsoripper-cathub/` and is added to the workspace members in `src/rust/Cargo.toml`. Internal modules:

- `radio` owns the radio transport. v1 supports serial; v2 adds TCP for FlexRadio. The module exposes an async `submit(cmd, priority) -> reply` API. All access is serialized through one tokio task. Scheduling preserves **per-face FIFO ordering** and prioritizes only between the ready heads of the per-face queues: a face's commands always reach the radio in the order that face submitted them, while across faces PTT outranks interactive writes, which outrank reads/passthrough, which outrank the background baseline poll. A read issued by a face after that face's own write is therefore guaranteed to observe its own write. This avoids reordering a single client's request stream (which serial CAT clients assume) while still keeping an operator's keying and tuning ahead of another client's bulk reads. Framing is configurable per backend: semicolon-terminated for Kenwood and Yaesu, `0xFD`-terminated for Icom CI-V. The radio task also demultiplexes **unsolicited** frames (see `ai` below) from solicited replies via an explicit frame matcher (§8.4). Reconnect on disconnect is automatic and idempotent.
- `backend` defines the `RadioBackend` trait. Implementations live in `backend/kenwood/ts590.rs` and `backend/loopback.rs` for v1, with `backend/icom/ci_v.rs`, `backend/yaesu/ft991a.rs`, and `backend/flex/smartsdr.rs` as v2 modules. The trait is the only seam between the radio and everything else.
- `state` is the universal in-memory snapshot. It must be rich enough for split- and VFO-aware clients (Hamlib `v`/`V`/`s`/`S`, WSJT-X split, N1MM, ARCP-590), so v1 models, per VFO where applicable: per-VFO frequency (A and B), per-VFO mode and passband, the active RX VFO, the active TX VFO, split enabled plus split TX VFO/frequency, RIT and XIT enabled plus offsets, S-meter, power, and PTT owner. Each field carries `last_polled`, `last_set`, and an `ai_covered` flag (see `poller`/§8.4). Mutations from any path go through this layer. Backends populate it. Dialects read and mutate it. The Hamlib net face reads and mutates it. The `ai` module updates it from unsolicited radio frames. None of those touch the others directly. The state layer also exposes a **change-notification broadcast** (a `tokio::sync::broadcast` channel) so faces can push updates to AI-aware clients.
- `poller` is a single background task that drives a low-rate baseline poll through the active backend into `state`. The baseline cadence is 200 ms by default. AI back-off is **per field, not blanket**: only fields whose `ai_covered` flag is set (those the radio actually reports via unsolicited `AI2` frames — typically frequency, mode, split) degrade to a slow liveness/heartbeat poll while AI is active. Fields the radio does *not* push spontaneously (S-meter, power, tuner/DSP detail) keep their own polling cadence regardless of AI state. The cache TTL per field controls when client-driven reads piggyback on the next baseline cycle versus serve directly from cache.
- `dialect` defines the `ClientDialect` trait. Implementations: `dialect/kenwood/ts590.rs` for N1MM and ARCP-590 (native pass-through with state caching and unmodeled-command passthrough), `dialect/kenwood/ts2000.rs` for OmniRig (translator). Dialects only touch the universal state and the radio passthrough channel; they never call a specific backend directly. This is what guarantees any dialect serves any backend. A dialect can both answer a request *and* emit asynchronous frames to its client (see AI fan-out, §8.4).
- `serial_face` is the generic virtual-COM listener. Each configured face binds one COM/PTY endpoint and routes its byte stream into a configured dialect. Faces run concurrently as independent tasks. Two faces serving the same dialect against the same backend is supported and expected (one for HDSDR, one for some other panadapter app; or N1MM and ARCP-590 both on the native TS-590 dialect). Each face owns an **outbound queue** so the daemon can push unsolicited frames (AI updates) to that client independently of request/response traffic.
- `hamlib_net` is the minimal `rigctld`-compatible TCP server. It binds `127.0.0.1:4532` by default and accepts multiple simultaneous connections (WSJT-X and the engine at once). It supports the **read and write** subset that real Hamlib clients use:
  - reads: `f` (get freq), `m` (get mode), `v` (get vfo), `s` (get split), `t` (get ptt), and `\get_powerstat`;
  - writes: `F` (set freq), `M` (set mode), `V` (set vfo), `S` (set split), `T` (set ptt);
  - protocol handshake: `\dump_state` and `\chk_vfo`, which WSJT-X and other Hamlib clients issue on connect to learn rig capabilities;
  - raw `w`/`W` (write/read CAT) passthrough for escape hatches.
  Every command is implemented against the universal state and the backend capability table, so it works for any backend without changes. Response formats — the `\dump_state` capability dump, `\chk_vfo`, the `M <mode> <passband>` shape, split replies, and the exact `RPRT <code>` lines — must match what real `rigctld` emits closely enough for unforgiving clients like WSJT-X; the dialect is validated against golden transcripts captured from a real `rigctld` (§10.1). Unsupported operations return the specific `RPRT` code rigctld uses (for example `RPRT -11`, "not available") rather than guessing.
  Because a single TCP endpoint cannot express per-client permissions and any local process could connect, write and PTT are **disabled by default** and the daemon supports more than one listener: a read-only endpoint for the QsoRipper engine and a separate write/PTT-enabled endpoint for WSJT-X. Each listener carries its own permission set (see `permissions`). The existing `RigctldProvider` in `src/rust/qsoripper-core/src/rig_control/rigctld.rs` connects to the read-only endpoint with no code change; WSJT-X connects to the write/PTT endpoint as a standard Hamlib NET rigctl rig.
- `ai` owns the radio's auto-information mode centrally. It sets `AI2;` on the real radio at startup, parses unsolicited frames the radio pushes (typically `IF;` responses and single-field updates) into universal-state mutations, and feeds the state change-notification broadcast. It also **virtualizes** the `AI` command per client: a client that sends `AI2;`/`AI1;`/`AI0;` toggles only its *own* face's push subscription; it never changes the real radio's AI state. This prevents clients from fighting over the single physical AI setting.
- `passthrough` handles native CAT commands the universal state does not model (the bulk of ARCP-590's traffic: `EX` menu reads/writes, filter/DSP queries, tuner, keyer). The dialect forwards the raw command through the serialized radio task and returns the radio's reply verbatim. Reads may be served from a short-TTL cache **keyed by the full normalized command** (not just a prefix — `EX` and similar commands carry parameters that select different values), and the cache is **invalidated by command family** whenever a write in that family is forwarded. Commands whose mutability or side effects are unknown are not cached. Passthrough writes that happen to mutate a modeled field also update the universal state so other clients stay consistent. Passthrough requires that the face's dialect native command set matches the active backend's native command set (see §7 caveat); it is not portable across radio families.
- `ptt` enforces PTT ownership and arbitration. Routed through the state mutation API. See §8.5.
- `permissions` defines a per-face (and per-Hamlib-listener) capability set driven by a **per-dialect command classification table** that tags each command as one of: modeled read, modeled write, passthrough read, transient write, PTT/TX-affecting write, persistent/config write, or denied/unknown. The coarse face flags — `read`, `write`, `ptt`, and `config_write` — gate those classes (`config_write` gates `EX`-menu and other persistent-setting writes). Unknown passthrough writes default to denied unless the face explicitly opts into unsafe full control. This lets the operator give ARCP-590 full control while restricting a panadapter face to read-only, and prevents a stray native command from keying TX or rewriting menu settings from an under-privileged face.
- `config` loads TOML from `%APPDATA%\QsoRipper\cathub.toml` on Windows and `$XDG_CONFIG_HOME/qsoripper/cathub.toml` on Linux. A `--config <path>` flag overrides the default. A `--dry-run` flag loads, validates, prints the resolved config, and exits without binding any ports.
- `logging` wires `tracing` with per-face spans, a rolling file appender under `%USERPROFILE%\qsoripper-cathub.log` on Windows or `$XDG_STATE_HOME/qsoripper/cathub.log` on Linux, and a periodic summary line carrying commands per second per face, cache hit ratio, real-radio reads per second, passthrough reads per second, AI frames per second, dropped/denied PTT writes, denied config writes, and reconnect events.
- `main` wires everything, installs Ctrl+C and SIGTERM handlers that emit `RX;` on shutdown to avoid a stuck transmitter, and exits with a non-zero code on fatal initialization failures.

## 7. Multi-radio model

The `RadioBackend` trait is the radio-side abstraction. Conceptual shape:

```rust
#[async_trait]
pub trait RadioBackend: Send + Sync {
    async fn poll(&self, state: &StateHandle) -> Result<(), BackendError>;
    async fn apply(&self, mutation: StateMutation, state: &StateHandle) -> Result<(), BackendError>;
    /// Parse an unsolicited frame pushed by the radio (AI mode) into a state mutation, if recognized.
    fn parse_unsolicited(&self, frame: &[u8]) -> Option<StateMutation>;
    /// Forward an opaque native command and return the raw reply (passthrough).
    async fn passthrough(&self, raw: &[u8]) -> Result<Vec<u8>, BackendError>;
    fn capabilities(&self) -> BackendCapabilities;
}
```

`StateMutation` is a universal enum:

```rust
pub enum StateMutation {
    SetVfoFreq { vfo: Vfo, hz: u64 },
    SetMode { vfo: Vfo, mode: Mode },
    SetSplit { enabled: bool, tx_vfo: Option<Vfo> },
    SetRit { offset_hz: i32, enabled: bool },
    SetXit { offset_hz: i32, enabled: bool },
    SetPtt { keyed: bool },
}
```

`BackendCapabilities` describes what the backend can do. A backend with no XIT reports `xit: false`, and the Hamlib net face returns `RPRT -11` for XIT commands against that backend without ever touching the wire. The capability table is also what `\dump_state` reports to Hamlib clients.

The `ClientDialect` trait is the client-side abstraction:

```rust
#[async_trait]
pub trait ClientDialect: Send + Sync {
    /// Handle one inbound request: read state, emit a mutation, or passthrough; return reply bytes.
    async fn handle(&self, request: &[u8], ctx: &FaceContext) -> Vec<u8>;
    /// Format a state-change notification as an unsolicited frame for this client (AI fan-out),
    /// or return None if this dialect/client does not want pushes for this change.
    fn format_notification(&self, change: &StateChange, ctx: &FaceContext) -> Option<Vec<u8>>;
}
```

`FaceContext` carries the face's permission set, its per-face AI subscription state, and handles to the universal state and the radio passthrough channel. Because dialects only touch the universal state and passthrough, any dialect can run on top of any backend **for modeled commands**. A TS-2000 dialect served by an Icom backend answers modeled reads/writes exactly as well as one served by a Kenwood backend, because both backends keep the universal state populated. **Native passthrough is the exception:** it forwards radio-family-specific bytes, so a native TS-590 dialect (N1MM, ARCP-590) only fully works over a Kenwood backend. Pairing a native dialect with a foreign backend must either fail config validation or run in modeled-only mode with passthrough disabled — the daemon must not promise ARCP-590 against a non-Kenwood radio.

### 7.1 v1 scope

- Backends: `Ts590Backend` and `LoopbackBackend`. The Kenwood code is factored so the Kenwood command table is the only thing that changes between models. `Ts2000Backend`, `Ts480Backend`, `Ts890Backend`, and friends drop in by replacing the table.
- Dialects: `Ts590Dialect` (native pass-through with state caching, AI fan-out, and unmodeled-command passthrough — serves both N1MM and ARCP-590) and `Ts2000Dialect` (for OmniRig, translator that answers `IF;`, `FA;`, `FB;` from the universal state and rejects Hamlib-style VFO-target writes).
- Faces: native serial faces for N1MM and ARCP-590, a TS-2000 serial face for OmniRig, and the Hamlib net face for WSJT-X and the engine.

### 7.2 v2 roadmap

- `IcomCiVBackend` for IC-7300, IC-7610, and IC-705. Different framing (`0xFE 0xFE` preamble, sub-address byte, `0xFD` end-of-message) but the same universal state. Icom transceive mode is the CI-V analogue of Kenwood AI and feeds the same `parse_unsolicited` path.
- `YaesuFt991aBackend` and similar Yaesu models. Kenwood-like ASCII with Yaesu-specific commands.
- `FlexSmartSdrBackend` over the native FlexRadio TCP API. No serial port involved on the radio side.
- `IcomCiVDialect` for client apps that prefer to speak CI-V.

Each entry in the v2 roadmap is one new trait impl. None of them changes any existing module.

## 8. Behavior contracts

### 8.1 Serialization and ordering

All real-radio I/O goes through the single radio task. No two commands are ever in flight to the transceiver. Each face has its own FIFO queue; the radio task selects the next command by priority **across the ready heads** of those queues — PTT (highest) > interactive client writes (frequency/mode/split) > client reads/passthrough > baseline poll (lowest) — and never reorders commands within a single face. A face's read therefore always observes that face's own earlier writes, and a client's request stream reaches the radio in submitted order, while an operator's keying and tuning still preempt another client's bulk reads. Each command carries a oneshot reply channel.

### 8.2 Coalesced polling

A client-driven status read of a **modeled** field is answered from `state` when the relevant field's `last_polled` is within its TTL. Otherwise the request waits for the next baseline poll cycle (or, when AI is active, the most recent pushed value). The result is that N concurrent client reads of `IF;` produce at most one real `IF;` to the radio per TTL window, regardless of how many client faces are active. Reads of **unmodeled** fields go through passthrough (§8.6) and are coalesced only by the optional passthrough response cache, so a controller that polls unmodeled fields hard will generate proportional real-radio traffic; the priority scheme keeps that traffic from starving interactive writes.

### 8.3 Write atomicity

Kenwood set commands do not all return an acknowledgment — many are "set and no reply." The daemon therefore classifies each modeled command's reply behavior in the backend command table as one of:

- **no-reply write:** state is updated optimistically after the serial write succeeds; correctness is reconciled by the next `AI2` echo (for `ai_covered` fields) or a verify-read for fields that are not AI-covered;
- **write with reply:** the daemon waits for and parses the reply before updating state and acking the client;
- **read:** the daemon waits for the matching solicited reply (frame matcher, §8.4).

For "write with reply" commands, subsequent reads from any face see the new value immediately after the ack. For "no-reply" commands the new value is visible after the optimistic update, then confirmed (and corrected if the radio rejected it) by the echo/verify path. Either way, HDSDR's waterfall follows N1MM's band change without HDSDR ever talking to N1MM directly. The atomicity guarantee is "ordered and eventually reconciled," not "blocked on an ack the radio never sends."

### 8.4 Auto-information (AI) fan-out

This is the mechanism that lets a knob turn on the physical radio, a band change in N1MM, a frequency set from WSJT-X, and a VFO change in ARCP-590 all stay mutually consistent at low cost.

**Frame demultiplexing contract.** With `AI2` enabled the radio emits unsolicited semicolon-terminated frames that can be byte-identical to solicited replies (notably `IF...;`). The radio task disambiguates with an explicit matcher: each outbound command declares its expected reply verb(s) and whether it expects a reply at all. When a frame arrives, if a command is pending whose expected verb matches, the frame completes that command's oneshot; otherwise the frame is routed to the `ai` parser. Commands that expect no reply (no-reply writes, §8.3) never capture an incoming frame. This must be tested against the hard interleavings: a pending `IF;` read arriving concurrently with an unsolicited `IF` push, a no-reply write immediately followed by its `AI2` echo, and passthrough reads arriving while AI frames stream.

- The `ai` module sets `AI2;` on the real radio once, at startup, and owns that setting for the daemon's lifetime.
- Unsolicited frames the radio pushes are parsed by the backend's `parse_unsolicited` into `StateMutation`s, applied to `state`, and emitted on the state change-notification broadcast.
- Each face subscribes to the broadcast. For each change, the face's dialect decides via `format_notification` whether and how to push it to its client. A native TS-590 client with its virtual AI enabled receives a synthesized `IF;`/field frame; a TS-2000 client receives the TS-2000-equivalent frame; a client with AI disabled receives nothing and continues to poll.
- The `AI` command from any client is **virtualized**: it toggles only that face's subscription, never the real radio's AI state. This removes the classic multi-client failure where one app sends `AI0;` and silences the auto-info another app depends on.
- Because the daemon, not each client, owns the real AI stream, one physical AI feed serves every AI-aware client and the baseline poll can back off (`poller`), cutting real-radio traffic.

**Push ordering and backpressure.** Each face's outbound push queue is bounded. Pushes are interleaved with that face's solicited replies at frame boundaries (never mid-frame). If a slow client lets its queue fill, the daemon **coalesces** superseded updates (keeping only the latest value per field) rather than blocking the shared broadcast or growing unboundedly; a sustained overflow is logged. AI fan-out must never apply backpressure to the radio task or to other faces.

### 8.4.1 AI mode granularity

The `AI` command is virtualized per face, but `AI1` (post-command echo) and `AI2` (spontaneous updates) are not identical semantics. v1 may collapse both to a single per-face push subscription; if it does, that limitation is documented and ARCP-590 and N1MM are tested specifically to confirm they tolerate the collapse before either is claimed as first-class. If a tested client depends on the `AI1`/`AI2` distinction, the finer three-state (`AI0`/`AI1`/`AI2`) model is implemented for that dialect.

### 8.5 PTT ownership and arbitration

Multiple clients are now PTT-capable (N1MM CAT PTT, WSJT-X `T 1`, ARCP-590). The daemon arbitrates with a single-owner lease:

- A face must have the `ptt` capability to key at all. Faces without it have their PTT writes dropped with a logged warning.
- The first capable face to key acquires the **PTT lease** and becomes the PTT owner. While the lease is held, PTT writes from any other face are rejected (Hamlib `RPRT` busy / native error) and logged, so two apps cannot key the transmitter into contention.
- The lease releases when the owner sends `RX;`/`T 0` (normal release), or after a configurable **maximum-transmit safety duration** (`ptt_max_tx_ms`) as a backstop against a client that keys and crashes. This timeout is a hard transmit-length ceiling, *not* a generic "no CAT activity" idle timer: WSJT-X keys PTT and then sends no CAT traffic for the length of a transmission, so an activity-based timeout would unkey it mid-over. `ptt_max_tx_ms` defaults above the longest expected digital-mode transmission and any safety release is logged loudly.
- The shutdown handler attempts to emit `RX;` to the radio on Ctrl+C and SIGTERM so an orderly stop never leaves the transmitter keyed. A panic or hard crash cannot reliably perform async serial I/O, so the daemon also relies on `ptt_max_tx_ms` and the radio's own TX timeout as the ultimate stuck-transmitter guards rather than promising the shutdown write always runs.

In a normal station the operator drives one mode at a time, so the lease is almost never contended; its job is to make contention safe rather than to support simultaneous transmit.

### 8.6 Passthrough for rich native clients

ARCP-590 (and any future faceplate-class controller) issues many commands the universal state does not model. The contract:

- A command whose normalized form maps to a modeled field is handled through the state path (cached read or atomic write) so it participates in coalescing, AI fan-out, and cross-client consistency.
- Any other command is forwarded verbatim through the serialized radio task and its raw reply is returned to the client unchanged. Reads may be served from a short-TTL cache keyed by the **full normalized command** (parameters included) and invalidated **by command family** when a write in that family is forwarded; commands with unknown side effects are not cached.
- Passthrough is gated by the command classification table and face permissions (§6 `permissions`): a face without `config_write` cannot push `EX`-menu or other persistent-setting writes, and unknown passthrough writes are denied by default; such attempts are logged.
- Passthrough never bypasses serialization. It is just another priority-classified entry in the radio task's inbox.

### 8.7 Graceful recovery

If the radio transport disappears (USB unplugged, radio powered off), the daemon keeps the client faces open and answers modeled status reads from `state` with a `stale` flag. State mutations and passthrough writes from clients return a non-fatal error. The radio task retries the transport with backoff. On reconnect, the daemon re-asserts `AI2;`, the baseline poller resumes, and the stale flag clears.

### 8.8 No Hamlib in the radio path

The daemon never links Hamlib. The Hamlib net protocol server is a thin server-side reimplementation of the wire protocol; it is not Hamlib code. This is the structural guarantee that the original VFO-targeting bug cannot return through any path, including the WSJT-X path, because the daemon's own `Ts590Backend` issues only TS-590 native commands and never the TS-2000-style VFO-target writes that triggered the oscillation.

## 9. Configuration

The TOML schema is:

```toml
[radio]
model = "kenwood-ts590"
transport = "serial"
port = "COM3"
baud = 115200

[poll]
baseline_ms = 200          # active when AI is unavailable
ai_heartbeat_ms = 2000     # slow liveness poll while AI2 is pushing updates
freq_ttl_ms = 50
mode_ttl_ms = 200

[ai]
enabled = true             # daemon sets AI2; on the real radio and owns it

# Read-only Hamlib endpoint for the QsoRipper engine.
[[hamlib_net]]
name = "engine"
bind = "127.0.0.1:4532"
permissions = ["read"]

# Write/PTT Hamlib endpoint for WSJT-X.
[[hamlib_net]]
name = "wsjtx"
bind = "127.0.0.1:4533"
permissions = ["read", "write", "ptt"]

[[face]]
name = "omnirig"
port = "COM10"
baud = 115200
dialect = "kenwood-ts2000"
permissions = ["read"]     # panadapter follows; does not drive the radio

[[face]]
name = "n1mm"
port = "COM20"
baud = 115200
dialect = "kenwood-ts590"
permissions = ["read", "write", "ptt"]

[[face]]
name = "arcp590"
port = "COM30"
baud = 115200
dialect = "kenwood-ts590"
permissions = ["read", "write", "ptt", "config_write"]  # full faceplate control
```

The operator creates one virtual serial pair per serial client. OmniRig binds `COM11` (the daemon binds `COM10`), N1MM binds `COM21` (daemon `COM20`), ARCP-590 binds `COM31` (daemon `COM30`). Each `[[face]]` may also specify serial line parameters (`parity`, `stop_bits`, `data_bits`) and control-line behavior (`dtr`, `rts`); these default to 8-N-1 but are configurable because some clients assert DTR/RTS or expect specific framing and the virtual pair must match on both ends. WSJT-X is configured as a Hamlib **NET rigctl** rig pointed at the write/PTT Hamlib endpoint. The QsoRipper engine continues to use its existing `RigctldProvider` configuration, pointed at the read-only Hamlib endpoint (TCP supports both clients simultaneously).

A v2 Icom configuration is structurally identical:

```toml
[radio]
model = "icom-ic7300"
transport = "serial"
port = "COM3"
baud = 115200
ci_v_address = 0x94
```

Faces, polling, AI, and the Hamlib net face are unchanged for modeled commands. Note that a native TS-590 dialect's passthrough does *not* carry over to an Icom backend (§7): an Icom station would drive the radio through the Hamlib net face and the TS-2000/CI-V dialects for modeled state, and a native Kenwood faceplate app like ARCP-590 is simply not applicable to a non-Kenwood radio. The universal state model, AI fan-out, and modeled reads/writes remain backend-independent.

## 10. Validation strategy

### 10.1 Unit tests

- `state` cache TTL, mutation, staleness, concurrent reader behavior, and change-notification broadcast delivery.
- `dialect/kenwood/ts590.rs` and `dialect/kenwood/ts2000.rs` round-trips against recorded TS-590 transcripts, including ARCP-590 `EX`-menu passthrough transcripts.
- `ai` parsing of recorded unsolicited `IF;` frames into mutations, and per-face AI virtualization (a client `AI0;` must not change the real radio's AI state).
- `permissions` enforcement: write/ptt/config_write denials for under-privileged faces.
- `hamlib_net` read and write command coverage validated against **golden transcripts captured from a real `rigctld`** (NET rigctl), including the `\dump_state` capability dump, `\chk_vfo`, `F`, `M <mode> <passband>`, `T`, `S`, `V`, simple vs extended response mode, and the exact `RPRT <code>` lines for unsupported operations. Per-endpoint permission enforcement (read-only endpoint rejects `F`/`M`/`T`).
- `backend/kenwood/ts590.rs` command table and `parse_unsolicited`/`passthrough` coverage against recorded byte sequences.
- `backend/loopback.rs` exposes a deterministic implementation used by every integration test.

### 10.2 Integration tests

- Virtual serial pairs via `tokio::io::duplex` simulate the OmniRig, N1MM, and ARCP-590 ports; a TCP client simulates WSJT-X and the engine on the Hamlib net face. A `LoopbackBackend` records every mutation and passthrough. Assertions: no modeled-field client poll causes more than one backend command per TTL window; no TS-2000 status poll ever causes a `FR0`/`FR1` to be sent; writes from one face are visible to reads on every other face; a simulated front-panel change (unsolicited frame from the loopback backend) propagates to all AI-subscribed faces.
- The existing `RigctldProvider` from `src/rust/qsoripper-core/src/rig_control/rigctld.rs` is pointed at the daemon's read-only Hamlib endpoint and consumes state without code changes. A separate simulated WSJT-X client on the write/PTT endpoint sets frequency/mode and keys PTT concurrently with the engine reading state, and a test confirms the read-only endpoint rejects `F`/`M`/`T`.
- A PTT arbitration test confirms the first capable face acquires the lease, a second capable face is rejected while the lease is held, the lease releases on `RX;`/`T 0` and on the `ptt_max_tx_ms` safety ceiling (and that a CAT-idle but actively-transmitting client is *not* unkeyed before that ceiling), and PTT from a face lacking the `ptt` capability is dropped with a warning. A shutdown test confirms `RX;` is emitted on Ctrl+C and SIGTERM, and that `ptt_max_tx_ms` bounds a simulated crash that skips the shutdown write.
- A frame-demux test drives the matcher through a pending `IF;` read arriving with a concurrent unsolicited `IF` push, a no-reply write followed by its `AI2` echo, and passthrough reads interleaved with AI frames, asserting each oneshot completes against the correct frame and no push is lost.
- A passthrough test confirms an ARCP-590-style `EX`-menu read is forwarded verbatim and that a `config_write` denial is enforced on a face without that permission.

### 10.3 Live bench

- Reproduce the VFO B scenario from the existing report. The daemon log must show zero `FR0`/`FR1` traffic and no front-panel flicker across a 30-minute soak.
- Run N1MM, HDSDR (via OmniRig), ARCP-590, WSJT-X, and the QsoRipper engine simultaneously. Exercise band changes, mode changes, RIT, and CAT PTT from N1MM; full faceplate control and `EX`-menu access from ARCP-590; frequency/mode/PTT from WSJT-X. Turn the VFO knob on the physical radio. Confirm every client reflects the change, that `dotnet run --project src\dotnet\QsoRipper.Cli -- status` reports the same state, and that only one transmitter key is ever active.
- AI fan-out: with `AI2;` owned by the daemon, confirm a physical-knob frequency change reaches every AI-subscribed client without any client having polled, and that only `ai_covered` fields back off to the heartbeat rate while meter/power keep their own cadence.
- Serial line behavior: confirm ARCP-590, N1MM, and OmniRig open their virtual ports with the configured parity/stop/data bits and DTR/RTS handling, and that a mismatch surfaces a clear error rather than silent garbled CAT.
- Stress test: poll at 50 ms from all faces simultaneously while ARCP-590 streams `EX`-menu reads. Modeled-field real-radio command rate should stay near the baseline poll rate, not the sum of client poll rates; interactive writes (PTT, frequency set) must stay responsive (priority scheduling) even under the passthrough load.

## 11. Rollout

The crate lands in phases so each phase is independently testable and useful.

Phase 1 brings up the skeleton, the radio task with the priority inbox, the `LoopbackBackend`, the universal state with change-notification broadcast, the `Ts590Backend`, and the `Ts590Dialect` with passthrough. The N1MM face works end-to-end against a real TS-590. The OmniRig face, ARCP-590 face, AI fan-out, and the Hamlib net face are not yet wired.

Phase 2 adds the `Ts2000Dialect` and the OmniRig serial face, plus the ARCP-590 native serial face and the `permissions` model. The Python safe bridge and `rigctlcom` are retired from the operator's startup scripts. For A/B comparison without two processes fighting over COM3, this phase ships a temporary `RigctldBackend` so the daemon can sit *in front of* a still-running `rigctld` (rigctld owns the physical port; the daemon owns the faces and AI fan-out) behind a configuration flag. Exactly one process owns COM3 at any time: either `rigctld` (via the `RigctldBackend`) or the daemon's `Ts590Backend` directly. Once the VFO B soak passes, the operator flips the flag and the daemon's `Ts590Backend` takes COM3 directly; the `RigctldBackend` is dropped after the transition.

Phase 3 adds the Hamlib net protocol server with full read/write support, `\dump_state`, and `\chk_vfo`. The QsoRipper engine config is repointed at the daemon, and WSJT-X is connected as a Hamlib NET rigctl rig. `rigctld` is removed from the chain entirely.

Phase 4 adds the `ai` module (central `AI2;` ownership, unsolicited-frame parsing, per-face AI virtualization, fan-out, and poller back-off), PTT lease arbitration, structured metrics, and the `--dry-run` mode.

Phase 5 finalizes documentation, but per the project's engine-spec-currency rule the spec and operator docs are updated **in the same PR as the behavior that changes them**, not deferred wholesale to the end — for example, the PR that repoints the engine at the daemon (Phase 3) updates the relevant spec note in that same PR. Phase 5 then consolidates: it updates `docs/architecture/engine-specification.md` to describe the daemon as the recommended rig-control front door on shared-radio stations, with `rigctld` retained as a supported alternative for single-client setups. It clarifies that the cathub daemon is **station infrastructure that lives below the engine's `rigctld` provider**, not part of the gRPC engine contract, so the engine remains client-agnostic and the §3.4 `RigControlService` / §5.3 rigctld integration sections are unchanged on the engine side. Adds an operator setup guide under `docs/integrations/cathub-setup.md` covering virtual serial pair creation (com0com on Windows, PTY pairs on Linux), OmniRig, N1MM, ARCP-590, and WSJT-X configuration, startup order, AI behavior, and a troubleshooting checklist. Adds `Start-CatHub`, `Stop-CatHub`, and `Get-CatHubLog` helpers to `scripts/profile-helpers.ps1` mirroring the existing `Start-Rigctld` and `Start-RigBridge` style.

## 12. Risks

- **com0com / PTY driver quirks.** Port-open failures must surface clear, actionable errors. The operator guide documents driver installation and the expected pair naming on both Windows and Linux.
- **TS-2000 and TS-590 command set drift.** A few TS-2000 commands have no clean TS-590 equivalent. The dialect layer rejects unsupported commands with a logged reply rather than guessing. The universal state plus baseline polling keeps status reads honest.
- **ARCP-590 command surface beyond the modeled state.** Passthrough covers it, but a command that mutates a modeled field through an unmodeled prefix could desync the cache. The backend's prefix map for state-mutating commands must be tested against recorded ARCP-590 transcripts, and AI fan-out provides a correction path because the radio echoes the real change.
- **AI frame parsing and the AI ownership contract.** If a client manages to change the real radio's AI state, other clients lose their push feed. The virtualization in `ai` must be the only path that ever writes `AI` to the radio; a test enforces that a client `AI0;` does not reach the wire.
- **PTT contention and stuck transmitter.** The lease makes contention safe; orderly Ctrl+C and SIGTERM paths emit `RX;`; and because a panic/crash cannot reliably do async serial I/O, `ptt_max_tx_ms` plus the radio's own TX timeout are the ultimate stuck-transmitter guards. Integration tests exercise the arbitration, the safety ceiling, and the shutdown paths.
- **Latency regressions versus the Python bridge under heavy passthrough.** The integration suite includes an end-to-end timing assertion. The budget is sub-five-millisecond added latency per modeled CAT round-trip on loopback, with interactive writes prioritized ahead of passthrough reads so ARCP-590 polling cannot inflate PTT/tuning latency.
- **Single radio, multiple writers stomping each other.** Serialization through the single radio task is structural, not advisory. An integration test fans writes in from all faces and confirms the radio sees a strict ordering.

## 13. Alignment with QsoRipper architecture

This design is consistent with the project's architectural principles:

- **Stable core, volatile edges.** The cathub daemon is an *edge* component (station infrastructure). The QsoRipper engine's stable core is untouched: it keeps consuming rig state through its existing `RigctldProvider` and the `RigControlService` gRPC contract. Hardware, dialect, and vendor-app quirks are isolated inside the daemon.
- **Normalize at the edge.** Vendor CAT dialects (TS-590 native, TS-2000, CI-V) and vendor apps (ARCP-590, OmniRig, WSJT-X) are normalized into one universal state model at the boundary, exactly as the engine normalizes QRZ and ADIF into project-owned proto/domain types.
- **Performance and low latency.** Priority scheduling, coalesced polling, AI-driven poll back-off, and a passthrough cache target the project's "everything should feel instant" goal for the interactive control path.
- **Engine specification currency.** Per project rules, Phase 5 updates `docs/architecture/engine-specification.md` in the same change that lands the behavioral shift, documenting the daemon as the recommended rig-control front door while keeping the engine's gRPC contract stable.
- **Cross-platform.** The daemon uses path-based serial endpoints and `std::path` semantics, supports com0com (Windows) and PTY pairs (Linux), and avoids hardcoded separators, satisfying the repository's Windows-and-Linux requirement even though the primary station is Windows.

## 14. Validation gates for the implementation PRs

When the implementation PRs land, each must pass:

- `cargo fmt --manifest-path src\rust\Cargo.toml --all -- --check`
- `cargo clippy --manifest-path src\rust\Cargo.toml --all-targets -- -D warnings`
- `cargo test --manifest-path src\rust\Cargo.toml`
- `cargo llvm-cov --manifest-path src\rust\Cargo.toml --workspace --exclude qsoripper-stress --exclude qsoripper-stress-tui --lcov --output-path rust-coverage.lcov`, with the workspace line coverage staying at or above the project's 80 percent threshold.
- `Push-Location src\rust; cargo deny check --config deny.toml; Pop-Location` for the PRs that touch dependencies.
- `dotnet build src\dotnet\QsoRipper.slnx` to confirm no engine regressions when the engine spec or rigctld provider are touched.

## 15. Open questions

- Whether to ship a small CLI subcommand for daemonless one-shot CAT calls (`cathub send IF;`) for troubleshooting. Easy to add; out of scope for v1.
- Whether to expose a JSON over WebSocket face for browser clients. Out of scope for v1; trivial to add later as another `ClientDialect`-shaped seam.
- Whether the per-face AI virtualization should support `AI1;` (post-command echo) semantics distinctly from `AI2;` (spontaneous), or collapse both to a single push subscription. v1 may collapse them; loggers that depend on the `AI1;` distinction would need the finer model.
- Whether to add an audio routing component in a future iteration so the same daemon can multiplex radio audio for digital modes (WSJT-X, fldigi). Explicitly out of scope and likely belongs in a separate daemon if pursued.
- Whether to support ARHP-590-style remote heads (cathub over the network) once loopback operation is proven. Out of scope for v1.

## 16. References

- Existing VFO B writeup: <https://mtreit.com/misc/radio/ts590-vfob-rigbridge-fix-report.html>.
- Hamlib `rigctld` net protocol and command set (`F`/`M`/`T` sets, `\dump_state`, `\chk_vfo`): <https://github.com/Hamlib/Hamlib/wiki/Documentation> and <https://hamlib.github.io/hamlib/rigctld.html>.
- Kenwood TS-590 CAT command reference, including the `AI` auto-information command and the `IF` status response (Kenwood publishes the official PDF on the TS-590S and TS-590SG product pages).
- Kenwood ARCP-590 Radio Control Program and ARHP-590 Host Program (Kenwood publishes both on the TS-590S/SG support pages).
- WSJT-X user guide, rig control via Hamlib / Hamlib NET rigctl: <https://wsjt.sourceforge.io/wsjtx-doc/wsjtx-main.html>.
- Icom CI-V reference (Icom publishes per-model CI-V command tables in each transceiver's PDF reference manual).
- com0com virtual serial port driver: <https://sourceforge.net/projects/com0com/>.
- N1MM Logger+ rig configuration: <https://n1mmwp.hamdocs.com/manual-windows/configurer/>.
- Existing QsoRipper rig control consumer: `src/rust/qsoripper-core/src/rig_control/rigctld.rs`.
- QsoRipper engine specification, rig control sections: `docs/architecture/engine-specification.md` (§3.4 RigControlService, §5.3 Rig Control).
