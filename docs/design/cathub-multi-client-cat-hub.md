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

This design replaces the legacy multiplexing chain (the Python safe bridge and a client-facing `rigctlcom`) with one Rust daemon that is the single owner of the radio link and fans it out to as many clients as the operator needs, regardless of whether a given client speaks native TS-590 CAT, a foreign Kenwood dialect (TS-2000), or the Hamlib net protocol. "Single owner of the radio link" has two forms (§7): in native mode the daemon owns the serial port directly; in bridge mode a private, daemon-owned `rigctld` owns the port and the daemon is its sole client.

## 2. Goals

- One daemon is the single owner of the radio link, and all client traffic is serialized through it. The link is owned either directly (a native backend holds the serial port) or as the sole client of a private `rigctld` the daemon owns (bridge mode). No other process may share that link.
- Multiple client apps run simultaneously against one radio with full read and write parity. This explicitly includes a heavy native controller (ARCP-590), one or more loggers (N1MM), a foreign-dialect panadapter client (OmniRig/HDSDR), a Hamlib-net digital-mode client (WSJT-X), and the QsoRipper engine, all at once.
- State changes from any client — or from the radio's front panel — propagate to all the other clients through a shared in-memory cache, so HDSDR's waterfall follows N1MM's band changes, ARCP-590 reflects a knob turn made on the physical radio, and the engine's view stays current.
- **The daemon is rig-agnostic by construction.** Only the `RadioBackend` is rig-specific; the state cache, polling, event fan-out, faces, dialects, and PTT arbitration know nothing about any particular transceiver. All hub logic is **capability-driven** (`BackendCapabilities`), never branched on rig model. Adding a transceiver is adding a backend, in one of the trust tiers below, with no changes to the rest of the daemon.
- **The radio path never emits VFO-retargeting commands during polling.** This is the wire-level invariant that makes the original VFO A/B oscillation structurally impossible (see §8.8). It is stated in terms of bytes on the wire, not which library is linked, so it holds for hand-written, descriptor-driven, and (once certified) Hamlib-bridged backends alike.
- The daemon is client-agnostic by construction. Adding a new client app means picking an existing CAT dialect, or writing one new `ClientDialect` implementation, or pointing the client at the Hamlib net face.
- The Hamlib net face supports the **read and write** subset that modeled rig-control clients use (frequency, mode, PTT, split/VFO, plus the `\dump_state`/`\chk_vfo` handshake), validated per client against captured transcripts. This is the recommended **rig-agnostic universal tier**: it works against any backend because it is implemented entirely against the universal state and capabilities.
- Radios that can push spontaneous status updates (Kenwood `AI2;`, Icom CI-V transceive, Yaesu auto-information, Flex streaming) have that stream owned centrally by the daemon and fanned out to the clients that want it, so one real-radio event stream feeds every event-aware client without each client polling. Radios without push get the same downstream fan-out via poll-diff synthesis (see §8.4).
- Native rich controllers such as ARCP-590 work without the daemon having to model every CAT command. Commands the universal state does not model are forwarded transparently to the radio through the same serialized path (passthrough). Passthrough is a family-scoped power feature (a native dialect over a same-family backend); the rig-agnostic path for everything else is the universal tier above.
- The QsoRipper engine continues to consume rig state without code change by speaking the Hamlib net protocol to a built-in server in the daemon.
- **The daemon is a universal bridge between the Hamlib world and the direct-CAT world.** Software that speaks Hamlib/`rigctld` (WSJT-X, the QsoRipper engine, most modern apps) connects to the Hamlib net face; software that expects to own a COM port and speak the rig's native CAT directly (ARCP-590, OmniRig/HDSDR, N1MM) connects to a virtual serial face speaking its expected dialect. Neither population has to change or learn about the other, and both drive the same physical radio simultaneously. Making these "just work" together is the central goal.
- **The radio backend may also be a trusted out-of-process `rigctld`.** `rigctld` is mature and, run against a rig's correct native model, drives many transceivers robustly. The daemon can therefore use a `RigctldBackend` that talks the `rigctld` net protocol *as a client* to a separate `rigctld` process (the daemon never links Hamlib; see §7.1, §8.8). This is the fast path to **breadth** — universal *modeled* control (frequency, mode, PTT, split, RIT/XIT) across the hundreds of rigs Hamlib supports — while the daemon supplies the multiplexing, caching, event fan-out, dialect translation, and PTT arbitration that bare `rigctld` does not. It is explicitly a **modeled-control bridge, not a transparent native-CAT pipe**: because `rigctld` normalizes the radio's CAT, rich native passthrough (an ARCP-590 `EX`-menu faceplate session) is *not* available through it. Full native fidelity — passthrough, native push, the no-VFO-retargeting guarantee by construction, and lowest latency — comes from a native backend (§7.1). For the TS-590, the native backend is the recommended default; `rigctld` is the breadth/bring-up path and a robustness-proven alternative.

## 3. Non-goals

- Replacing OmniRig, HDSDR, N1MM, ARCP-590, WSJT-X, or any other client app. The daemon brokers them; it does not reimplement them.
- Reimplementing Hamlib in full. The daemon implements only the subset of the `rigctld` net protocol that real clients are shown (by captured transcript) to use. The universal tier targets **modeled rig-control clients**, not literally every Hamlib feature; the supported set is a tested compatibility matrix (§10), not an open-ended "any client" promise.
- Modeling the full vendor CAT command set in the universal state. The state models the hot, cross-client fields (frequency, mode, split, RIT/XIT, S-meter, power, PTT). Everything else (the `EX` menu, filter/DSP detail, keyer memories, tuner state) flows through family-scoped passthrough.
- Shipping the data-driven descriptor backend interpreter or an **in-process** libhamlib (FFI) backend in v1. Both are deliberate roadmap items (§7.3): the descriptor language is deferred until at least two hand-written backends exist to validate its shape, and linking libhamlib is deferred behind a non-default feature flag because of native-dependency packaging cost. The **out-of-process** `RigctldBackend` (TCP client to a separate `rigctld` process, no linking) is *not* deferred — it ships in v1 as the broad-compatibility backend (§7.1, §7.2) so the daemon is immediately useful against any rigctld-supported rig. The hand-written, Hamlib-free `Ts590Backend` remains the certified first-class reference so the trust story and the core invariants are proven on at least one rig the project owns end to end.
- Supporting every transceiver in v1. v1 ships the Kenwood TS-590 as the first reference rig. The architecture is rig-agnostic; the Icom, Yaesu, and FlexRadio backends are designed for but not shipped in v1.
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

The daemon (`qsoripper-cathub`) is one Rust binary. It is the single owner of the radio link — directly via a serial port (native backend) or as the sole client of a private `rigctld` (bridge backend, §7). It exposes several client faces. Each client face is either a virtual COM port (for client apps that expect serial CAT) or a TCP server (for clients that speak the Hamlib net protocol).

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

