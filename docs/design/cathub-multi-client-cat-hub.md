# Design: `qsoripper-cathub` multi-client CAT hub

> **Status:** Proposed. This document is the source of truth for implementing the hub. Once implementation lands, the contract sections graduate into `docs/architecture/` and this file becomes a historical record.
>
> **Audience:** An implementer (human or AI agent) with access to this repository, the Hamlib `rigctld` net protocol reference, the Kenwood TS-590 CAT command reference, the Icom CI-V reference, and the com0com virtual serial port documentation.

## 1. Problem

A modern ham radio station runs several pieces of software against one transceiver at the same time. A typical contest station wants:

- A panadapter or SDR receiver such as HDSDR with click-to-tune on the waterfall.
- A contest logger such as N1MM Logger+ driving the rig directly for band changes, mode switches, and CAT PTT.
- The QsoRipper engine reading rig state for QSO enrichment.

Each of these wants its own CAT link to the radio. The radio exposes a single physical serial port. Only one Windows process can own that port at a time. The result is a forced choice between contest logging, panadapter tuning, and engine awareness.

The existing workaround uses Hamlib's `rigctld` to multiplex the radio over TCP and a Python "safe bridge" to translate OmniRig's serial CAT into raw Hamlib calls. The full report is at <https://mtreit.com/misc/radio/ts590-vfob-rigbridge-fix-report.html>. That stack solves a real bug (the TS-590 oscillating between VFO A and VFO B because Hamlib's TS-2000 backend retargets VFOs on every status poll), but it is fragile in three ways:

- `rigctlcom` and the Python bridge each occupy a process and a com0com endpoint, and both must agree about which one owns the radio.
- The fix depends on never letting Hamlib invoke its VFO-targeting APIs on the TS-590. Any future Hamlib backend change can reintroduce the oscillation.
- Adding a second client (N1MM, on its own virtual COM port) requires the bridge to multiplex serial faces, which the current Python script does not do.

This design replaces the bridge, `rigctlcom`, and `rigctld` with one Rust daemon that owns the radio and fans it out to as many clients as the operator needs.

## 2. Goals

- One daemon owns the real radio port. All client traffic is serialized through it.
- Multiple client apps run simultaneously against one radio with full read and write parity.
- State changes from any client propagate to all the others through a shared in-memory cache, so HDSDR's waterfall follows N1MM's band changes and the engine's view stays current.
- The radio path never invokes Hamlib's VFO-targeting APIs. The oscillation bug is structurally impossible.
- The daemon is radio-agnostic by construction. Adding a new transceiver model is a single new `RadioBackend` implementation with no changes to dialects, faces, or the state cache.
- The daemon is client-agnostic by construction. Adding a new client app means picking an existing CAT dialect or writing one new `ClientDialect` implementation.
- The QsoRipper engine continues to consume rig state without code change by speaking the Hamlib net protocol to a built-in server in the daemon.

## 3. Non-goals

- Replacing OmniRig, HDSDR, N1MM, or any other client app.
- Reimplementing Hamlib in full. The daemon implements only the subset of the `rigctld` net protocol that QsoRipper and similar clients actually use.
- Supporting every transceiver in v1. v1 ships the Kenwood TS-590. The Icom, Yaesu, and FlexRadio backends are designed for but not shipped in v1.
- Remote operation across hosts. The daemon binds loopback only. Cross-host operation is a future extension.
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

N1MM cannot insert itself anywhere in this chain without taking COM3 away from `rigctld`. The QsoRipper engine consumes state by speaking the Hamlib net protocol to `rigctld`.

## 5. Proposed architecture

The daemon (`qsoripper-cathub`) is one Rust binary. It owns the radio port. It exposes several client faces. Each client face is either a virtual COM port (for client apps that expect serial CAT) or a TCP server (for clients that speak the Hamlib net protocol).