- `radio` owns the radio transport. v1 supports a serial port (native backend) and a TCP connection to a private `rigctld` (bridge backend); v2 adds TCP for FlexRadio. The module exposes an async `submit(cmd, priority) -> reply` API. All access is serialized through one tokio task. Scheduling preserves **per-face FIFO ordering** and prioritizes only between the ready heads of the per-face queues: a face's commands always reach the radio in the order that face submitted them, while across faces PTT outranks interactive writes, which outrank reads/passthrough, which outrank the background baseline poll. A read issued by a face after that face's own write is therefore guaranteed to observe its own write. This avoids reordering a single client's request stream (which serial CAT clients assume) while still keeping an operator's keying and tuning ahead of another client's bulk reads. Framing is configurable per backend: semicolon-terminated for Kenwood and Yaesu, `0xFD`-terminated for Icom CI-V. The radio task also demultiplexes **spontaneous** frames (see `events` below) from solicited replies via an explicit frame matcher (§8.4). Reconnect on disconnect is automatic and idempotent.
- `backend` defines the `RadioBackend` trait. Implementations live in `backend/kenwood/ts590.rs` and `backend/loopback.rs` for v1, with `backend/icom/ci_v.rs`, `backend/yaesu/ft991a.rs`, and `backend/flex/smartsdr.rs` as v2 modules. The trait is the only seam between the radio and everything else.
- `state` is the universal in-memory snapshot. It must be rich enough for split- and VFO-aware clients (Hamlib `v`/`V`/`s`/`S`, WSJT-X split, N1MM, ARCP-590), so v1 models, per VFO where applicable: per-VFO frequency (A and B), per-VFO mode and passband, the active RX VFO, the active TX VFO, split enabled plus split TX VFO/frequency, RIT and XIT enabled plus offsets, S-meter, power, and PTT owner. Each field carries `last_polled`, `last_set`, and a `native_push_covered` flag (see `poller`/§8.4). Mutations from any path go through this layer. Backends populate it. Dialects read and mutate it. The Hamlib net face reads and mutates it. The `events` module updates it from spontaneous radio frames (Kenwood `AI2;`, CI-V transceive, etc.). None of those touch the others directly. The state layer also exposes a **change-notification broadcast** (a `tokio::sync::broadcast` channel) so faces can push updates to event-aware clients.
- `poller` is a single background task that drives a low-rate baseline poll through the active backend into `state`. The baseline cadence is 200 ms by default. Push back-off is **per field, not blanket**: only fields whose `native_push_covered` flag is set (those the radio actually reports via its native push stream — Kenwood `AI2;`, typically frequency, mode, split) degrade to a slow liveness/heartbeat poll while native push is active. Fields the radio does *not* push spontaneously (S-meter, power, tuner/DSP detail) keep their own polling cadence regardless. Crucially, only `RadioEventSource::NativePush` coverage sets this flag — poll-diff synthesized events (§8.4) never do — so a backend without native push (including the `RigctldBackend`) keeps polling at the baseline rate. The cache TTL per field controls when client-driven reads piggyback on the next baseline cycle versus serve directly from cache.
- `dialect` defines the `ClientDialect` trait. Implementations: `dialect/kenwood/ts590.rs` for N1MM and ARCP-590 (native pass-through with state caching and unmodeled-command passthrough), `dialect/kenwood/ts2000.rs` for OmniRig (translator). Dialects only touch the universal state and the radio passthrough channel; they never call a specific backend directly. This is what guarantees any dialect serves any backend. A dialect can both answer a request *and* emit asynchronous frames to its client (see event fan-out, §8.4).
- `serial_face` is the generic virtual-COM listener. Each configured face binds one COM/PTY endpoint and routes its byte stream into a configured dialect. Faces run concurrently as independent tasks. Two faces serving the same dialect against the same backend is supported and expected (one for HDSDR, one for some other panadapter app; or N1MM and ARCP-590 both on the native TS-590 dialect). Each face owns an **outbound queue** so the daemon can push unsolicited frames (AI updates) to that client independently of request/response traffic.
- `hamlib_net` is the minimal `rigctld`-compatible TCP server. It binds `127.0.0.1:4532` by default and accepts multiple simultaneous connections (WSJT-X and the engine at once). It supports the **read and write** subset that real Hamlib clients use:
  - reads: `f` (get freq), `m` (get mode), `v` (get vfo), `s` (get split), `t` (get ptt), and `\get_powerstat`;
  - writes: `F` (set freq), `M` (set mode), `V` (set vfo), `S` (set split), `T` (set ptt);
  - protocol handshake: `\dump_state` and `\chk_vfo`, which WSJT-X and other Hamlib clients issue on connect to learn rig capabilities;
  - raw `w`/`W` (write/read CAT) passthrough for escape hatches.
  Every command is implemented against the universal state and the backend capability table, so it works for any backend without changes. Response formats — the `\dump_state` capability dump, `\chk_vfo`, the `M <mode> <passband>` shape, split replies, and the exact `RPRT <code>` lines — must match what real `rigctld` emits closely enough for unforgiving clients like WSJT-X; the dialect is validated against golden transcripts captured from a real `rigctld` (§10.1). Unsupported operations return the specific `RPRT` code rigctld uses (for example `RPRT -11`, "not available") rather than guessing.
  Because a single TCP endpoint cannot express per-client permissions and any local process could connect, write and PTT are **disabled by default** and the daemon supports more than one listener: a read-only endpoint for the QsoRipper engine and a separate write/PTT-enabled endpoint for WSJT-X. Each listener carries its own permission set (see `permissions`). The existing `RigctldProvider` in `src/rust/qsoripper-core/src/rig_control/rigctld.rs` connects to the read-only endpoint with no code change; WSJT-X connects to the write/PTT endpoint as a standard Hamlib NET rigctl rig.