```
                                                                +-----------------+
HDSDR --> OmniRig (TS-2000) --> com0com --> COM_OMNIRIG_SIDE -->|                 |
                                                                |                 |
N1MM (native TS-590) ------------> com0com --> COM_N1MM_SIDE -->|   qsoripper-    |
                                                                |     cathub      |--> COM3 --> TS-590
QsoRipper engine -- Hamlib net protocol --> 127.0.0.1:4532  --->|   (Rust)        |
                                                                |                 |
                                                                |  shared state   |
                                                                |  cache          |
                                                                +-----------------+
```

Two com0com pairs are required on the operator's machine. OmniRig binds one end of the first pair, the daemon binds the other end. N1MM binds one end of the second pair, the daemon binds the other end. Neither client app needs any reconfiguration beyond pointing at its assigned virtual COM port.

## 6. Component layout

The crate lives at `src/rust/qsoripper-cathub/` and is added to the workspace members in `src/rust/Cargo.toml`. Internal modules:

- `radio` owns the radio transport. v1 supports serial; v2 adds TCP for FlexRadio. The module exposes an async `submit(cmd) -> reply` API. All access is serialized through one tokio task with a bounded mpsc inbox. Framing is configurable per backend: semicolon-terminated for Kenwood and Yaesu, `0xFD`-terminated for Icom CI-V. Reconnect on disconnect is automatic and idempotent.
- `backend` defines the `RadioBackend` trait. Implementations live in `backend/kenwood/ts590.rs` and `backend/loopback.rs` for v1, with `backend/icom/ci_v.rs`, `backend/yaesu/ft991a.rs`, and `backend/flex/smartsdr.rs` as v2 modules. The trait is the only seam between the radio and everything else.
- `state` is the universal in-memory snapshot. It holds VFO A and B frequencies, mode, split, RIT and XIT offsets, S-meter, power, and PTT owner, with per-field `last_polled` and `last_set` timestamps. Mutations from any path go through this layer. Backends populate it. Dialects read and mutate it. The Hamlib net face reads and mutates it. None of those three touches the others directly.
- `poller` is a single background task that drives a low-rate baseline poll through the active backend into `state`. The baseline cadence is 200 ms by default. The cache TTL per field controls when client-driven reads piggyback on the next baseline cycle versus serve directly from cache.
- `dialect` defines the `ClientDialect` trait. Implementations: `dialect/kenwood/ts590.rs` for N1MM, `dialect/kenwood/ts2000.rs` for OmniRig, with `dialect/icom/ci_v.rs` and others as future additions. Dialects only touch the universal state. They never call the radio backend directly. This is what guarantees any dialect serves any backend.
- `serial_face` is the generic virtual-COM listener. Each configured face binds one COM port and routes its byte stream into a configured dialect. Faces run concurrently as independent tasks. Two faces serving the same dialect against the same backend is supported and expected (one for HDSDR, one for some other panadapter app).
- `hamlib_net` is the minimal `rigctld`-compatible TCP server. It binds `127.0.0.1:4532` by default. It supports the subset `qsoripper-core` actually uses: `f` (get freq), `m` (get mode), `v` (get vfo), `s` (get split), and raw `w` (write CAT) passthrough. It is implemented against the universal state, so it works for any backend without changes. The existing `RigctldProvider` in `src/rust/qsoripper-core/src/rig_control/rigctld.rs` connects to the daemon with no code change.
- `ptt` enforces PTT ownership. Routed through the state mutation API. The default policy honors `TX;` and `RX;` from a face marked `allow_ptt = true` in config and drops PTT writes from other faces with a logged warning. v1 expects only the N1MM face to set `allow_ptt = true`.
- `config` loads TOML from `%APPDATA%\QsoRipper\cathub.toml` on Windows and `$XDG_CONFIG_HOME/qsoripper/cathub.toml` on Linux. A `--config <path>` flag overrides the default. A `--dry-run` flag loads, validates, prints the resolved config, and exits without binding any ports.
- `logging` wires `tracing` with per-face spans, a rolling file appender under `%USERPROFILE%\qsoripper-cathub.log` on Windows or `$XDG_STATE_HOME/qsoripper/cathub.log` on Linux, and a periodic summary line carrying commands per second per face, cache hit ratio, real-radio reads per second, dropped PTT writes, and reconnect events.
- `main` wires everything, installs Ctrl+C and SIGTERM handlers that emit `RX;` on shutdown to avoid a stuck transmitter, and exits with a non-zero code on fatal initialization failures.