- `events` owns the radio's spontaneous-update (push) stream centrally. On a rig that supports it, it enables the native push mode at startup (Kenwood `AI2;`, Icom CI-V transceive, etc.), parses pushed frames (typically `IF;` responses and single-field updates) into universal-state mutations tagged `RadioEventSource::NativePush`, and feeds the state change-notification broadcast. On a rig without native push, the baseline poller diffs successive polls and feeds the same broadcast tagged `PollDiff`, so downstream fan-out is uniform. It also **virtualizes** the auto-info command per client: a client that sends `AI2;`/`AI1;`/`AI0;` toggles only its *own* face's push subscription; it never changes the real radio's push state. This prevents clients from fighting over the single physical push setting.
- `passthrough` handles native CAT commands the universal state does not model (the bulk of ARCP-590's traffic: `EX` menu reads/writes, filter/DSP queries, tuner, keyer). The dialect forwards the raw command through the serialized radio task and returns the radio's reply verbatim. Reads may be served from a short-TTL cache **keyed by the full normalized command** (not just a prefix — `EX` and similar commands carry parameters that select different values), and the cache is **invalidated by command family** whenever a write in that family is forwarded. Commands whose mutability or side effects are unknown are not cached. Passthrough writes that happen to mutate a modeled field also update the universal state so other clients stay consistent. Passthrough requires that the face's dialect native command set matches the active backend's native command set (see §7 caveat); it is not portable across radio families.
- `ptt` enforces PTT ownership and arbitration. Routed through the state mutation API. See §8.5.
- `permissions` defines a per-face (and per-Hamlib-listener) capability set driven by a **per-dialect command classification table** that tags each command as one of: modeled read, modeled write, passthrough read, transient write, PTT/TX-affecting write, persistent/config write, or denied/unknown. The coarse face flags — `read`, `write`, `ptt`, and `config_write` — gate those classes (`config_write` gates `EX`-menu and other persistent-setting writes). Unknown passthrough writes default to denied unless the face explicitly opts into unsafe full control. This lets the operator give ARCP-590 full control while restricting a panadapter face to read-only, and prevents a stray native command from keying TX or rewriting menu settings from an under-privileged face.
- `config` loads TOML from `%APPDATA%\QsoRipper\cathub.toml` on Windows and `$XDG_CONFIG_HOME/qsoripper/cathub.toml` on Linux. A `--config <path>` flag overrides the default. A `--dry-run` flag loads, validates, prints the resolved config, and exits without binding any ports.
- `logging` wires `tracing` with per-face spans, a rolling file appender under `%USERPROFILE%\qsoripper-cathub.log` on Windows or `$XDG_STATE_HOME/qsoripper/cathub.log` on Linux, and a periodic summary line carrying commands per second per face, cache hit ratio, real-radio reads per second, passthrough reads per second, native-push frames per second, dropped/denied PTT writes, denied config writes, and reconnect events.
- `main` wires everything, installs Ctrl+C and SIGTERM handlers that emit `RX;` on shutdown to avoid a stuck transmitter, and exits with a non-zero code on fatal initialization failures.

## 7. Multi-radio model

The `RadioBackend` trait is the radio-side abstraction. Conceptual shape:

```rust
#[async_trait]
pub trait RadioBackend: Send + Sync {
    async fn poll(&self, state: &StateHandle) -> Result<(), BackendError>;
    async fn apply(&self, mutation: StateMutation, state: &StateHandle) -> Result<(), BackendError>;
    /// Parse a spontaneous frame the radio pushed (Kenwood AI, CI-V transceive, Yaesu
    /// auto-info, Flex stream) into a state mutation, if recognized. Backends without a
    /// native push source return None and rely on poll-diff synthesis (§8.4).
    fn parse_event(&self, frame: &[u8]) -> Option<StateMutation>;
    /// Forward an opaque native command and return the raw reply (family-scoped passthrough).
    async fn passthrough(&self, raw: &[u8]) -> Result<Vec<u8>, BackendError>;
    fn capabilities(&self) -> BackendCapabilities;
}
```

`BackendCapabilities` is the single source of rig-specific truth the rest of the daemon consults: VFO count and sub-receiver presence, supported modes and split style, RIT/XIT and meter availability, frequency ranges, native-push support and its per-field coverage, native command family (for passthrough compatibility), and the trust tier (§7.1). No hub module branches on rig model; it branches on capabilities. The capability table is also what `\dump_state` reports to Hamlib clients. A backend with no XIT reports `xit: false`, and the Hamlib net face returns `RPRT -11` for XIT commands against that backend without ever touching the wire.

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

`BackendCapabilities` (above) is the rig-specific seam; nothing else in the daemon is.

The `ClientDialect` trait is the client-side abstraction:

```rust
#[async_trait]
pub trait ClientDialect: Send + Sync {
    /// Handle one inbound request: read state, emit a mutation, or passthrough; return reply bytes.
    async fn handle(&self, request: &[u8], ctx: &FaceContext) -> Vec<u8>;
    /// Format a state-change notification as an unsolicited frame for this client (event fan-out),
    /// or return None if this dialect/client does not want pushes for this change.
    fn format_notification(&self, change: &StateChange, ctx: &FaceContext) -> Option<Vec<u8>>;
}
```

`FaceContext` carries the face's permission set, its per-face event subscription state, and handles to the universal state and the radio passthrough channel. There are two tiers of client compatibility, and the distinction is what makes the daemon rig-agnostic:

- **Universal tier (rig-agnostic).** The Hamlib net face and any dialect's *modeled* commands are served entirely from the universal state and capabilities. They work against any backend. A TS-2000 modeled read served by an Icom backend answers exactly as well as one served by a Kenwood backend. This is the recommended path for the broad population of clients.
- **Native-dialect tier (family-scoped).** Raw passthrough forwards radio-family-specific bytes, so a native TS-590 dialect (N1MM, ARCP-590) only fully works over a Kenwood backend. Passthrough **fails closed**: pairing a native dialect with a non-matching backend family must fail config validation, or run in modeled-only mode with passthrough disabled. The daemon never promises ARCP-590 against a non-Kenwood radio.

### 7.1 Backend strategies and trust tiers

A backend is any implementation of `RadioBackend`. There are four ways to produce one, all interchangeable from the daemon's point of view, each declaring a **trust tier** in its capabilities:

- **First-class native backend** (e.g. `Ts590Backend`): hand-written Rust, no Hamlib dependency, wire-trace certified to honor the no-VFO-retargeting-on-poll invariant (§8.8), and covered by transcript tests. This is the bug-proof reference for rigs the project supports directly. Command metadata (framing, per-field get/set templates, reply behavior, mode/band value maps, push command and parse rules, side-effect class) is held in **tables** rather than scattered through code, so it is straightforward to later serialize into a descriptor.
- **Out-of-process `rigctld` backend** (`RigctldBackend`, first-class for breadth, v1): the daemon connects to a separately launched, daemon-private `rigctld` over TCP and speaks the `rigctld` net protocol **as a client**. The daemon never links Hamlib, so this carries no native-linking packaging cost and cannot reintroduce in-process Hamlib state bugs. It gives immediate **modeled-control** breadth across every rig Hamlib supports, using each rig's **correct native Hamlib model** (notably *not* the TS-2000 model on a TS-590). It is the recommended way to bring up a rig that has no native backend yet, and a robustness-proven alternative on rigs that do. Two deliberate limits: (1) it is a *modeled-control* bridge — `rigctld` normalizes the native CAT away, so family-scoped native passthrough (ARCP-590 `EX` menu) is **not** available through it, and it should report `native_command_family: None` / no raw-passthrough capability; (2) it is **uncertified by default** for the no-VFO-retargeting invariant, because the daemon does not control `rigctld`'s radio-side traffic. A specific `rigctld` version + model + configuration is promoted to *certified* only after the §10.3 soak observes the **`rigctld`-to-radio wire** (not just daemon-to-`rigctld`) and finds no VFO-retargeting. Until then the pairing runs behind an explicit opt-in and does not advertise the guarantee in `\dump_state` trust metadata.
- **Descriptor backend** (roadmap, §7.3): one generic interpreter driven by a declarative descriptor (TOML/RON) carrying the same table data, so adding a same-family rig is data plus tests rather than code. This is the long-term path to native fidelity (passthrough, push, wire-level control) *without* per-rig code, where `rigctld` gives breadth without native fidelity. There is strong prior art for the format: OmniRig's per-rig `.ini` files describe each transceiver's CAT command set and status-polling/parse masks declaratively (see the RigIni format reference in §16), and Hamlib encodes equivalent per-rig knowledge in its backends. The descriptor language should be researched against OmniRig's `.ini` model as a starting point — while deliberately *not* copying its polling behavior, because OmniRig's own status polling (like Hamlib's TS-2000 backend) is a source of the VFO-switching issue this design exists to prevent. A descriptor is only "supported" when its test suite passes, including a certification that its poll command list emits no VFO-target traffic. Deferred until at least two native backends exist to prove the descriptor language must express enough (reply matching, event demux, no-reply/verify semantics, side-effect classification, passthrough invalidation, VFO semantics, per-field poll/push coverage).
- **In-process libhamlib (FFI) backend** (roadmap, §7.3, non-default `hamlib-ffi` feature): links libhamlib directly. Functionally similar reach to the out-of-process bridge but with native packaging cost (DLL/SO discovery, ABI drift, cross-build, licensing) and the in-process state-bug surface the out-of-process bridge avoids, so it is deferred and, when built, CI-gated on both Windows and Linux. First-class native backends remain Hamlib-free regardless.

The out-of-process `rigctld` backend and the first-class native backend together cover the two realities: a hand-certified, dependency-free, full-fidelity driver for the primary rig, and a trusted breadth bridge to everything else Hamlib already drives well. The hub's durable value — single-owner serialization, coalesced polling, event fan-out, PTT arbitration, and dialect translation — is identical regardless of which strategy produced the backend.

### 7.1.1 Which backend is best, and why the trait is the real abstraction

The deliberate answer is *no single backend implementation is best for every rig* — which is exactly why the abstraction lives in the `RadioBackend` trait and `BackendCapabilities`, not in any one strategy. Backends trade along two axes:

- **Fidelity** — exact bytes on the wire (so the no-VFO-retargeting invariant holds *by construction*), native push (AI2 / CI-V transceive) as a true event stream, transparent native-CAT passthrough for rich faceplate apps (ARCP-590's `EX` menu, keyer, tuner), and the lowest latency (no extra process or TCP hop).
- **Breadth and effort** — how many rigs are covered for how much engineering.

A **hand-written native backend maximizes fidelity** and is therefore the **recommended default for the TS-590 and any rig the project supports richly**. It is the only strategy that delivers ARCP-590 faceplate passthrough, native AI2 push, the invariant by construction, and minimal latency — i.e. the fast, efficient TS-590 control this design targets. Its cost is per-rig engineering.

The **`rigctld` bridge maximizes breadth** at near-zero per-rig cost by reusing Hamlib's mature drivers, but it is modeled-control-only and cannot guarantee the invariant or carry native passthrough (above). It is the best choice for bringing a *new* rig up quickly and as a robustness-proven alternative, not as the TS-590 default.

The **descriptor backend** is the intended way to eventually get native fidelity *and* breadth together — declarative per-rig data over one interpreter — which is why it, not the `rigctld` bridge, is the long-term scaling target. The `rigctld` bridge remains valuable as the zero-effort fallback for the long tail of rigs no one has described yet.

So the recommended composition is: native backend for the TS-590 (and each rig we invest in), `rigctld` bridge for immediate breadth and as a trusted alternative, descriptor backend as the scaling endgame, and in-process FFI only as a last resort. Picking the right strategy per rig is a capability/config choice; the rest of the daemon never changes.

### 7.2 v1 scope

- Backends: `Ts590Backend` (first-class native, the recommended default for the reference rig), `RigctldBackend` (out-of-process bridge for modeled control of any other rigctld-supported rig), and `LoopbackBackend`. The Kenwood code is table-driven so `Ts2000Backend`, `Ts480Backend`, `Ts890Backend`, and friends drop in by replacing the table; those tables are the future descriptor data.
- Dialects: `Ts590Dialect` (native pass-through with state caching, event fan-out, and family-scoped passthrough — serves both N1MM and ARCP-590) and `Ts2000Dialect` (for OmniRig, translator that answers `IF;`, `FA;`, `FB;` from the universal state and rejects Hamlib-style VFO-target writes).
- Faces: native serial faces for N1MM and ARCP-590, a TS-2000 serial face for OmniRig, and the Hamlib net face for WSJT-X and the engine.

### 7.3 Roadmap

- `IcomCiVBackend` for IC-7300, IC-7610, and IC-705. Different framing (`0xFE 0xFE` preamble, sub-address byte, `0xFD` end-of-message) but the same universal state. Icom transceive mode is the CI-V analogue of Kenwood AI and feeds the same `parse_event` path.
- `YaesuFt991aBackend` and similar Yaesu models. Kenwood-like ASCII with Yaesu-specific commands.
- `FlexSmartSdrBackend` over the native FlexRadio TCP API. No serial port involved on the radio side; its status stream feeds `parse_event`.
- The **descriptor backend** interpreter, once the second native backend has clarified the descriptor language.
- The **in-process libhamlib (FFI) backend** (non-default `hamlib-ffi` feature), only if a deployment cannot run a separate `rigctld` process and the out-of-process `RigctldBackend` is therefore unavailable.
- `IcomCiVDialect` for client apps that prefer to speak CI-V.

Each native/descriptor entry is one new backend (or descriptor file). None of them changes any existing module; the hub stays rig-agnostic.

## 8. Behavior contracts

### 8.1 Serialization and ordering

All real-radio I/O goes through the single radio task. No two commands are ever in flight to the transceiver. Each face has its own FIFO queue; the radio task selects the next command by priority **across the ready heads** of those queues — PTT (highest) > interactive client writes (frequency/mode/split) > client reads/passthrough > baseline poll (lowest) — and never reorders commands within a single face. A face's read therefore always observes that face's own earlier writes, and a client's request stream reaches the radio in submitted order, while an operator's keying and tuning still preempt another client's bulk reads. Each command carries a oneshot reply channel.

### 8.2 Coalesced polling

A client-driven status read of a **modeled** field is answered from `state` when the relevant field's `last_polled` is within its TTL. Otherwise the request waits for the next baseline poll cycle (or, when AI is active, the most recent pushed value). The result is that N concurrent client reads of `IF;` produce at most one real `IF;` to the radio per TTL window, regardless of how many client faces are active. Reads of **unmodeled** fields go through passthrough (§8.6) and are coalesced only by the optional passthrough response cache, so a controller that polls unmodeled fields hard will generate proportional real-radio traffic; the priority scheme keeps that traffic from starving interactive writes.

Poll back-off keys off native-push coverage of the **currently active** receive VFO, not a fixed VFO A. The baseline poller asks whether the active VFO's frequency is covered by `NativePush` before slowing down; if it tested a hard-coded VFO A while the radio sat on VFO B, it would needlessly over-poll a VFO whose pushes already keep state fresh.

### 8.3 Write atomicity

Kenwood set commands do not all return an acknowledgment — many are "set and no reply." The daemon therefore classifies each modeled command's reply behavior in the backend command table as one of:

- **no-reply write:** state is updated optimistically after the serial write succeeds; correctness is reconciled by the next native-push echo (for `native_push_covered` fields) or a verify-read for fields that are not push-covered;
- **write with reply:** the daemon waits for and parses the reply before updating state and acking the client;
- **read:** the daemon waits for the matching solicited reply (frame matcher, §8.4).

For "write with reply" commands, subsequent reads from any face see the new value immediately after the ack. For "no-reply" commands the new value is visible after the optimistic update, then confirmed (and corrected if the radio rejected it) by the echo/verify path. Either way, HDSDR's waterfall follows N1MM's band change without HDSDR ever talking to N1MM directly. The atomicity guarantee is "ordered and eventually reconciled," not "blocked on an ack the radio never sends."

### 8.4 Event (spontaneous update) fan-out

This is the mechanism that lets a knob turn on the physical radio, a band change in N1MM, a frequency set from WSJT-X, and a VFO change in ARCP-590 all stay mutually consistent at low cost. It is **rig-agnostic**: the Kenwood `AI2;` auto-information stream is one concrete event source; Icom CI-V transceive, Yaesu auto-information, and the Flex status stream are others; and rigs with no push at all are handled by poll-diff synthesis. All of them normalize to one internal stream of state mutations, each tagged with a source:

```rust
pub enum RadioEventSource {
    NativePush,      // AI2 / CI-V transceive / Yaesu auto-info / Flex stream
    PollDiff,        // synthesized by the poller diffing successive polls
    OptimisticWrite, // a client write we applied before reconciliation
    VerifyRead,      // a confirming read after a no-reply write
}
```

Downstream fan-out is identical regardless of source, so a station with a non-push rig behaves the same as one with `AI2`. The crucial distinction is that **only `NativePush` events count as evidence the radio provides spontaneous updates**: poll back-off (`poller`) and any field's `native_push_covered`/freshness flag are driven by `NativePush` coverage alone. Poll-diff events give downstream uniformity but never cause the poller to slow down or claim freshness it does not have.

**Frame demultiplexing contract.** When a native push source is enabled the radio emits frames that can be byte-identical to solicited replies (Kenwood `IF...;` is the classic case). The radio task disambiguates with an explicit matcher: each outbound command declares its expected reply verb(s) and whether it expects a reply at all. When a frame arrives, if a command is pending whose expected verb matches, the frame completes that command's oneshot; otherwise the frame is routed to the backend's `parse_event`. Commands that expect no reply (no-reply writes, §8.3) never capture an incoming frame. This must be tested against the hard interleavings: a pending `IF;` read arriving concurrently with an unsolicited `IF` push, a no-reply write immediately followed by its push echo, and passthrough reads arriving while push frames stream.

- The `events` module owns the radio's native push setting centrally (for Kenwood it sets `AI2;` once at startup; for Icom it enables transceive; etc.) and owns it for the daemon's lifetime.
- Native frames the radio pushes are parsed by the backend's `parse_event` into `StateMutation`s, tagged `NativePush`, applied to `state`, and emitted on the state change-notification broadcast. Where no native push exists, the poller emits the same broadcast tagged `PollDiff`.
- Each face subscribes to the broadcast. For each change, the face's dialect decides via `format_notification` whether and how to push it to its client. A native TS-590 client with its virtual auto-info enabled receives a synthesized `IF;`/field frame; a TS-2000 client receives the TS-2000-equivalent frame; a client with auto-info disabled receives nothing and continues to poll.
- The auto-info command from any client (Kenwood `AI`, etc.) is **virtualized**: it toggles only that face's subscription, never the real radio's push state. This removes the classic multi-client failure where one app disables auto-info and silences the stream another app depends on.
- Because the daemon, not each client, owns the real push stream, one physical event feed serves every event-aware client and the baseline poll can back off (for `NativePush`-covered fields only), cutting real-radio traffic.

**Push ordering and backpressure.** Each face's outbound push queue is bounded. Pushes are interleaved with that face's solicited replies at frame boundaries (never mid-frame). If a slow client lets its queue fill, the daemon **coalesces** superseded updates (keeping only the latest value per field) rather than blocking the shared broadcast or growing unboundedly; a sustained overflow is logged. Event fan-out must never apply backpressure to the radio task or to other faces.

**VFO-switch fan-out must lead with a frequency frame.** When the active receive VFO changes (e.g. an operator or client switches VFO A→B), the dialect that synthesizes the notification for a panadapter-class client (TS-2000 translator used by HDSDR/OmniRig) **must emit the new active VFO's `FA` frequency frame (and `MD` mode) first**, not only an `IF` status frame. HDSDR/OmniRig retune the panadapter exclusively from `FA`; an `IF`-only notification leaves the display stuck on the old VFO's frequency even though the cache is correct. This is the wire-level fix for the HDSDR "lost rig control on A/B switch" failure.

**Native TS-590 active-VFO pushes must carry the operating `IF` frame.** Operating-frequency trackers on the native dialect (N1MM Logger+, Log4OM-as-TS590, ARCP-590) read the *displayed* frequency and mode from the `IF;` operating-status answer, not from a bare per-VFO `FB`. The native `Ts590Dialect` therefore emits the explicit per-VFO frame (`FA`/`FB`) for any frequency change **and**, whenever the change is on the **active** receive VFO, appends a synthesized `IF` frame; an active-VFO mode change emits `MD` plus the same `IF`. Without the appended `IF`, a frequency change while the rig sat on VFO B pushed only `FB`, which these clients ignore — so the displayed frequency silently froze on VFO B while VFO A worked. This is the native-dialect counterpart to the TS-2000 lead-with-`FA` rule above and the wire-level fix for the N1MM "frequency stops tracking on VFO B" failure.

**Lagged-subscriber resync.** The change-notification broadcast ring is bounded, so a momentarily slow face can miss intermediate one-shot changes (a single Mode or VFO toggle that is evicted before the face drains the channel). When a face observes a broadcast *lag* it must **not** simply skip ahead — that would leave the client rendering permanently stale state until the next coincidental change. Instead the face replays the current `state` snapshot as a full set of field notifications through its dialect (which still gates on the face's own auto-info subscription, so an auto-info-off face emits nothing), with the active-VFO selection applied last so the client ends on the correct receive VFO.

### 8.4.1 Auto-info mode granularity

The auto-info command is virtualized per face, but for Kenwood `AI1` (post-command echo) and `AI2` (spontaneous updates) are not identical semantics (other vendors have analogous distinctions). v1 may collapse them to a single per-face push subscription; if it does, that limitation is documented and ARCP-590 and N1MM are tested specifically to confirm they tolerate the collapse before either is claimed as first-class. If a tested client depends on the distinction, the finer model is implemented for that dialect.

### 8.4.2 Single-VFO operating-VFO virtualization

Some loggers run as single-VFO controllers and actively reject the inactive VFO. N1MM Logger+ in **SO1V** mode is the canonical case: when the radio reports VFO B as the active receive VFO (Kenwood `IF;` field P10 = `1`), N1MM raises *"You should not use VFO B when configured for SO1V"* and **freezes its frequency display**, so an operator working on VFO B sees a stuck frequency even though the hub's cache is correct. Faithfully reporting P10 = `1` (§8.4 native active-VFO rule) is *correct* radio behavior, but it defeats the hub's goal of seamless A/B operation for this class of client.

The fix is a per-face, opt-in **operating-VFO virtualization** policy (`single_vfo = true` on a `ts590` face). When enabled, the face never exposes the physical VFO letter; it always presents whichever VFO the radio is actually on (the operating / receive VFO) **as VFO A**:

- **`IF;` reads** report P10 = `0` (VFO A) and split = `0`, carrying the operating VFO's frequency and mode. The 38-byte frame length is unchanged; only the active-VFO and split digits are rewritten.
- **`FA;` reads** return the operating VFO's frequency; **`FA` writes** tune the operating VFO (so a "set VFO A" from the logger tunes physical VFO B when the rig is on B).
- **`FB`** is mirrored onto the operating VFO rather than leaking physical VFO B, and split-on-B never reaches the face.
- **`FR`/`FT`** (receive/transmit VFO select) are intercepted: reads return VFO A; a select write is swallowed (never forwarded to the radio), so the logger can never drive an A/B retarget.
- **Notifications** are re-presented: an operating-VFO frequency change pushes `FA` (never `FB`) plus the synthesized operating `IF`; a mode change pushes `MD` + `IF`; and an **A→B (or B→A) switch re-presents the new operating VFO** by pushing its `FA` + `MD` + an `IF` with P10 = `0`, so the logger seamlessly retunes with no SO1V warning. Inactive-VFO churn is suppressed.
- **Raw passthrough** frames are suppressed for a single-VFO face, since a verbatim native frame could leak a physical VFO-B verb.

This policy is **opt-in per face and off by default**. It is enabled only for genuine single-VFO loggers (N1MM SO1V). Dual-VFO faceplates such as **ARCP-590 must leave it off** (`single_vfo = false`), because they legitimately control and display the real VFO A and VFO B. Split-aware clients are out of scope for this simplification: a single-VFO face always sees split = `0`. SO2V is an N1MM-side workaround, not a substitute for this hub-side virtualization.

### 8.5 PTT ownership and arbitration

Multiple clients are now PTT-capable (N1MM CAT PTT, WSJT-X `T 1`, ARCP-590). The daemon arbitrates with a single-owner lease:

- A face must have the `ptt` capability to key at all. Faces without it have their PTT writes dropped with a logged warning.
- The first capable face to key acquires the **PTT lease** and becomes the PTT owner. While the lease is held, PTT writes from any other face are rejected (Hamlib `RPRT` busy / native error) and logged, so two apps cannot key the transmitter into contention.
- The lease releases when the owner sends `RX;`/`T 0` (normal release), or after a configurable **maximum-transmit safety duration** (`ptt_max_tx_ms`) as a backstop against a client that keys and crashes. This timeout is a hard transmit-length ceiling, *not* a generic "no CAT activity" idle timer: WSJT-X keys PTT and then sends no CAT traffic for the length of a transmission, so an activity-based timeout would unkey it mid-over. `ptt_max_tx_ms` defaults above the longest expected digital-mode transmission and any safety release is logged loudly.
- The shutdown handler attempts to emit `RX;` to the radio on Ctrl+C and SIGTERM so an orderly stop never leaves the transmitter keyed. A panic or hard crash cannot reliably perform async serial I/O, so the daemon also relies on `ptt_max_tx_ms` and the radio's own TX timeout as the ultimate stuck-transmitter guards rather than promising the shutdown write always runs.
- A face that **disconnects while it holds the PTT lease** (TCP drop, client crash, EOF, or a protocol-violating over-long frame that forces the face closed) releases the lease as part of face teardown: the daemon unkeys the radio (`RX;`/`T 0`) and frees the lease so the next client can key, rather than holding the transmitter up until the `ptt_max_tx_ms` ceiling. Teardown releases **only** when the disconnecting face is the current lease owner, so it never disturbs another face's active transmission.

In a normal station the operator drives one mode at a time, so the lease is almost never contended; its job is to make contention safe rather than to support simultaneous transmit.

### 8.6 Passthrough for rich native clients

ARCP-590 (and any future faceplate-class controller) issues many commands the universal state does not model. The contract:

- A command whose normalized form maps to a modeled field is handled through the state path (cached read or atomic write) so it participates in coalescing, event fan-out, and cross-client consistency.
- Any other command is forwarded verbatim through the serialized radio task and its raw reply is returned to the client unchanged. Reads may be served from a short-TTL cache keyed by the **full normalized command** (parameters included) and invalidated **by command family** when a write in that family is forwarded; commands with unknown side effects are not cached.
- Passthrough is gated by the command classification table and face permissions (§6 `permissions`): a face without `config_write` cannot push `EX`-menu or other persistent-setting writes, and unknown passthrough writes are denied by default; such attempts are logged.
- Passthrough never bypasses serialization. It is just another priority-classified entry in the radio task's inbox.

### 8.7 Graceful recovery

If the radio transport disappears (USB unplugged, radio powered off), the daemon keeps the client faces open and answers modeled status reads from `state` with a `stale` flag. State mutations and passthrough writes from clients return a non-fatal error. The radio task retries the transport with backoff. On reconnect, the daemon re-asserts the native push setting (Kenwood `AI2;`, Icom transceive, etc.), the baseline poller resumes, and the stale flag clears.

### 8.8 The no-VFO-retargeting invariant

The structural guarantee against the original bug is stated at the **wire level**, not in terms of which library is linked: *baseline polling and modeled reads must never emit radio-side VFO-selection or VFO-retargeting commands; such commands are sent only for an explicit user/client VFO or split mutation.* This is what makes the TS-590 VFO A/B oscillation impossible, and it is the certification target for every backend (§10.3).

- The daemon **never links Hamlib** in any first-class path. The Hamlib net face is a thin server-side reimplementation of the wire protocol, not Hamlib code, and the out-of-process `RigctldBackend` is a TCP *client* of a separate `rigctld` process — neither links libhamlib.
- The first-class native `Ts590Backend` satisfies the invariant by construction: it issues only TS-590 native commands and never the TS-2000-style VFO-target writes that triggered the oscillation. Its poll command list is asserted in tests to contain no VFO-target verbs.
- The out-of-process `RigctldBackend` is **uncertified by default**. It satisfies the invariant only when `rigctld` runs against the rig's **correct native model** (e.g. the TS-590 model, never the TS-2000 model), and even then the daemon cannot prove it because it does not control `rigctld`'s radio-side traffic. A specific `rigctld` **version + model + configuration** is promoted to *certified* only after the §10.3 soak observes the **`rigctld`-to-radio wire** (a serial-line capture, not just the daemon-to-`rigctld` TCP traffic) and finds no VFO-retargeting under the full client compatibility matrix. Until certified, the pairing requires explicit operator opt-in and does not advertise the no-VFO guarantee in logs or `\dump_state` trust metadata. The bridge also reports no native push (it relies on poll-diff events, §8.4) unless a tested net-protocol push path is added.
- The optional in-process libhamlib (FFI) backend carries the same per-rig certification requirement and the additional in-process state-bug surface, which is exactly why the out-of-process bridge is preferred and FFI is deferred (§7.1).

The invariant is enforced by a per-backend certification soak, not by a blanket "no Hamlib anywhere" rule, so the design can honor both a dependency-free certified driver and a trusted external `rigctld` without weakening the guarantee.

### 8.9 Input bounds and malformed-input handling

A multi-client hub accepts bytes from several long-lived, independently-failing endpoints plus the radio link itself, so every byte-accumulating buffer is explicitly bounded and every parsed field is validated rather than trusted:

- **Frame/line length caps.** Each in-progress accumulation buffer — the serial-face partial-frame buffer, the radio task's frame reassembler, and the `hamlib_net` request-line reader — is capped (4096 bytes). A delimiter-less stream that would otherwise grow a buffer without limit is treated as a fault: the radio/serial reassembler discards the malformed partial frame and resynchronizes on the next delimiter, while the `hamlib_net` reader closes the offending connection (and releases its PTT lease, §8.5). The cap bounds only in-progress *requests*; long multi-line *replies* such as `\dump_state` are unaffected. The `hamlib_net` line reader is a length-bounded manual reader, not an unbounded `lines()` adapter.
- **Reject, never silently coerce.** A command that decodes to an unrecognized value must be rejected with the protocol's error reply, never quietly substituted with a plausible default that would mis-drive the radio. In particular, a Hamlib `set_mode` (`M`) with an unrecognized mode token returns `RPRT -EINVAL` and writes nothing to the radio, instead of resolving the unknown token to a default mode and falsely reporting `RPRT 0`.

## 9. Configuration

The TOML schema is:

```toml
[radio]
model = "kenwood-ts590"
transport = "serial"
port = "COM3"
baud = 115200

[poll]
baseline_ms = 200          # active when native push is unavailable
heartbeat_ms = 2000        # slow liveness poll while native push covers a field
freq_ttl_ms = 50
mode_ttl_ms = 200

[events]
native_push = true         # daemon enables the rig's push stream (AI2 on TS-590) and owns it

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

Faces, polling, events, and the Hamlib net face are unchanged for modeled commands. Note that a native TS-590 dialect's passthrough does *not* carry over to an Icom backend (§7): an Icom station would drive the radio through the Hamlib net face and the TS-2000/CI-V dialects for modeled state, and a native Kenwood faceplate app like ARCP-590 is simply not applicable to a non-Kenwood radio. The universal state model, event fan-out, and modeled reads/writes remain backend-independent.

To drive a rig through a trusted out-of-process `rigctld` instead of a native backend, the operator points the radio at the bridge. The daemon owns the faces, cache, event fan-out, and arbitration; a **daemon-private** `rigctld` owns the physical port. That `rigctld` must accept the daemon as its **sole client** — no other app may connect to it directly, or it would bypass the daemon's serialization, PTT lease, and cache. The Hamlib model `rigctld` is launched with is the per-rig safety knob (§8.8):

```toml
[radio]
backend = "rigctld"        # out-of-process bridge; the daemon never links Hamlib
transport = "tcp"
host = "127.0.0.1"
port = 4532                 # the daemon-private rigctld (no other client may connect)
# rigctld itself is started against the rig's correct native model, e.g.
#   rigctld -m 2045 -r COM3 -s 115200      (TS-590SG native model)
# uncertified by default; promoted per rigctld version+model+config via the §10.3 soak.
```

The rest of the config — faces, permissions, polling, events — is identical for the **modeled** control surface, which is the point: a direct-CAT app (OmniRig, N1MM) gets the same frequency/mode/PTT/split behavior over its virtual COM face whether the backend is native or the `rigctld` bridge. The bridge does *not* carry native faceplate passthrough, so an `EX`-menu controller like ARCP-590 still requires a native same-family backend (§7.1).

## 10. Validation strategy

### 10.1 Unit tests

- `state` cache TTL, mutation, staleness, concurrent reader behavior, and change-notification broadcast delivery.
- `dialect/kenwood/ts590.rs` and `dialect/kenwood/ts2000.rs` round-trips against recorded TS-590 transcripts, including ARCP-590 `EX`-menu passthrough transcripts.
- `events` parsing of recorded spontaneous frames (Kenwood unsolicited `IF;`, and an Icom CI-V transceive sample) into mutations tagged `NativePush`, and per-face event virtualization (a client `AI0;` must not change the real radio's auto-info state). A poll-diff test confirms synthesized events are tagged `PollDiff` and never trigger poller back-off or set `native_push_covered`.
- `permissions` enforcement: write/ptt/config_write denials for under-privileged faces.
- `hamlib_net` read and write command coverage validated against **golden transcripts captured from a real `rigctld`** (NET rigctl), including the `\dump_state` capability dump, `\chk_vfo`, `F`, `M <mode> <passband>`, `T`, `S`, `V`, simple vs extended response mode, and the exact `RPRT <code>` lines for unsupported operations. Per-endpoint permission enforcement (read-only endpoint rejects `F`/`M`/`T`).
- `backend/kenwood/ts590.rs` command table and `parse_event`/`passthrough` coverage against recorded byte sequences. A static assertion confirms the backend's poll command list contains no VFO-target verbs (the no-VFO-retargeting invariant, §8.8).
- `backend/rigctld.rs` (out-of-process bridge) protocol-client coverage against recorded `rigctld` net-protocol transcripts: read/set frequency, mode, PTT, split/VFO, and the `RPRT` error mapping into `BackendError`. A test confirms the bridge surfaces an uncertified rig+model pairing in its reported trust tier.
- `backend/loopback.rs` exposes a deterministic implementation used by every integration test.

### 10.2 Integration tests

- Virtual serial pairs via `tokio::io::duplex` simulate the OmniRig, N1MM, and ARCP-590 ports; a TCP client simulates WSJT-X and the engine on the Hamlib net face. A `LoopbackBackend` records every mutation and passthrough. Assertions: no modeled-field client poll causes more than one backend command per TTL window; no TS-2000 status poll ever causes a `FR0`/`FR1` to be sent; writes from one face are visible to reads on every other face; a simulated front-panel change (unsolicited frame from the loopback backend) propagates to all event-subscribed faces.
- The existing `RigctldProvider` from `src/rust/qsoripper-core/src/rig_control/rigctld.rs` is pointed at the daemon's read-only Hamlib endpoint and consumes state without code changes. A separate simulated WSJT-X client on the write/PTT endpoint sets frequency/mode and keys PTT concurrently with the engine reading state, and a test confirms the read-only endpoint rejects `F`/`M`/`T`.
- A PTT arbitration test confirms the first capable face acquires the lease, a second capable face is rejected while the lease is held, the lease releases on `RX;`/`T 0` and on the `ptt_max_tx_ms` safety ceiling (and that a CAT-idle but actively-transmitting client is *not* unkeyed before that ceiling), and PTT from a face lacking the `ptt` capability is dropped with a warning. A shutdown test confirms `RX;` is emitted on Ctrl+C and SIGTERM, and that `ptt_max_tx_ms` bounds a simulated crash that skips the shutdown write.
- A frame-demux test drives the matcher through a pending `IF;` read arriving with a concurrent unsolicited `IF` push, a no-reply write followed by its `AI2` echo, and passthrough reads interleaved with push frames, asserting each oneshot completes against the correct frame and no push is lost.
- A passthrough test confirms an ARCP-590-style `EX`-menu read is forwarded verbatim and that a `config_write` denial is enforced on a face without that permission.
- A `RigctldBackend` integration test runs the daemon in front of a stub `rigctld` server (an in-process TCP listener replaying recorded net-protocol exchanges) and confirms every face — the OmniRig TS-2000 face, the N1MM native face, and the Hamlib net face — performs **modeled** reads and writes (frequency, mode, PTT, split) against the same radio through the bridge, so a direct-CAT app is demonstrably bridged to a rigctld-driven rig for modeled control. A companion test asserts the bridge declares no native-passthrough capability, so a native `EX`-menu passthrough attempt (ARCP-590 style) fails closed rather than being silently forwarded.
- A **client compatibility matrix** is maintained as a table of captured per-client transcripts (ARCP-590, N1MM, OmniRig/HDSDR, WSJT-X, the QsoRipper engine), each replayed against the daemon in CI. A client is "supported" only when its transcript passes; the matrix, not an open-ended "any client" claim, defines the supported set (§3).

### 10.3 Live bench

- **Per-backend VFO-retargeting certification soak (§8.8).** For each backend a station will use, certify the no-VFO-retargeting invariant against the **radio-side wire** (a serial-line capture: the daemon's own port for the native backend, the `rigctld`-to-radio port for the bridge — never just the daemon-to-`rigctld` TCP hop). Reproduce the VFO B scenario from the existing report *and* replay the full client compatibility matrix (ARCP-590, N1MM, OmniRig, WSJT-X, engine) so retargeting triggered by modeled reads, split/VFO queries, or `\chk_vfo` paths is caught, not only the idle baseline poll. The capture must show zero `FR0`/`FR1` (VFO-target) traffic and no front-panel flicker across a 30-minute soak. A specific backend + (for the bridge) `rigctld` version + model + configuration that fails is not marked certified, regardless of trust tier. This soak is the gate that promotes a `rigctld`-driven rig from "uncertified bridge" to "certified."
- Run N1MM, HDSDR (via OmniRig), ARCP-590, WSJT-X, and the QsoRipper engine simultaneously. Exercise band changes, mode changes, RIT, and CAT PTT from N1MM; full faceplate control and `EX`-menu access from ARCP-590; frequency/mode/PTT from WSJT-X. Turn the VFO knob on the physical radio. Confirm every client reflects the change, that `dotnet run --project src\dotnet\QsoRipper.Cli -- status` reports the same state, and that only one transmitter key is ever active.
- Event fan-out: with the native push stream (Kenwood `AI2;`) owned by the daemon, confirm a physical-knob frequency change reaches every event-subscribed client without any client having polled, and that only `native_push_covered` fields back off to the heartbeat rate while meter/power keep their own cadence. Repeat against the `RigctldBackend` to confirm poll-diff synthesized events fan out identically and that no field is marked `native_push_covered` (the bridge keeps polling at the baseline rate).
- Serial line behavior: confirm ARCP-590, N1MM, and OmniRig open their virtual ports with the configured parity/stop/data bits and DTR/RTS handling, and that a mismatch surfaces a clear error rather than silent garbled CAT.
- Stress test: poll at 50 ms from all faces simultaneously while ARCP-590 streams `EX`-menu reads. Modeled-field real-radio command rate should stay near the baseline poll rate, not the sum of client poll rates; interactive writes (PTT, frequency set) must stay responsive (priority scheduling) even under the passthrough load.

## 11. Rollout

The crate lands in phases so each phase is independently testable and useful.

Phase 1 brings up the skeleton, the radio task with the priority inbox, the `LoopbackBackend`, the universal state with change-notification broadcast, the `Ts590Backend`, and the `Ts590Dialect` with passthrough. The N1MM face works end-to-end against a real TS-590. The OmniRig face, ARCP-590 face, event fan-out, and the Hamlib net face are not yet wired.

Phase 2 adds the `Ts2000Dialect` and the OmniRig serial face, plus the ARCP-590 native serial face and the `permissions` model. It also lands the **out-of-process `RigctldBackend`** as a first-class breadth backend: the daemon connects as the sole client of a daemon-private `rigctld` and presents that radio's **modeled** control surface (frequency, mode, PTT, split, RIT/XIT) to every face. This lets the daemon bring up *modeled* control of non-Kenwood and not-yet-natively-supported rigs immediately, and lets the operator migrate off the legacy Python safe bridge and `rigctlcom` while keeping the `rigctld` they trust. Native faceplate passthrough (ARCP-590 `EX` menu) is **not** carried over the bridge and still requires the native `Ts590Backend` (§7.1). Exactly one process owns the physical port: either `rigctld` (with the daemon as its sole client) or the daemon's native `Ts590Backend` directly — never both, and no third app connects to that `rigctld`. On a TS-590 the native backend is the recommended default; the bridge is the path for other rigs and a robustness-proven alternative. The legacy bridge and `rigctlcom` are retired from the operator's startup scripts.

Phase 3 adds the Hamlib net protocol server with full read/write support, `\dump_state`, and `\chk_vfo`. The QsoRipper engine config is repointed at the daemon, and WSJT-X is connected as a Hamlib NET rigctl rig. The *legacy* multiplexing chain (the Python safe bridge plus a client-facing `rigctld`/`rigctlcom`) is removed; `rigctld` now appears only where it belongs, as the optional radio-side backend behind `RigctldBackend` for rigs without a native backend.

Phase 4 adds the `events` module (central ownership of the radio's native push stream — Kenwood `AI2;`, CI-V transceive, etc. — spontaneous-frame parsing tagged by `RadioEventSource`, per-face push virtualization, fan-out, and `NativePush`-only poller back-off), PTT lease arbitration, structured metrics, and the `--dry-run` mode.

Phase 5 finalizes documentation, but per the project's engine-spec-currency rule the spec and operator docs are updated **in the same PR as the behavior that changes them**, not deferred wholesale to the end — for example, the PR that repoints the engine at the daemon (Phase 3) updates the relevant spec note in that same PR. Phase 5 then consolidates: it updates `docs/architecture/engine-specification.md` to describe the daemon as the recommended rig-control front door on shared-radio stations, with `rigctld` retained as a supported alternative for single-client setups. It clarifies that the cathub daemon is **station infrastructure that lives below the engine's `rigctld` provider**, not part of the gRPC engine contract, so the engine remains client-agnostic and the §3.4 `RigControlService` / §5.3 rigctld integration sections are unchanged on the engine side. Adds an operator setup guide under `docs/integrations/cathub-setup.md` covering virtual serial pair creation (com0com on Windows, PTY pairs on Linux), OmniRig, N1MM, ARCP-590, and WSJT-X configuration, startup order, AI behavior, and a troubleshooting checklist. Adds `Start-CatHub`, `Stop-CatHub`, and `Get-CatHubLog` helpers to `scripts/profile-helpers.ps1` mirroring the existing `Start-Rigctld` and `Start-RigBridge` style.

## 12. Risks

- **com0com / PTY driver quirks.** Port-open failures must surface clear, actionable errors. The operator guide documents driver installation and the expected pair naming on both Windows and Linux.
- **TS-2000 and TS-590 command set drift.** A few TS-2000 commands have no clean TS-590 equivalent. The dialect layer rejects unsupported commands with a logged reply rather than guessing. The universal state plus baseline polling keeps status reads honest.
- **ARCP-590 command surface beyond the modeled state.** Passthrough covers it, but a command that mutates a modeled field through an unmodeled prefix could desync the cache. The backend's prefix map for state-mutating commands must be tested against recorded ARCP-590 transcripts, and event fan-out provides a correction path because the radio echoes the real change.
- **Spontaneous-event parsing and the push-ownership contract.** If a client manages to change the real radio's auto-info state (Kenwood `AI`), other clients lose their push feed. The virtualization in `events` must be the only path that ever writes the auto-info command to the radio; a test enforces that a client `AI0;` does not reach the wire.
- **PTT contention and stuck transmitter.** The lease makes contention safe; orderly Ctrl+C and SIGTERM paths emit `RX;`; and because a panic/crash cannot reliably do async serial I/O, `ptt_max_tx_ms` plus the radio's own TX timeout are the ultimate stuck-transmitter guards. Integration tests exercise the arbitration, the safety ceiling, and the shutdown paths.
- **Latency regressions versus the Python bridge under heavy passthrough.** The integration suite includes an end-to-end timing assertion. The budget is sub-five-millisecond added latency per modeled CAT round-trip on loopback, with interactive writes prioritized ahead of passthrough reads so ARCP-590 polling cannot inflate PTT/tuning latency.
- **Single radio, multiple writers stomping each other.** Serialization through the single radio task is structural, not advisory. An integration test fans writes in from all faces and confirms the radio sees a strict ordering.
- **Out-of-process `rigctld` dependency.** The `RigctldBackend` adds a second process and a TCP hop, and inherits whatever model `rigctld` is launched with. Mitigations: the daemon supervises/launches `rigctld` and surfaces its exit clearly; an uncertified rig+model pairing is flagged in logs and trust metadata until the §10.3 soak passes; and a TS-590 station always has the native `Ts590Backend` as the dependency-free, certified alternative.

## 13. Alignment with QsoRipper architecture

This design is consistent with the project's architectural principles:

- **Stable core, volatile edges.** The cathub daemon is an *edge* component (station infrastructure). The QsoRipper engine's stable core is untouched: it keeps consuming rig state through its existing `RigctldProvider` and the `RigControlService` gRPC contract. Hardware, dialect, and vendor-app quirks are isolated inside the daemon.
- **Normalize at the edge.** Vendor CAT dialects (TS-590 native, TS-2000, CI-V) and vendor apps (ARCP-590, OmniRig, WSJT-X) are normalized into one universal state model at the boundary, exactly as the engine normalizes QRZ and ADIF into project-owned proto/domain types.
- **Rig-agnostic by construction (stable core, volatile edges, restated for hardware).** Rigs are edges, not core. Only the `RadioBackend` is rig-specific; the state cache, polling, event fan-out, faces, dialects, and arbitration are capability-driven and never branch on rig model. A native certified driver, a trusted out-of-process `rigctld`, and a future descriptor file are interchangeable behind one trait — so the daemon bridges the Hamlib world and the direct-CAT world for *many* rigs, not one, without the core ever learning a model name.
- **Performance and low latency.** Priority scheduling, coalesced polling, native-push-driven poll back-off, and a passthrough cache target the project's "everything should feel instant" goal for the interactive control path.
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
- When to introduce the data-driven **descriptor backend** interpreter. The trigger is the second hand-written native backend (Icom), which clarifies what the descriptor language must express; OmniRig's `.ini` RigIni format and Hamlib's per-rig backends are the prior art to study (§7.1, §16).
- Whether an **in-process libhamlib (FFI) backend** is ever worth the native packaging cost, or whether the out-of-process `RigctldBackend` covers every realistic deployment. Current lean: keep FFI deferred behind a non-default feature unless a deployment genuinely cannot run a separate `rigctld`.
- How the daemon should **supervise `rigctld`** when using `RigctldBackend`: launch and own its lifecycle, or attach to an operator-managed instance. v1 leans toward attach-or-launch configurable, with clear surfacing of `rigctld` exit.

## 16. References

- Existing VFO B writeup: <https://mtreit.com/misc/radio/ts590-vfob-rigbridge-fix-report.html>.
- Hamlib `rigctld` net protocol and command set (`F`/`M`/`T` sets, `\dump_state`, `\chk_vfo`): <https://github.com/Hamlib/Hamlib/wiki/Documentation> and <https://hamlib.github.io/hamlib/rigctld.html>.
- Kenwood TS-590 CAT command reference, including the `AI` auto-information command and the `IF` status response (Kenwood publishes the official PDF on the TS-590S and TS-590SG product pages).
- Kenwood ARCP-590 Radio Control Program and ARHP-590 Host Program (Kenwood publishes both on the TS-590S/SG support pages).
- WSJT-X user guide, rig control via Hamlib / Hamlib NET rigctl: <https://wsjt.sourceforge.io/wsjtx-doc/wsjtx-main.html>.
- Icom CI-V reference (Icom publishes per-model CI-V command tables in each transceiver's PDF reference manual).
- com0com virtual serial port driver: <https://sourceforge.net/projects/com0com/>.
- N1MM Logger+ rig configuration: <https://n1mmwp.hamdocs.com/manual-windows/configurer/>.
- OmniRig RigIni descriptor format (declarative per-rig CAT command + status-parse `.ini` files), studied as prior art for the future descriptor backend: <https://www.dxatlas.com/OmniRig/Files/RigIni.pdf>.
- OmniRig2 (MIT-licensed open-source fork), referenced for descriptor-format inspiration only — not its polling/VFO behavior: <https://github.com/HB9FXQ/OmniRig2>.
- Existing QsoRipper rig control consumer: `src/rust/qsoripper-core/src/rig_control/rigctld.rs`.
- QsoRipper engine specification, rig control sections: `docs/architecture/engine-specification.md` (§3.4 RigControlService, §5.3 Rig Control).