## 7. Multi-radio model

The `RadioBackend` trait is the radio-side abstraction. Conceptual shape:

```rust
#[async_trait]
pub trait RadioBackend: Send + Sync {
    async fn poll(&self, state: &StateHandle) -> Result<(), BackendError>;
    async fn apply(&self, mutation: StateMutation, state: &StateHandle) -> Result<(), BackendError>;
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

`BackendCapabilities` describes what the backend can do. A backend with no XIT reports `xit: false`, and the Hamlib net face returns `RPRT -11` for XIT commands against that backend without ever touching the wire.

The `ClientDialect` trait is the client-side abstraction:

```rust
#[async_trait]
pub trait ClientDialect: Send + Sync {
    async fn handle(&self, request: &[u8], state: &StateHandle) -> Vec<u8>;
}
```

A dialect parses incoming bytes, either reads from `state` to build a reply or emits a `StateMutation` through `state`'s mutation API, and returns bytes to the client. The state layer dispatches mutations to the active backend through a bounded channel and updates the cached value on backend acknowledgment.

Because dialects only touch the universal state, any dialect can run on top of any backend. A TS-2000 dialect served by an Icom backend works exactly as well as a TS-2000 dialect served by a Kenwood backend, because both backends keep the universal state populated.

### 7.1 v1 scope

- Backends: `Ts590Backend` and `LoopbackBackend`. The Kenwood code is factored so the Kenwood command table is the only thing that changes between models. `Ts2000Backend`, `Ts480Backend`, `Ts890Backend`, and friends drop in by replacing the table.
- Dialects: `Ts590Dialect` (for N1MM, native pass-through with state caching) and `Ts2000Dialect` (for OmniRig, translator that answers `IF;`, `FA;`, `FB;` from the universal state and rejects Hamlib-style VFO-target writes).

### 7.2 v2 roadmap

- `IcomCiVBackend` for IC-7300, IC-7610, and IC-705. Different framing (`0xFE 0xFE` preamble, sub-address byte, `0xFD` end-of-message) but the same universal state.
- `YaesuFt991aBackend` and similar Yaesu models. Kenwood-like ASCII with Yaesu-specific commands.
- `FlexSmartSdrBackend` over the native FlexRadio TCP API. No serial port involved on the radio side.
- `IcomCiVDialect` for client apps that prefer to speak CI-V.

Each entry in the v2 roadmap is one new trait impl. None of them changes any existing module.

## 8. Behavior contracts

### 8.1 Serialization

All real-radio I/O goes through the single radio task. No two commands are ever in flight to the transceiver. The radio task drains its mpsc inbox in FIFO order. Each command carries a oneshot reply channel.

### 8.2 Coalesced polling

A client-driven status read is answered from `state` when the relevant field's `last_polled` is within its TTL. Otherwise the request waits for the next baseline poll cycle to refresh the field. The result is that N concurrent client reads of `IF;` produce at most one real `IF;` to the radio per TTL window, regardless of how many client faces are active.

### 8.3 Write atomicity

When any face emits a state mutation, the daemon forwards the resulting CAT command to the radio, awaits the acknowledgment, updates `state`, then acks the client. Subsequent reads from any face see the new value immediately. This is what makes HDSDR's waterfall follow N1MM's band change without HDSDR ever talking to N1MM directly.

### 8.4 PTT ownership

The daemon honors PTT writes only from faces configured with `allow_ptt = true`. v1 enables this only on the N1MM face. PTT writes from other faces are dropped with a logged warning. The shutdown handler emits `RX;` to the radio whether or not PTT is currently held, so a daemon crash never leaves the transmitter keyed.

### 8.5 Graceful recovery

If the radio transport disappears (USB unplugged, radio powered off), the daemon keeps the client faces open and answers status reads from `state` with a `stale` flag. State mutations from clients return a non-fatal error. The radio task retries the transport with backoff. On reconnect, the baseline poller resumes and the stale flag clears.

### 8.6 No Hamlib in the radio path

The daemon never links Hamlib. The Hamlib net protocol server is a thin server-side reimplementation of the wire protocol; it is not Hamlib code. This is the structural guarantee that the original VFO-targeting bug cannot return through any path.

## 9. Configuration

The TOML schema is:

```toml
[radio]
model = "kenwood-ts590"
transport = "serial"
port = "COM3"
baud = 115200

[poll]
baseline_ms = 200
freq_ttl_ms = 50
mode_ttl_ms = 200

[hamlib_net]
enabled = true
bind = "127.0.0.1:4532"

[[face]]
name = "omnirig"
port = "COM10"
baud = 115200
dialect = "kenwood-ts2000"
allow_ptt = false

[[face]]
name = "n1mm"
port = "COM20"
baud = 115200
dialect = "kenwood-ts590"
allow_ptt = true
```

The operator creates two com0com pairs. OmniRig binds `COM11` (the daemon binds `COM10`). N1MM binds `COM21` (the daemon binds `COM20`). The QsoRipper engine continues to use its existing `RigctldProvider` configuration, now pointed at the daemon's built-in Hamlib net server.

A v2 Icom configuration is structurally identical:

```toml
[radio]
model = "icom-ic7300"
transport = "serial"
port = "COM3"
baud = 115200
ci_v_address = 0x94
```

Faces, polling, and the Hamlib net face are unchanged. The same N1MM TS-590 dialect on the same com0com pair continues to drive the radio because the universal state model and the dialect do not depend on the backend.

## 10. Validation strategy

### 10.1 Unit tests

- `state` cache TTL, mutation, staleness, and concurrent reader behavior.
- `dialect/kenwood/ts590.rs` and `dialect/kenwood/ts2000.rs` round-trips against recorded TS-590 transcripts.
- `backend/kenwood/ts590.rs` command table coverage against recorded byte sequences.
- `backend/loopback.rs` exposes a deterministic implementation used by every integration test.

### 10.2 Integration tests

- Two `tokio::io::duplex` pipes simulate the OmniRig and N1MM virtual COM ports. A `LoopbackBackend` records every mutation. The test asserts: no client poll causes more than one backend command per TTL window; no TS-2000 status poll ever causes a `FR0` or `FR1` to be sent; writes from one face are visible to reads on the other face.
- The existing `RigctldProvider` from `src/rust/qsoripper-core/src/rig_control/rigctld.rs` is pointed at the daemon's Hamlib net face. The test confirms the engine consumes state without code changes.
- A PTT routing test confirms that `TX;` from the N1MM face reaches the backend and `TX;` from the OmniRig face is dropped with a warning. A shutdown test confirms `RX;` is emitted on Ctrl+C and on panic.

### 10.3 Live bench

- Reproduce the VFO B scenario from the existing report. The daemon log must show zero `FR0` or `FR1` traffic and no front-panel flicker across a 30-minute soak.
- Run N1MM, HDSDR, and the QsoRipper engine simultaneously. Exercise band changes, mode changes, RIT, and CAT PTT from N1MM. Confirm HDSDR's waterfall follows N1MM's band changes and that `dotnet run --project src\dotnet\QsoRipper.Cli -- status` reports the same state.
- Stress test: poll at 50 ms from all faces simultaneously. Real-radio command rate should stay near the baseline poll rate, not the sum of client poll rates.

## 11. Rollout

The crate lands in phases so each phase is independently testable and useful.

Phase 1 brings up the skeleton, the radio task, the `LoopbackBackend`, the universal state, the `Ts590Backend`, and the `Ts590Dialect`. The N1MM face works end-to-end against a real TS-590. The OmniRig face and the Hamlib net face are not yet wired.

Phase 2 adds the `Ts2000Dialect` and the second serial face. The Python safe bridge and `rigctlcom` are retired from the operator's startup scripts. `rigctld` continues to own the radio in this phase, behind a configuration flag, so the operator can A/B against the daemon. Once the VFO B soak passes, the daemon takes COM3 directly.

Phase 3 adds the Hamlib net protocol server. The QsoRipper engine config is repointed at the daemon. `rigctld` is removed from the chain entirely.

Phase 4 adds PTT routing and ownership, structured metrics, and the `--dry-run` mode.

Phase 5 updates `docs/architecture/engine-specification.md` to describe the daemon as the recommended rig control front door on shared-radio stations, with `rigctld` retained as a supported alternative for single-client setups. Adds an operator setup guide under `docs/integrations/cathub-setup.md` covering com0com pair creation, OmniRig and N1MM configuration, startup order, and a troubleshooting checklist. Adds `Start-CatHub`, `Stop-CatHub`, and `Get-CatHubLog` helpers to `scripts/profile-helpers.ps1` mirroring the existing `Start-Rigctld` and `Start-RigBridge` style.

## 12. Risks

- **com0com driver quirks on Windows.** Port-open failures must surface clear, actionable errors. The operator guide documents driver installation and the expected pair naming.
- **TS-2000 and TS-590 command set drift.** A few TS-2000 commands have no clean TS-590 equivalent. The dialect layer rejects unsupported commands with a logged reply rather than guessing. The universal state plus baseline polling keeps status reads honest.
- **Latency regressions versus the Python bridge.** The integration suite includes an end-to-end timing assertion. The budget is sub-five-millisecond added latency per CAT round-trip on loopback.
- **PTT routing bug locking the transmitter on.** The Ctrl+C, SIGTERM, and panic handlers emit `RX;` unconditionally. An integration test exercises the shutdown path.
- **Single radio, multiple writers stomping each other.** Serialization through the single radio task is structural, not advisory. An integration test fans writes in from all faces and confirms the radio sees a strict ordering.

## 13. Validation gates for the implementation PRs

When the implementation PRs land, each must pass:

- `cargo fmt --manifest-path src\rust\Cargo.toml --all -- --check`
- `cargo clippy --manifest-path src\rust\Cargo.toml --all-targets -- -D warnings`
- `cargo test --manifest-path src\rust\Cargo.toml`
- `cargo llvm-cov --manifest-path src\rust\Cargo.toml --workspace --exclude qsoripper-stress --exclude qsoripper-stress-tui --lcov --output-path rust-coverage.lcov`, with the workspace line coverage staying at or above the project's 80 percent threshold.
- `Push-Location src\rust; cargo deny check --config deny.toml; Pop-Location` for the PRs that touch dependencies.
- `dotnet build src\dotnet\QsoRipper.slnx` to confirm no engine regressions when the engine spec or rigctld provider are touched.

## 14. Open questions

- Whether to ship a small CLI subcommand for daemonless one-shot CAT calls (`cathub send IF;`) for troubleshooting. Easy to add; out of scope for v1.
- Whether to expose a JSON over WebSocket face for browser clients. Out of scope for v1; trivial to add later as another `ClientDialect`-shaped seam.
- Whether to add an audio routing component in a future iteration so the same daemon can multiplex radio audio for digital modes. Explicitly out of scope and likely belongs in a separate daemon if pursued.

## 15. References

- Existing VFO B writeup: <https://mtreit.com/misc/radio/ts590-vfob-rigbridge-fix-report.html>.
- Hamlib `rigctld` net protocol: <https://github.com/Hamlib/Hamlib/wiki/Documentation>.
- Kenwood TS-590 CAT command reference (Kenwood publishes the official PDF on the TS-590S and TS-590SG product pages).
- Icom CI-V reference (Icom publishes per-model CI-V command tables in each transceiver's PDF reference manual).
- com0com virtual serial port driver: <https://sourceforge.net/projects/com0com/>.
- N1MM Logger+ rig configuration: <https://n1mmwp.hamdocs.com/manual-windows/configurer/>.
- Existing QsoRipper rig control consumer: `src/rust/qsoripper-core/src/rig_control/rigctld.rs`.
