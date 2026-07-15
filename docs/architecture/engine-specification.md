# QsoRipper Engine Specification

> **Version 1.0** — The authoritative contract for implementing a QsoRipper engine in any language.
>
> This is a living document. When proto files, services, or behavioral contracts change, update this specification in the same change.

A QsoRipper engine is the core runtime that owns QSO logging, callsign lookup, rig control, space weather, contest calendar lookup, station profiles, and external sync. Engines expose a gRPC API over HTTP/2 that any client — TUI, GUI, CLI, or web — can consume. The architecture is explicitly multi-engine: any conformant implementation, regardless of language, can serve as the engine behind any QsoRipper client.

This document is self-contained. A developer should be able to implement a fully conformant engine using only this specification and the `.proto` files under `proto/`.

---

## 1. Overview

QsoRipper is a high-performance ham radio logging system. Its architecture separates the **engine** (the server process that owns all data, integration, and business logic) from **clients** (TUI, GUI, CLI, DebugHost) that consume the engine over gRPC.

Key architectural properties:

- **Protocol Buffers are the single source of truth** for all shared types and service contracts. The `.proto` files under `proto/` define every message, enum, and RPC.
- **Engines are interchangeable.** Any implementation that passes the conformance harness is a valid engine.
- **Clients never own business logic.** They render state, capture input, and call RPCs.
- **ADIF is an edge concern.** Internal IPC uses protobuf exclusively. ADIF is only for external file interchange and QRZ API communication.
- **Offline-first.** Local logging must work without any network connectivity. External integrations degrade gracefully.

---

## 2. Architecture

### 2.1 Engine Role

The engine is a long-running server process responsible for:

| Responsibility | Description |
|---|---|
| QSO storage | Persistent CRUD for QSO records via a pluggable storage backend |
| Callsign lookup | QRZ XML lookups with caching, deduplication, and DXCC enrichment |
| QRZ logbook sync | Bidirectional synchronization with the QRZ logbook API |
| Rig control | Polling a rigctld daemon for logging-relevant frequency, mode, split, and power state |
| Space weather | Fetching and caching NOAA space weather indices |
| Contest calendar | Fetching and caching active contest metadata |
| Station profiles | Managing station identity and per-session overrides |
| Setup/bootstrap | First-run wizard state, credential validation, and configuration persistence |
| Runtime config | Live developer-facing configuration overrides |

The engine does **not** own any UI rendering, keyboard handling, or display logic.

### 2.2 Client-Engine Separation

```
┌─────────────┐  ┌─────────────┐  ┌─────────────┐
│     TUI     │  │     GUI     │  │  DebugHost  │
│   (Rust)    │  │  (Avalonia) │  │   (Blazor)  │
└──────┬──────┘  └──────┬──────┘  └──────┬──────┘
       │                │                │
       └────────┬───────┴────────┬───────┘
                │   gRPC/HTTP2   │
          ┌─────┴────────────────┴─────┐
          │         Engine             │
          │   (Rust or .NET or ...)    │
          └────────────────────────────┘
```

Clients connect to the engine through a single configured gRPC endpoint. The built-in local profiles use `http://127.0.0.1:50051` for Rust and `http://127.0.0.1:50052` for .NET so both engines can run concurrently. Clients may also connect through a gRPC-Web proxy for browser-based surfaces.

### 2.3 Protocol Buffers as Contract Core

All shared types live under `proto/`:

| Directory | Contents |
|---|---|
| `proto/domain/` | Domain model messages and enums (QsoRecord, CallsignRecord, Band, Mode, etc.) |
| `proto/services/` | Service definitions, RPC envelopes, and service-layer support types |

Engines generate language-specific bindings from these files. In Rust, `prost` and `tonic` generate types at build time. In C#, `Grpc.Tools` generates at build time. Never hand-write types that should come from proto generation.

The 1-1-1 rule applies: one top-level message, enum, or service per `.proto` file. Every RPC uses unique `XxxRequest`/`XxxResponse` envelopes. See `docs/architecture/data-model.md` for the full proto conventions.

### 2.4 Transport: gRPC over HTTP/2

- Native gRPC clients (CLI, TUI, GUI) connect directly over HTTP/2.
- Browser clients (DebugHost) connect through a gRPC-Web proxy that translates between gRPC-Web and native gRPC.
- The engine listens on a configurable address controlled by `QSORIPPER_SERVER_ADDR` or the launcher. Standalone defaults are `127.0.0.1:50051` for Rust and `127.0.0.1:50052` for .NET.
- TLS is not required for local development. Production deployments should use TLS or a reverse proxy.

---

## 3. Required gRPC Services

An engine must implement all services in this section except where marked **optional**. Each subsection documents every RPC with its exact types, streaming mode, expected behavior, and error semantics.

For generated protobuf runtimes, absent optional scalar fields must be **omitted**, not assigned `null`. A successful handler must never fail while materializing a response just because an optional string/error field is not present.

### 3.1 EngineService

**Proto file:** `proto/services/engine_service.proto`

A stable handshake endpoint that identifies the engine implementation. Clients use this to verify connectivity and discover engine capabilities.

#### RPCs

| RPC | Request | Response | Mode |
|---|---|---|---|
| `GetEngineInfo` | `GetEngineInfoRequest` | `GetEngineInfoResponse` | Unary |

#### GetEngineInfo

Returns metadata about the running engine.

**Behavior:**
- Must always succeed if the engine is running.
- Returns the engine's identity (`engine_id`, `display_name`), version string, and a list of supported capability strings.
- The response is an `EngineInfo` message (see `proto/services/engine_info.proto`) containing:
  - `engine_id` — stable identifier (for example, `"rust-tonic"` or `"dotnet-aspnet"`)
  - `display_name` — human-readable label
  - `version` — implementation version. Rust reports SemVer. .NET may report the four-component assembly version form.
  - `capabilities` — repeated list of capability names (see §8)

**Error semantics:**
- This RPC should never fail under normal operation.
- `UNAVAILABLE` — engine is shutting down.

### 3.2 LogbookService

**Proto file:** `proto/services/logbook_service.proto`

The primary QSO CRUD and sync surface. This is the most critical service in the engine.

#### RPCs

| RPC | Request | Response | Mode |
|---|---|---|---|
| `LogQso` | `LogQsoRequest` | `LogQsoResponse` | Unary |
| `UpdateQso` | `UpdateQsoRequest` | `UpdateQsoResponse` | Unary |
| `DeleteQso` | `DeleteQsoRequest` | `DeleteQsoResponse` | Unary |
| `RestoreQso` | `RestoreQsoRequest` | `RestoreQsoResponse` | Unary |
| `PurgeDeletedQsos` | `PurgeDeletedQsosRequest` | `PurgeDeletedQsosResponse` | Unary |
| `GetQso` | `GetQsoRequest` | `GetQsoResponse` | Unary |
| `ListQsos` | `ListQsosRequest` | `stream ListQsosResponse` | Server-streaming |
| `SyncWithQrz` | `SyncWithQrzRequest` | `stream SyncWithQrzResponse` | Server-streaming |
| `GetSyncStatus` | `GetSyncStatusRequest` | `GetSyncStatusResponse` | Unary |
| `ImportAdif` | `stream ImportAdifRequest` | `ImportAdifResponse` | Client-streaming |
| `ExportAdif` | `ExportAdifRequest` | `stream ExportAdifResponse` | Server-streaming |

#### LogQso

Creates a new QSO record in the local logbook.

**Behavior:**
1. Generate a new `local_id` (UUID v4).
2. Normalize the `worked_callsign` (trim whitespace, convert to uppercase).
3. Validate required fields: `worked_callsign`, `band`, `mode`, `utc_timestamp` must be present and non-default.
4. Stamp `station_callsign` from the active station profile.
5. Capture a `StationSnapshot` from the active station context and attach it to the QSO.
6. Set `created_at` and `updated_at` to the current UTC time.
7. Set `sync_status` to `SYNC_STATUS_LOCAL_ONLY`, clear QRZ linkage, and clear soft-delete state. These fields are engine-owned and caller values are ignored.
8. Persist the record via the storage backend.
9. If `sync_to_qrz=true`, immediately push the new record to QRZ Logbook (per-operation sync; see §7.3 below). On success, adopt the QRZ-assigned `qrz_logid`, set `sync_status=SYNC_STATUS_SYNCED`, and write the row back to storage. Populate `LogQsoResponse.sync_success=true`. On failure (no API key, network error, QRZ rejection), leave `sync_status=SYNC_STATUS_LOCAL_ONLY`, set `sync_success=false`, and put a human-readable message in `sync_error`. The local persist MUST succeed regardless. If sync was not requested, `sync_success=false` and `sync_error` is absent.
10. Return the generated `local_id` and any successful QRZ `qrz_logid`. The response envelope does not carry a `QsoRecord`; clients that need the stored row call `GetQso`.

**Error semantics:**
- `INVALID_ARGUMENT` — missing or invalid required fields.
- `FAILED_PRECONDITION` — no active station profile set (station context unavailable).
- `INTERNAL` — storage write failure.

#### UpdateQso

Updates an existing QSO record by `local_id`.

**Behavior:**
1. Look up the existing record by `local_id`.
2. Treat `request.qso` as a full replacement of all caller-owned QSO fields. Proto3 default values clear scalar fields, absent optional fields clear those fields, and `extra_fields` replaces the existing map. This RPC is not a patch operation because the request has no `FieldMask`.
3. Set `updated_at` to the current UTC time.
4. If the QSO was previously synced, set `sync_status` to `SYNC_STATUS_MODIFIED`. The engine — not the client — is the source of truth for `sync_status` and `qrz_logid` on update: round-tripping the existing values from a client request MUST NOT change them, and a client MUST NOT be able to advance a `LOCAL_ONLY` row to `SYNCED` (or claim a `qrz_logid`) via `UpdateQso`. The only state transitions the engine performs here are `SYNCED → MODIFIED` on edit, and (in step 6) `MODIFIED → SYNCED` on a successful per-op sync.
5. Persist the updated record.
6. If `sync_to_qrz=true`, immediately push the updated record to QRZ Logbook (per-operation sync; see §7.3 below). If the row already has a `qrz_logid`, use REPLACE so the same remote row is updated in place; otherwise INSERT. On success, write back the QRZ-assigned `qrz_logid` and `sync_status=SYNC_STATUS_SYNCED`, and set `UpdateQsoResponse.sync_success=true`. On failure, leave the local row in its current state (`SYNC_STATUS_MODIFIED` or `SYNC_STATUS_LOCAL_ONLY`), set `sync_success=false`, and put a human-readable message in `sync_error`. If sync was not requested, `sync_success=false` and `sync_error` is absent. The local persist MUST succeed regardless.
7. Return `success=true`. The response envelope does not carry a `QsoRecord`; clients that need the stored row call `GetQso`.

The engine preserves all engine-owned fields from the existing row: `local_id`, `station_snapshot`, `created_at`, `qrz_logid`, `qrz_bookid`, sync state, delete state, and QRZ linkage held in optional fields. Resolution is by `local_id` only. A missing ID is `INVALID_ARGUMENT`; an unknown ID is `NOT_FOUND`.

Clients that round-trip a complete `QsoRecord` while editing MUST preserve `RstReport.raw`.
Signed digital reports such as `+11` and `-10` are not legacy RST digit fields and MUST
round-trip exactly when an unrelated field, including QRZ enrichment, is changed.

**Error semantics:**
- `NOT_FOUND` — no QSO with the given `local_id`.
- `INVALID_ARGUMENT` — invalid field values.
- `INTERNAL` — storage write failure.

#### DeleteQso

Soft-deletes a QSO record by `local_id`. See §7.8 for the complete state-transition contract.

**Behavior:**
1. Look up the existing record by `local_id`.
2. Set its tombstone and, when requested and possible, queue a remote QRZ delete.
3. Return success without physically removing the row.

**Error semantics:**
- `NOT_FOUND` — no QSO with the given `local_id`.
- `INTERNAL` — storage delete failure.

#### GetQso

Retrieves a single QSO record by `local_id`.

**Behavior:**
- Return the full `QsoRecord` if found.

**Error semantics:**
- `NOT_FOUND` — no QSO with the given `local_id`.

#### ListQsos

Streams QSO records matching optional filter criteria.

**Behavior:**
- Apply filters from the request: time range (`after`/`before`), `callsign_filter`, `band_filter`, `mode_filter`, `contest_id`, `limit`, `offset`.
- `after` and `before` are **inclusive** boundaries (`utc_timestamp_ms >= after` and `utc_timestamp_ms <= before`). A QSO whose timestamp exactly matches a boundary MUST be included.
- `callsign_filter` is a case-insensitive substring match against **either** `station_callsign` **or** `worked_callsign`. The match is normalized via uppercase comparison so `"w1aw"` and `"W1AW"` behave identically regardless of database collation.
- Sort by `QsoSortOrder` (default: newest first).
- Stream one `ListQsosResponse` per matching QSO record.
- An empty logbook produces zero stream messages (not an error).

**Error semantics:**
- `INVALID_ARGUMENT` — malformed filter values.

#### SyncWithQrz

Initiates a bidirectional sync with the QRZ logbook API.

**Behavior:**

The sync follows the ordered lifecycle in §7.3:

1. Resolve QRZ `STATUS` and the logbook owner before downloading.
2. Under the review-safe conflict policy, pre-upload locally modified QRZ-linked rows before downloading so stale remote data cannot overwrite local corrections.
3. Download and reconcile remote QSOs.
4. Upload local-only and remaining modified rows, then push pending remote deletes.
5. Persist sync metadata from the `STATUS` result.

New records use a normal INSERT. Modified records use the documented `OPTION=REPLACE` value exactly; engines must not append a `LOGID` selector to that option. QRZ matches the duplicate from the ADIF identity fields and returns the affected `qrz_logid`. On success, update `sync_status` to `SYNC_STATUS_SYNCED` and record that returned value.

   **Previous-callsign rewrite (issue #337).** QRZ logbooks are bound to a single callsign and reject ADIF whose `STATION_CALLSIGN` does not match the logbook owner. Operators who have changed callsigns (e.g. KB7QOP → AE7XI) keep historical QSOs locally with the old call. To avoid those rejections, engines MUST:

   - Fetch the QRZ logbook owner callsign once per sync via QRZ `STATUS` before the download phase. Reuse the same result for the metadata phase. Do not call `STATUS` twice.
   - If `STATUS` fails or returns an empty owner, fall back to the cached `sync_metadata.qrz_logbook_owner`.
   - Per upload, when the resolved owner is non-empty and differs (case-insensitive, trimmed) from the QSO's `station_callsign`, rewrite the upload payload only:
     - Set the payload's `station_callsign` (and `station_snapshot.station_callsign`) to the owner.
     - If `station_snapshot.operator_callsign` is empty, set it to the original `station_callsign` so the historical operator-of-record is preserved as ADIF `OPERATOR`.
   - The local stored row MUST NOT be modified by the rewrite. Skip the rewrite entirely when `station_callsign` contains a `/` (portable / mobile / secondary suffix); those callsigns generally belong to a different QRZ logbook.

   *Known caveat:* on the next download, the merge logic for `SYNC_STATUS_SYNCED` rows is remote-wins, so the local `station_callsign` may drift to the book owner. The historical operator survives in `station_snapshot.operator_callsign`. Round-tripping the original via an `APP_QSORIPPER_ORIG_STATION_CALLSIGN` ADIF field on download is planned future work.

The metadata phase updates `sync_metadata` with the QRZ QSO count, last sync timestamp, and logbook owner callsign from the initial `STATUS` call. Because `STATUS` precedes upload, the persisted `qrz_qso_count` reflects the pre-upload count; the next cycle observes the post-upload count.

The server stream MUST contain a terminal response with `complete=true`. Engines SHOULD emit intermediate phase/progress responses as work is produced; intermediate granularity is implementation-defined.

**Response fields (`SyncWithQrzResponse`):**

| Field | Type | Description |
|---|---|---|
| `total_records` | `uint32` | Total records in scope for the sync pass. |
| `processed_records` | `uint32` | Records processed so far. |
| `uploaded_records` | `uint32` | Records successfully uploaded to QRZ. |
| `downloaded_records` | `uint32` | Records downloaded (inserted or merged) from QRZ. |
| `conflict_records` | `uint32` | Records flagged for conflict resolution. |
| `current_action` | `string` (optional) | Human-readable status string for progress display. |
| `complete` | `bool` | `true` on the terminal message; `false` on intermediate progress. |
| `error` | `string` (optional) | Accumulated error summary if any phase encountered failures. |
| `remote_deletes_pushed` | `uint32` | Number of pending remote deletes successfully pushed to QRZ (Phase 2.5). |
| `deletes_skipped_remote` | `uint32` | Number of download records skipped because they matched a soft-deleted local row (Phase 1). |
| `duplicate_replaces` | `uint32` | INSERT uploads retried successfully as REPLACE after QRZ reported a duplicate. |

**Error semantics:**
- `FAILED_PRECONDITION` — QRZ logbook credentials are not configured. This is returned before the first stream message.
- Failures discovered before the first stream message use the appropriate gRPC status (`UNAVAILABLE` or `INTERNAL`).
- After any stream message has been emitted, later integration, storage, parsing, and partial per-QSO failures are reported in a terminal response with `complete=true` and `error` populated. They do not change the transport status.

#### GetSyncStatus

Returns the current sync metadata state.

**Behavior:**
- Return live local and pending-upload counts plus `sync_metadata` values: QRZ QSO count, last sync timestamp, and logbook owner callsign.
- Report `is_syncing`, `next_sync`, `auto_sync_enabled`, and `last_sync_error` from the live scheduler/sync lifecycle.
- `auto_sync_enabled` is true only when periodic sync is enabled and a non-empty QRZ Logbook API key is available.
- If no sync has ever occurred, return zero remote count and no last-sync timestamp.

**Error semantics:**
- `INTERNAL` — storage read failure.

#### ImportAdif

Imports QSO records from a client-streamed ADIF payload.

**Behavior:**
1. Receive `ImportAdifRequest` messages, each containing an `AdifChunk` (a fragment of ADIF text).
2. Concatenate all chunks into a complete ADIF document.
3. Parse the ADIF document into individual QSO records.
4. For each parsed QSO, generate a `local_id`, normalize fields, and insert into storage.
5. Return `records_imported`, `records_skipped`, `records_updated`, and sanitized warning strings. The response has no separate total or error fields.

**Duplicate handling:** The engine should detect duplicates by matching on station callsign + worked callsign + band + mode + compatible submode/frequency + compatible UTC timestamp and skip them rather than creating duplicate entries. Timestamp matching must handle minute-precision ADIF sources (for example N1MM contest exports) by matching an existing second-precision QSO in the same displayed minute when either side is minute-precision. Small frequency drift between ADIF sources and QRZ-enriched rows should not create a duplicate when the contact identity otherwise matches.

WSJT-X ingestion, manual ADIF imports, and any future ADIF-based recovery inputs MUST use this same import path so duplicate behavior, active station-profile fallback, refresh semantics, and storage state remain consistent across sources.

**Error semantics:**
- `INVALID_ARGUMENT` — ADIF content is malformed or unparseable.
- `INTERNAL` — storage write failure.

#### ExportAdif

Streams the logbook as an ADIF document.

**Behavior:**
1. Query all QSO records (optionally filtered by the request parameters).
2. Serialize each QSO to ADIF format.
3. Stream `ExportAdifResponse` messages, each containing an `AdifChunk`.
4. When `include_header=true`, the first chunk contains the ADIF header. When false, no header is emitted.
5. Preserve `extra_fields` from imported QSOs for lossless round-trip.

**Error semantics:**
- `INTERNAL` — storage read or serialization failure.

### 3.3 LookupService

**Proto file:** `proto/services/lookup_service.proto`

Callsign lookup and DXCC enrichment.

#### RPCs

| RPC | Request | Response | Mode |
|---|---|---|---|
| `Lookup` | `LookupRequest` | `LookupResponse` | Unary |
| `StreamLookup` | `StreamLookupRequest` | `stream StreamLookupResponse` | Server-streaming |
| `GetCachedCallsign` | `GetCachedCallsignRequest` | `GetCachedCallsignResponse` | Unary |
| `GetDxccEntity` | `GetDxccEntityRequest` | `GetDxccEntityResponse` | Unary |
| `BatchLookup` | `BatchLookupRequest` | `BatchLookupResponse` | Unary |

#### Lookup

Performs a single callsign lookup.

**Behavior:**
1. Check the local lookup cache first. If a fresh, non-expired result exists, return it immediately with `cache_hit = true`.
2. If no cache hit, query the QRZ XML API.
3. Normalize the QRZ response into a `CallsignRecord`.
4. Cache the result in the `lookup_snapshots` store with an expiry timestamp.
5. Enrich with DXCC entity data if available.
6. Return a `LookupResult` containing the `CallsignRecord`, lookup state, latency, and cache hit status.
7. Populate `prior_qsos` and `prior_qso_total_count` from the local logbook for every non-`Loading` result (`Found`, `NotFound`, `Stale`, cache hit, batch entry). See [3.3.1 Prior QSO history](#331-prior-qso-history) below.

Desktop clients may use this RPC for both fast-entry and advanced QSO-card workflows. Those clients should debounce user typing and cancel stale in-flight UI requests, but they must still route callsign enrichment through this shared lookup service rather than duplicating QRZ XML logic in the UI layer.

Credentials supplied through setup remain process-session secrets and are never serialized. For restart availability, operators provide secrets through the documented environment variables or a secure configuration provider. See §6.3.

**Slash-call fallback:** If the callsign contains a `/` modifier (e.g., `W1AW/7`), and the full lookup fails, strip the modifier and retry with the base callsign. Populate `base_callsign`, `modifier_text`, and `modifier_kind` on the result.

**In-flight deduplication:** If a lookup for the same callsign is already in progress, coalesce the request rather than firing a duplicate QRZ query.

**Error semantics:**
- `NOT_FOUND` — callsign not found in QRZ (this is a valid result state, not a gRPC error; return `LookupState.LOOKUP_STATE_NOT_FOUND`).
- `UNAVAILABLE` — QRZ API unreachable (return `LookupState.LOOKUP_STATE_ERROR` in the result).
- Missing QRZ credentials are represented as `LookupState.LOOKUP_STATE_ERROR` with a sanitized authentication/configuration message, not a transport error.

#### StreamLookup

Performs a callsign lookup with streaming progress updates.

**Behavior:**
- Emits a `Loading` `LookupResult` immediately, **before** any cache or provider work, so clients get instant feedback that the request is in flight.
- When `skip_cache=true`, bypass fresh and stale cache reads and proceed directly from `Loading` to provider work.
- If a fresh cache entry exists, emits a `Found` (or `NotFound`) update and closes the stream.
- If a stale cache entry exists, emits a `Stale` update with the cached record, then continues to the provider.
- After the provider call completes, emits the final `Found`, `NotFound`, or `Error` update and closes the stream.
- Updates must be pushed to the transport as they are produced; engines must not buffer the full transition sequence before sending.

**Error semantics:** Same as `Lookup`. Per-request errors surface as a final `LookupResult` with state `LOOKUP_STATE_ERROR`; transport-level failures (e.g., the client dropping the stream) cancel the in-flight work without a panic.

#### GetCachedCallsign

Returns a cached lookup result without querying the external provider.

**Behavior:**
- Query the `lookup_snapshots` store for the requested callsign.
- If found and not expired, return the cached `CallsignRecord`.
- If not found or expired, return an empty result (not an error).

**Error semantics:**
- `INTERNAL` — storage read failure.

#### GetDxccEntity

Returns DXCC entity information for a given DXCC code.

**Behavior:**
- Look up the `DxccEntity` by numeric DXCC code from the engine's DXCC reference data.
- Return country name, continent, zones, and geographic data.
- Engines that derive entity data from the embedded ADIF DXCC table populate
  `dxcc_code`, `country_name`, `continent`, `cq_zone`, and `itu_zone`. Optional
  geographic fields (`utc_offset`, `latitude`, `longitude`, `notes`) remain unset
  unless the engine has access to a richer reference source (for example, QRZ DXCC
  XML).
- Prefix-based lookup (`GetDxccEntityRequest.prefix`) is reserved for a future engine
  release that integrates QRZ's prefix reduction algorithm. Engines that have not yet
  shipped prefix support must return `UNIMPLEMENTED` for that branch and `INVALID_ARGUMENT`
  when neither `dxcc_code` nor `prefix` is set.

**Error semantics:**
- `NOT_FOUND` — unknown DXCC code.
- `UNIMPLEMENTED` — request used the `prefix` branch and the engine has not yet
  implemented prefix-based DXCC resolution.
- `INVALID_ARGUMENT` — neither `dxcc_code` nor `prefix` was specified.

#### BatchLookup

Performs lookups for multiple callsigns in a single request.

**Behavior:**
- Accept a list of callsigns.
- Perform lookups for each (cache-first, then external).
- Return a list of `LookupResult` entries, one per input callsign.
- Order of results matches order of input callsigns.
- Engines should bound concurrency (the reference implementations cap parallel
  lookups at 5) and reuse the same `LookupCoordinator` path that powers the unary
  `Lookup` RPC so cache, debounce, and provider fallback semantics stay consistent.
- Empty input is valid and returns an empty `results` list.

**Error semantics:**
- Per-callsign errors are reported in individual `LookupResult` entries, not as top-level gRPC errors.
- `INTERNAL` — orchestration failure (e.g., a worker task panicked) before a per-callsign
  result could be produced.

#### 3.3.1 Prior QSO history

Every `LookupResult` returned by `Lookup`, `StreamLookup`, `GetCachedCallsign`, and `BatchLookup` (except the initial streaming `Loading` placeholder) must include the operator's prior contacts with the queried callsign:

- `prior_qsos` — most-recent-first list of `QsoHistoryEntry` records, capped at the engine's history limit (the reference implementations use 25). Each entry carries `local_id`, `utc_timestamp`, `band`, `mode`, `submode`, `frequency_hz`, `frequency_rx_hz`, and `contest_id`.
- `prior_qso_total_count` — total active prior QSOs with the worked callsign regardless of the cap, so clients can render "47 prior contacts (showing 25)".

History is recomputed on every response (never cached in the lookup snapshot store) so manual logbook edits are reflected immediately. The query is an exact, case-insensitive match on `worked_callsign` against the active (not soft-deleted) logbook rows; a substring match on `worked_callsign` would silently produce wrong history (`K7A` matching `K7AB`). The `Loading` streaming placeholder must NOT carry history; populating it would defeat its low-latency purpose.

Storage backends expose this via a dedicated `list_qso_history(worked_callsign, limit) -> { entries, total }` query rather than overloading the existing list-with-filter API. SQLite implementations should reuse the `idx_qsos_worked_callsign` index.

The history shape is forward-compatible with future contest-mode dupe checking: `(band, mode, contest_id)` is sufficient for every common contest dupe rule, and `contest_id` lets a future contest engine partition current-contest contacts from past contacts. Contest mode is not part of this contract; when added later it will arrive as additive `LookupRequest` and `LookupResult` fields with no break to existing clients.

### 3.4 RigControlService

**Proto file:** `proto/services/rig_control_service.proto`

Rig integration via the rigctld protocol.

#### RPCs

| RPC | Request | Response | Mode |
|---|---|---|---|
| `GetRigStatus` | `GetRigStatusRequest` | `GetRigStatusResponse` | Unary |
| `GetRigSnapshot` | `GetRigSnapshotRequest` | `GetRigSnapshotResponse` | Unary |
| `TestRigConnection` | `TestRigConnectionRequest` | `TestRigConnectionResponse` | Unary |

#### GetRigStatus

Returns the current rig connection status.

**Behavior:**
- Return a `RigConnectionStatus` value: `Connected`, `Disconnected`, `Error`, or `Disabled`.
- If rig control is disabled via configuration, return `Disabled`.

**Error semantics:**
- This RPC should always succeed. Connection problems are reported in the status value, not as gRPC errors.

#### GetRigSnapshot

Returns the most recent normalized logging snapshot from the rig.

**Behavior:**
- Return a `RigSnapshot` containing required transmit-side `frequency_hz`, `band`, `mode`, optional `submode`, provider diagnostic `raw_mode`, optional split receive-side `frequency_rx_hz` and `band_rx`, optional `tx_power_watts`, `status`, and `sampled_at`.
- In simplex operation, `frequency_hz` and `band` contain the current operating frequency and band. In split operation, they contain the transmit frequency and band when rigctld exposes it; `frequency_rx_hz` and `band_rx` contain the receive side.
- `tx_power_watts` is normalized from Hamlib's relative `RFPOWER` level through `power2mW`. Relative power values never cross the engine boundary.
- Split and power reads are capability tolerant. `RPRT` errors or unparseable values from optional commands leave the corresponding fields absent without failing the required frequency/mode snapshot.
- If the rig is disconnected or disabled, return a snapshot with appropriate status and no frequency/mode data.
- If the last snapshot is older than `QSORIPPER_RIGCTLD_STALE_THRESHOLD_MS`, mark it as stale.

**Error semantics:**
- This RPC should always succeed. Rig errors are reported in the snapshot's `status` and `error_message` fields.

#### TestRigConnection

Tests TCP connectivity to a rigctld instance, including unpersisted setup values.

**Behavior:**
1. Resolve host and port independently from request overrides, then configured values, then the engine defaults.
2. Reject an explicitly supplied blank host or port outside `1..=65535` with `INVALID_ARGUMENT`. Do not silently fall back from an invalid override.
3. Attempt a TCP connection to the resolved endpoint.
4. If the connection succeeds, send a basic command (for example, `f\n`) and verify a response.
5. Return success/failure with diagnostics.

**Error semantics:**
- Connection and protocol errors are reported in the response, not as gRPC errors.

#### Optional rig-control front door: standalone CatHub

`RigControlService` consumes rig state from a rigctld-compatible endpoint. On a multi-app
station the engine must **not** connect directly to the radio's serial port, because many
applications share one radio and direct contention produces VFO A/B oscillation, frequency
drift, and PTT conflicts.

The supported topology is a single independently installed CatHub daemon that owns
the radio serial port and fans it out to every client over its native protocol. The engine
points its `RigctldProvider` at one of the hub's read-only Hamlib NET endpoints
(`QSORIPPER_RIGCTLD_HOST`:`QSORIPPER_RIGCTLD_PORT`); the hub also serves other applications
(HDSDR/OmniRig, N1MM, ARCP-590, WSJT-X, Log4OM) on their own endpoints simultaneously. The
hub enforces the no-VFO-retargeting invariant (baseline polling never emits VFO-select/retarget
commands), serializes all writes, owns the radio's native push stream, and arbitrates PTT with
a single-owner lease and a hard transmit-time ceiling. The engine's behavior contracts above
are unchanged: it remains a read-mostly NET rigctl client and is agnostic to whether the
endpoint is CatHub or a bare `rigctld`. CatHub is not part of either engine and is never
required for ordinary QsoRipper logging.

The hub's read-only Hamlib NET endpoint implements the engine's optional logging probes:
`i`/`\get_split_freq`, `x`/`\get_split_mode`, `l RFPOWER`, and `2`/`\power2mW`. CatHub serves
these from its universal radio snapshot. The native TS-590 backend populates configured power
from `PC;`; the rigctld bridge forwards the optional probes to its private downstream rigctld.
When a backend cannot expose a value, CatHub returns `RPRT -11`, preserving the provider's
capability-tolerant behavior.

- CatHub implementation and design: <https://github.com/treitforge/cathub>.
- QsoRipper integration setup: `docs/integrations/cathub-setup.md`.

Native `ts590` endpoints support an opt-in, per-endpoint **single-VFO operating-VFO virtualization**
policy (`single_vfo = true`). When enabled, the endpoint never exposes the physical VFO letter:
it always presents the operating (receive) VFO as VFO A, mirroring `FA`/`FB` reads and writes
onto whichever VFO the radio is on, intercepting `FR`/`FT` VFO-select verbs, forcing the `IF;`
active-VFO digit (P10) and split to `0`, and re-presenting an A/B switch by pushing the new
operating VFO's `FA` + `MD` + `IF`. This makes single-VFO loggers — N1MM Logger+ in SO1V mode,
which otherwise warns "You should not use VFO B when configured for SO1V" and freezes — track
the radio seamlessly across A/B switches. It is **off by default** and must stay off for genuine
dual-VFO control panels such as ARCP-590. See design §8.4.2.

The **same `single_vfo` policy is available on `[[hamlib_net]]` (rigctld) endpoints** for
rigctld clients that, like N1MM SO1V, expect to receive on VFO A: WSJT-X stops decoding when
it sees VFO B as the active VFO, and Log4OM polls the fixed `\get_vfo_info VFOA` and would log
the inactive VFO's stale frequency on VFO B. On a `single_vfo` Hamlib NET endpoint the endpoint
uses a **strict** single-VFO contract: `get_vfo` reports `VFOA`; `get_vfo_info` resolves any
requested VFO to the operating VFO and reports `Split: 0`; `get_split_vfo` reports `0`/`VFOA`;
`set_split_vfo 1` is **rejected** (`RPRT -11`) because the presentation cannot model a real
split (use WSJT-X "Fake It"); and `\set_vfo ?` advertises only `VFOA`. Reads and writes already
target the operating VFO, so the frequency/mode are real on either physical VFO. The engine's
read-only endpoint leaves `single_vfo = false` so it logs the true operating VFO. Plain
`get_freq` / `get_mode` already read the operating VFO on every endpoint, which is why a logger that
polls only `f` (WSJT-X) tracks A/B even without virtualization, whereas one that polls
`get_vfo_info VFOA` (Log4OM) requires it.

Native TS-590 controllers that speak the radio's exact protocol — notably **ARCP-590**, Kenwood's
own control panel — get a dedicated **transparent mirror** dialect (`dialect = "ts590-transparent"`)
instead of the virtualizing `ts590` dialect. A transparent endpoint behaves as if it were wired
directly to the rig: every request except PTT and auto-information is forwarded to the radio
verbatim (`format_notification` returns nothing — the endpoint never consumes a synthesized frame),
and the radio's entire CAT stream — both modeled frames and unmodeled ones — is relayed back
byte-for-byte whenever the endpoint has auto-information enabled. Because the rig runs in AI2 and
echoes every change any client makes through the hub, a transparent controller stays perfectly in
sync with the radio's true VFO/frequency/mode/split state, eliminating the synthesis/snapshot
drift (stale A/B, frozen frequency) that a virtualizing view can accumulate for a client that
already knows the native protocol. The hub still owns the single physical port, so three things
stay hub-mediated even on a mirror endpoint: PTT (`TX`/`RX`) routes through the shared single-owner
lease; auto-information (`AI`) is virtualized per endpoint (the rig itself stays in hub-owned AI2);
and identity/keepalive reads (`ID;`/`PS;`) are answered locally so the controller's steady-state
heartbeat generates zero radio traffic. A lagged mirror endpoint is re-synced by re-presenting the
full current state as raw frames (`FA`/`FB`/`FR`/`FT`/`MD`/`IF`). `ts590-transparent` is
mutually exclusive with `single_vfo = true` (a mirror relays the radio's real dual-VFO stream and
cannot virtualize the operating VFO); the daemon rejects that combination at config validation.

To support transparent relay, the hub broadcasts a modeled native frame **twice** on its internal
event bus: once as a coalesced modeled change (consumed by virtualizing endpoints) and once as a
verbatim raw-native event (consumed only by transparent endpoints). Unmodeled frames continue to
broadcast as a single verbatim raw event. This keeps existing virtualizing dialects byte-for-byte
unchanged while giving a mirror endpoint the radio's exact stream.

The hub's Hamlib NET endpoints accept both the plain rigctld protocol (WSJT-X, N1MM, the engine)
and Hamlib's **Extended Response Protocol** (ERP). An ERP request prefixes the command with a
separator (`+` joins records with newlines; `;`, `|`, or `,` joins them on one line with that
character), and the reply echoes the long command name, emits labeled data records, and ends
with `RPRT x`. Log4OM-NG relies on ERP exclusively — it handshakes with `;V ?` (list supported
VFOs) and polls with `+\get_vfo_info VFOA` — so a conformant hub must implement ERP framing for
those shapes, not just the plain protocol, or Log4OM stays offline.

##### Managed and external configuration

CatHub owns its standalone configuration and defaults to `%APPDATA%\cathub\cathub.toml` on
Windows or `$XDG_CONFIG_HOME/cathub/cathub.toml` on Unix, overridable with
`CATHUB_CONFIG_PATH`. QsoRipper may instead manage CatHub from its unified per-user
`config.toml`. In that mode the daemon reads settings from `[cat_hub]` and the launcher passes
the unified path explicitly. CatHub accepts both layouts.

Because multiple components write the same file, every engine setup save is **merge-preserving**:
the engine replaces only its own top-level tables (`logbook`, `storage`, `station_profile`,
`station_profiles`, `qrz_xml`, `qrz_logbook`, `sync`, `rig_control`) and preserves all other
tables (`[cat_hub]`, `[launcher]`, and any future component sections). The conditional
`[wsjtx_ingest]` setup table is replaced only when `SaveSetup.wsjtx_ingest` is explicitly
supplied; omitted WSJT-X ingest settings are preserved verbatim. A conformant engine in any
language must implement this merge-preserving behavior rather than rewriting the whole file,
so it never clobbers another component's configuration.

`[cat_hub]` is a **managed-mode compatibility surface**: it is preserved verbatim on every save *unless*
the `SaveSetup` request explicitly carries a `cat_hub` (`CatHubSettings`) message, in which case
the engine performs a full-replacement rewrite of the section (see SetupService → SaveSetup). For
status and wizard display, the engine parses `[cat_hub]` **leniently and separately** from its own
configuration: a malformed or unknown-schema `[cat_hub]` yields an empty projection (and a logged
warning) but never fails engine load. A conformant engine must keep this read path isolated so the
daemon's section can never break engine startup, and must only rewrite `[cat_hub]` when an explicit
replacement is supplied. When CatHub is externally managed, QsoRipper stores only its rigctld and
CW broker client endpoints and does not write CatHub's standalone file. CatHub remains the
authoritative parser and validator for both layouts.

### 3.5 SpaceWeatherService

**Proto file:** `proto/services/space_weather_service.proto`

Cached space weather data from NOAA SWPC.

#### RPCs

| RPC | Request | Response | Mode |
|---|---|---|---|
| `GetCurrentSpaceWeather` | `GetCurrentSpaceWeatherRequest` | `GetCurrentSpaceWeatherResponse` | Unary |
| `RefreshSpaceWeather` | `RefreshSpaceWeatherRequest` | `RefreshSpaceWeatherResponse` | Unary |

#### GetCurrentSpaceWeather

Returns the most recently cached space weather snapshot.

**Behavior:**
- Return a `SpaceWeatherSnapshot` with K-index, A-index, solar flux, sunspot number, geomagnetic storm scale, and fetch timestamps.
- If space weather is disabled, return a snapshot with `SpaceWeatherStatus.SPACE_WEATHER_STATUS_ERROR` and a sanitized explanatory error. The protobuf enum has no disabled value.
- When enabled, refresh before returning if there is no cache or the cache is past the configured refresh interval. Otherwise return the cached snapshot.
- If refresh fails after a previous successful fetch, return the cached values with `SPACE_WEATHER_STATUS_STALE`. Without usable cached data, return `SPACE_WEATHER_STATUS_ERROR`.

**Error semantics:**
- This RPC should always succeed. Data unavailability is reported in the snapshot status.

#### RefreshSpaceWeather

Forces an immediate refresh from the NOAA APIs.

**Behavior:**
1. Fetch fresh data from NOAA SWPC endpoints (K-index JSON and solar indices text).
2. Parse and update the cached snapshot.
3. Return the new snapshot.

**Error semantics:**
- This RPC succeeds at the transport layer. Disabled configuration and NOAA failures are represented by an `ERROR` or `STALE` snapshot with a sanitized error message.

### 3.6 SetupService

**Proto file:** `proto/services/setup_service.proto`

First-run bootstrap and credential validation.

#### RPCs

| RPC | Request | Response | Mode |
|---|---|---|---|
| `GetSetupStatus` | `GetSetupStatusRequest` | `GetSetupStatusResponse` | Unary |
| `SaveSetup` | `SaveSetupRequest` | `SaveSetupResponse` | Unary |
| `GetSetupWizardState` | `GetSetupWizardStateRequest` | `GetSetupWizardStateResponse` | Unary |
| `ValidateSetupStep` | `ValidateSetupStepRequest` | `ValidateSetupStepResponse` | Unary |
| `TestQrzCredentials` | `TestQrzCredentialsRequest` | `TestQrzCredentialsResponse` | Unary |
| `TestQrzLogbookCredentials` | `TestQrzLogbookCredentialsRequest` | `TestQrzLogbookCredentialsResponse` | Unary |

#### GetSetupStatus

Returns whether initial setup has been completed.

**Behavior:**
- Check if a valid configuration and station profile exist.
- Return a `SetupStatus` indicating `complete` or `incomplete` with details about what is missing.
- Include current `wsjtx_ingest` settings when configured and live `wsjtx_ingest_status` diagnostics. A conformant engine that runs the WSJT-X ingest supervisor (required when ingestion is enabled — see "WSJT-X ingest runtime behavior") MUST populate `wsjtx_ingest_status` with its current live state.

#### SaveSetup

Persists setup configuration and station profile.

**Behavior:**
1. Validate all provided fields.
2. Persist non-secret configuration and station/storage settings to the config path. QRZ passwords and API keys supplied by the request are installed only in process memory and are never serialized.
3. Apply the configuration to the running engine (activate the station profile, enable integrations).
4. Mark setup as complete.

**CAT hub (`cat_hub`) management:**
- Serial and WinKeyer endpoints may include `application_transport`, the paired virtual endpoint
  opened by the client application. The hub opens only `transport`. Engines preserve and return
  the application-side value for setup guidance and reject an endpoint whose two transports are the
  same.
- The optional `cat_hub` field (`CatHubSettings`) is a compatibility adapter for a
  QsoRipper-managed standalone CatHub process. It writes only the unified file's `[cat_hub]`
  section. It never writes CatHub's external standalone file.
- The field is **full-replacement**: when present it is the complete desired `[cat_hub]`
  section. A `radio` (with a `backend`) is required and at least one endpoint (a
  `serial_endpoints` or `hamlib_net` entry) is required; the engine rewrites `[cat_hub]` from it.
- When `cat_hub` is omitted, the engine leaves any existing `[cat_hub]` section **untouched**
  (verbatim, including comments and unknown keys). Only an explicit `cat_hub` triggers a
  rewrite — so a routine save (e.g. updating QRZ credentials) never reserializes the daemon's
  configuration. This mirrors the conditional-ownership rule in the unified-configuration note.
- QsoRipper performs bounded input validation for its compatibility projection. CatHub remains
  authoritative and its `cathub config validate` command must be used for full semantic validation.
  The compatibility validation covers `backend` ∈ {ts590, rigctld,
  loopback}; radio `transport` ∈ {serial, tcp}; non-loopback serial radios require a `port`;
  endpoint `dialect` ∈ {ts590, ts590-transparent, ts2000}; endpoint names unique across endpoints and hamlib_net; endpoint
  transports distinct; hamlib_net binds distinct and in `host:port` form; an endpoint transport may
  not reuse the radio port. CAT endpoint permission tokens include `read`, `frequency_write`,
  `write`, `ptt`, and `config_write`; `frequency_write` grants tuning without other modeled
  mutations, while `write` grants all modeled writes. Violations return `INVALID_ARGUMENT`.

**WSJT-X ingest (`wsjtx_ingest`) management:**
- The optional `wsjtx_ingest` field (`WsjtxIngestSettings`) lets setup clients manage the
  engine-owned `[wsjtx_ingest]` section.
- Omit `wsjtx_ingest` to leave existing WSJT-X ingest settings unchanged. When present, the
  engine persists a replacement `[wsjtx_ingest]` table with `enabled`, `udp_enabled`,
  `udp_bind`, `adif_tail_enabled`, `adif_tail_path`, `poll_interval_ms`, and `sync_to_qrz`.
- Defaults are conservative: ingestion disabled; UDP bind `127.0.0.1:2237`; UDP enabled when
  ingestion is enabled and the field is omitted; ADIF tail disabled unless a path is supplied;
  poll interval defaults to a modest nonzero value; immediate QRZ sync disabled.
- Validation requires `udp_bind` to be `host:port` with port 1-65535 and `adif_tail_path` to
  be present when ADIF tailing is enabled. `poll_interval_ms=0` means use the engine default;
  positive values are accepted as supplied. Violations return `INVALID_ARGUMENT`.
- WSJT-X ingest is a first-class setup surface, not a TOML-only escape hatch. GUI setup wizard,
  GUI Settings, CLI `setup --status`, CLI `setup --from-env`, and the interactive CLI setup
  wizard must all project the same `WsjtxIngestSettings` fields. `setup --from-env` recognizes
  `QSORIPPER_WSJTX_INGEST_ENABLED`, `QSORIPPER_WSJTX_INGEST_UDP_ENABLED`,
  `QSORIPPER_WSJTX_INGEST_UDP_BIND`, `QSORIPPER_WSJTX_INGEST_ADIF_TAIL_ENABLED`,
  `QSORIPPER_WSJTX_INGEST_ADIF_TAIL_PATH`, `QSORIPPER_WSJTX_INGEST_POLL_INTERVAL_MS`, and
  `QSORIPPER_WSJTX_INGEST_SYNC_TO_QRZ`. CLI setup may also accept shorter non-runtime aliases
  for compatibility, but documentation and examples should prefer the canonical runtime names.

**WSJT-X ingest runtime behavior:**
- This runtime behavior is engine-neutral and REQUIRED: every conformant engine, regardless of
  implementation language, MUST provide WSJT-X ingestion when `wsjtx_ingest.enabled=true`. Both the
  Rust engine (`qsoripper-server`) and the .NET engine (`QsoRipper.Engine.DotNet`) implement this
  contract; a third-party engine must too. An engine MAY surface a documented capability flag if it
  cannot host long-running background work, but the default expectation is full parity.
- When enabled, a conformant engine starts a background supervisor with independent UDP and ADIF-tail
  inputs. The supervisor MUST NOT block normal logging or engine startup after configuration has
  been accepted. The supervisor MUST observe `wsjtx_ingest` settings changes applied through
  `SaveSetup` at runtime (start, stop, rebind, or re-point the tail without a process restart).
- UDP input listens for WSJT-X Logged ADIF datagrams and ignores non-logged WSJT-X messages. Raw
  ADIF and lightweight JSON-wrapped ADIF may be accepted by test/simulation helpers, but runtime
  framed WSJT-X packets must only import logged-QSO ADIF payloads.
- ADIF-tail input polls `wsjtx_log.adi`, scans from byte 0 on first run for startup recovery, and
  then imports only appended complete ADIF records while the engine is running. Complete-record
  detection must honor ADIF field lengths as character counts so literal `<EOR>` text inside a
  field value cannot move the cursor. It must not advance its cursor past an incomplete trailing
  record or a failed import. Startup replay is recovery-only: it imports missing QSOs and skips
  existing duplicates, including soft-deleted or locally edited rows that retain the original import
  fingerprint, rather than refreshing older ADIF rows over newer local edits.
- Both inputs feed `LogbookEngine::import_adif_qsos`. Duplicate UDP events, repeated ADIF scans,
  and later manual imports must not create duplicate QSOs.
- `WsjtxIngestStatus` reports enabled/running state, UDP/tail health, last event time, last
  imported callsign/local id, imported/updated/skipped/duplicate counters, parse errors, last
  ingestion error, and last QRZ sync result.
- When `sync_to_qrz=true`, imported or refreshed WSJT-X QSOs use the same per-operation QRZ upload
  semantics as `LogQso(sync_to_qrz=true)`: success writes QRZ metadata back locally, while failure
  leaves the local QSO persisted and retryable and records an actionable diagnostic. The async
  writeback must patch only QRZ metadata and sync state onto the current local row so a stale upload
  task cannot overwrite newer operator edits. QRZ upload work must not block the local UDP/tail
  ingestion loops.

**Error semantics:**
- `INVALID_ARGUMENT` — invalid or missing required setup fields, an invalid `cat_hub` section, or invalid `wsjtx_ingest` settings.
- `INTERNAL` — failed to persist configuration.

#### GetSetupWizardState

Returns the current state of the setup wizard for multi-step UIs.

**Behavior:**
- Return the list of `SetupWizardStep` values with their completion status (`SetupWizardStepStatus`).
- The ordered enum surface is `LOG_FILE`, `STATION_PROFILES`, `QRZ_INTEGRATION`, `CAT_HUB`, and `REVIEW`.
- The CAT hub step (`SETUP_WIZARD_STEP_CAT_HUB`) is optional and always reported complete; it
  exposes the current `[cat_hub]` projection for display and editing.

#### ValidateSetupStep

Validates a single step of the setup wizard without persisting.

**Behavior:**
- Accept a `SetupWizardStep` identifier and field values.
- Validate the fields for that step.
- Return validation results per field (`SetupFieldValidation`).
- The station-profile wizard step requires profile name, station callsign, operator callsign, and grid because setup-completion guidance needs a complete operating identity. This is stricter than `SaveStationProfile`, which requires only profile name and station callsign.

**Error semantics:**
- `INVALID_ARGUMENT` — unknown step identifier.

#### TestQrzCredentials

Tests QRZ XML API credentials by performing a real lookup request.

**Behavior:**
1. Send an authenticated QRZ XML lookup for the stable test callsign `W1AW` with the provided username and password.
2. Return success if authentication succeeds and the response is valid.
3. Return failure with a descriptive message if authentication fails.

**Error semantics:**
- Authentication, rejection, and network failures are reported as `success=false` with a sanitized response message. Malformed requests use `INVALID_ARGUMENT`.

#### TestQrzLogbookCredentials

Tests QRZ logbook API credentials.

**Behavior:**
1. Send a `STATUS` request to the QRZ logbook API with the provided API key.
2. Return success if the API responds with valid logbook metadata.
3. Return failure with a descriptive message otherwise.

**Error semantics:**
- Authentication, rejection, and network failures are reported as `success=false` with a sanitized response message. Malformed requests use `INVALID_ARGUMENT`.

### 3.7 StationProfileService

**Proto file:** `proto/services/station_profile_service.proto`

Manages station identity profiles and session overrides.

#### RPCs

| RPC | Request | Response | Mode |
|---|---|---|---|
| `ListStationProfiles` | `ListStationProfilesRequest` | `ListStationProfilesResponse` | Unary |
| `GetStationProfile` | `GetStationProfileRequest` | `GetStationProfileResponse` | Unary |
| `SaveStationProfile` | `SaveStationProfileRequest` | `SaveStationProfileResponse` | Unary |
| `DeleteStationProfile` | `DeleteStationProfileRequest` | `DeleteStationProfileResponse` | Unary |
| `SetActiveStationProfile` | `SetActiveStationProfileRequest` | `SetActiveStationProfileResponse` | Unary |
| `GetActiveStationContext` | `GetActiveStationContextRequest` | `GetActiveStationContextResponse` | Unary |
| `SetSessionStationProfileOverride` | `SetSessionStationProfileOverrideRequest` | `SetSessionStationProfileOverrideResponse` | Unary |
| `ClearSessionStationProfileOverride` | `ClearSessionStationProfileOverrideRequest` | `ClearSessionStationProfileOverrideResponse` | Unary |

#### ListStationProfiles

Returns all saved station profiles.

**Behavior:**
- Return a list of `StationProfileRecord` entries with their profile names and data.

#### GetStationProfile

Returns a single station profile by name.

**Error semantics:**
- `NOT_FOUND` — no profile with the given name.

#### SaveStationProfile

Creates or updates a station profile.

**Behavior:**
1. Validate required fields: profile name, station callsign.
2. Persist the profile.
3. If this is the first profile and no active profile is set, automatically activate it.

**Error semantics:**
- `INVALID_ARGUMENT` — missing or invalid fields.

#### DeleteStationProfile

Deletes a station profile by name.

**Error semantics:**
- `NOT_FOUND` — no profile with the given name.
- `FAILED_PRECONDITION` — cannot delete the active profile while it is active.

#### SetActiveStationProfile

Activates a saved profile as the current station context.

**Behavior:**
- Load the named profile and set it as the active station context.
- All subsequent `LogQso` calls will stamp QSOs with this profile's station data.

**Error semantics:**
- `NOT_FOUND` — no profile with the given name.

#### GetActiveStationContext

Returns the currently active station context.

**Behavior:**
- Return an `ActiveStationContext` containing the resolved station profile (accounting for any session override) and the profile name.
- If no active profile is set, return an empty context (not an error).

#### SetSessionStationProfileOverride

Temporarily overrides the active station profile for the current session.

**Behavior:**
- Accept a complete, valid `StationProfile` replacement. The current protobuf message is not a patch and has no field-presence model for individual scalar overrides.
- Resolve the active context from the session profile as a full replacement of the saved active profile. Omitted optional sections and values are cleared for the session.
- The override persists until explicitly cleared or the engine restarts.

#### ClearSessionStationProfileOverride

Removes the session override, reverting to the base active profile.

### 3.8 ContestCalendarService

**Proto file:** `proto/services/contest_calendar_service.proto`

Cached contest calendar metadata for operator "what contest is active?" queries.

#### RPCs

| RPC | Request | Response | Mode |
|---|---|---|---|
| `GetActiveContests` | `GetActiveContestsRequest` | `GetActiveContestsResponse` | Unary |
| `RefreshContestCalendar` | `RefreshContestCalendarRequest` | `RefreshContestCalendarResponse` | Unary |

#### GetActiveContests

Returns contests whose UTC window overlaps the requested time and lookahead window. If `at_utc` is omitted, the engine uses its current UTC time. `band` and `mode` filters are optional. If a contest entry does not include band or mode metadata, it only matches a band or mode filter when `include_partial_matches=true`.

**Behavior:**
- Use cached calendar data when it is fresh.
- Refresh the provider cache when there is no cache or the refresh interval has elapsed.
- Return stale cached data with `ContestCalendarStatus.CONTEST_CALENDAR_STATUS_STALE` if refresh fails after a previous successful fetch.
- Separate provider/cache status from data completeness. `ContestCalendarStatus` describes current/stale/error/disabled state, while `ContestDetailsStatus` describes whether a contest has metadata-only, partial, or full details.
- Preserve source attribution through `source_name` and `source_url`.

**Source limitation:**
The built-in WA7BNM provider uses the public RSS calendar feed and treats it as a metadata source. RSS-derived entries include name, UTC start/end, source link, and metadata-only status. The runtime engine must not scrape WA7BNM HTML detail pages for exchange, band, mode, or rules data. Richer details require an authorized data source, a user-configured source, a curated local catalog, or a reviewed catalog generated offline.

**Local details catalog:**
The engine loads a reviewed JSON catalog from `QSORIPPER_CONTEST_CALENDAR_DETAILS_PATH`, or from `data\contest-calendar\contest-details.json` when the variable is unset and that file exists. It uses the catalog to enrich RSS entries with known bands, modes, exchange text, rules URL, and `ContestDetailsStatus`. Catalog entries may match by `contestId`, `sourceUrl`, or normalized contest `name`. This catalog is offline input to the engine. Generated catalog candidates must be reviewed before they become authoritative engine input, and tools that produce them must respect rules-site terms.

**Error semantics:**
- This RPC should usually succeed. Data unavailability is reported through `ContestCalendarStatus` and `error_message`.
- Proto3 accepts unknown numeric enum values. The service MUST explicitly reject unknown band or mode values with `INVALID_ARGUMENT`.

#### RefreshContestCalendar

Forces an immediate provider refresh and returns the resulting contest entries plus status metadata.

**Behavior:**
1. Fetch fresh data from the configured contest calendar provider.
2. Parse and normalize contest entries into `ContestCalendarEntry`.
3. Update the cache on success.
4. On provider failure, return stale cached data when available; otherwise return `CONTEST_CALENDAR_STATUS_ERROR` or `CONTEST_CALENDAR_STATUS_DISABLED`.

### 3.9 CwService

**Proto file:** `proto/services/cw_service.proto`

Engine-owned CW macro expansion and keyer dispatch for contest workflows. The service is intentionally independent from UI key bindings: clients may map F-keys, buttons, or CLI commands to named macros, but the engine owns expansion and backend dispatch.

#### RPCs

| RPC | Request | Response | Mode |
|---|---|---|---|
| `ListCwMacros` | `ListCwMacrosRequest` | `ListCwMacrosResponse` | Unary |
| `SendCwMacro` | `SendCwMacroRequest` | `SendCwMacroResponse` | Unary |
| `SendCwText` | `SendCwTextRequest` | `SendCwTextResponse` | Unary |
| `AbortCw` | `AbortCwRequest` | `AbortCwResponse` | Unary |
| `SetCwSpeed` | `SetCwSpeedRequest` | `SetCwSpeedResponse` | Unary |
| `GetCwKeyerStatus` | `GetCwKeyerStatusRequest` | `GetCwKeyerStatusResponse` | Unary |

#### Built-in macros

The first conformant implementation exposes built-in named macros. Persisted user macro profiles are a future additive feature. The default macro names are stable identifiers, not keyboard bindings:

| Name | Template | Purpose |
|---|---|---|
| `cq` | `CQ TEST {MYCALL} {MYCALL}` | Call CQ |
| `exchange` | `{HISCALL} {RST} {EXCH}` | Send current exchange |
| `tu` | `TU {MYCALL}` | Complete QSO |
| `repeat` | `{HISCALL} {RST} {EXCH}` | Repeat exchange |

#### Macro grammar

CW macro templates are ASCII text with braced tokens. Token names are case-insensitive. Unknown tokens are rejected with `INVALID_ARGUMENT`; they are not passed through literally. Literal braces are escaped by doubling them: `{{` emits `{` and `}}` emits `}`. Unmatched braces are rejected with `INVALID_ARGUMENT`. Expansion preserves all other whitespace and punctuation.

Defined tokens:

| Token | Source |
|---|---|
| `{MYCALL}` | Active station context station callsign. If no active station callsign exists, reject with `FAILED_PRECONDITION`. |
| `{HISCALL}` | `CwSendContext.worked_callsign`, normalized to uppercase. |
| `{RST}` | `CwSendContext.rst`; default `599` when omitted. |
| `{EXCH}` | `CwSendContext.exchange`. |
| `{NR}` | `CwSendContext.serial`, formatted as decimal with no padding for the first slice. Future contest sessions may assign this engine-side. |

#### Backend semantics

| Backend | Semantics |
|---|---|
| `CW_KEYER_BACKEND_NULL` | Always accepts valid expanded text and performs no hardware I/O. This backend exists for CI, development, and CLI smoke tests. |
| `CW_KEYER_BACKEND_WINKEYER` | Sends expanded text and control commands to a serial-connected WinKeyer-compatible hardware keyer. The engine reports serial connection errors explicitly and never silently falls back to null when WinKeyer is selected. |
| `CW_KEYER_BACKEND_CWDAEMON` | Reserved for future UDP cwdaemon support. cwdaemon is a mostly Linux-oriented background daemon that accepts CW text over UDP and performs keying through hardware it controls. UDP is best-effort, so completion cannot be proven by the engine. |

`SendCwMacro` and `SendCwText` return the expanded text and a `CwSendState`. `ACCEPTED` means the configured backend accepted the command for dispatch. `COMPLETED` may only be returned by a backend that can prove completion. `AbortCw` sends the backend's abort or clear-buffer command when supported and returns `ABORT_REQUESTED`.

The WinKeyer backend is an engine-lifetime, serialized hardware session. The engine opens the serial port and sends Host Open (`00 02`) on the first status or control operation, retains the returned firmware revision, and reuses that connection for later RPCs. All keyer commands run through one backend worker or lock so concurrent RPCs cannot interleave serial bytes. On shutdown, initialization failure after Host Open, or an I/O failure, the engine makes a best-effort Host Close (`00 03`) before releasing the port. It never opens and abandons a new host session for each RPC.

Hardware transmission is opt-in. `SendCwMacro` and `SendCwText` reject with `FAILED_PRECONDITION` unless `QSORIPPER_CW_TRANSMIT_ENABLED=true`; status and speed operations remain available for setup diagnostics. Each accepted hardware send arms the configured safety ceiling. At expiry the engine requests WinKeyer status (`15`) and clears the input buffer (`0A`) only when the BUSY bit remains set. `AbortCw` cancels the active watchdog and clears the buffer immediately. A subsequent send replaces the previous watchdog deadline.

CW configuration is read from the shared `[cw_keying]` table in `config.toml`. Each corresponding `QSORIPPER_CW_*` environment variable overrides only that TOML key. Both engines apply the same precedence and validation at startup. Setup saves preserve the entire `[cw_keying]` table verbatim because the CW service does not yet expose a configuration mutation RPC.

`CwKeyerStatus` reports the configured backend, probed availability, retained speed, optional port, optional last error, hardware transmit gate, maximum transmit duration, and optional firmware revision. For WinKeyer, status performs a real connection probe instead of inferring availability from configuration alone.

#### Error semantics

- `NOT_FOUND` — unknown macro name.
- `INVALID_ARGUMENT` — invalid speed, malformed macro text, unknown token, or missing token context.
- `FAILED_PRECONDITION` — no active station context for `{MYCALL}`, the configured backend is unavailable, or hardware text transmission was requested while the explicit transmit gate is disabled.
- `UNAVAILABLE` — keyer I/O failure where retrying may help.
- `INTERNAL` — unexpected backend failure.

### 3.10 GreatCircleService

**Proto file:** `proto/services/great_circle_service.proto`

Computes great-circle geodesics (distance, bearing, sample arc) between two
points on the sphere. Used by clients to render azimuthal map projections,
beam-heading indicators, and contact-distance displays.

#### RPCs

| RPC | Request | Response | Mode |
|---|---|---|---|
| `ComputeGreatCircle` | `ComputeGreatCircleRequest` | `ComputeGreatCircleResponse` | Unary |

#### ComputeGreatCircle

Computes the great-circle path from `origin` to `target`.

**Request fields:**
- `GeoReference origin` — required; must contain `coordinates` or `maidenhead`.
- `GeoReference target` — required; same constraint.
- `uint32 sample_count` — `0` selects the engine default (64); valid range is `2..=512`; values of `1` or `> 512` are rejected.

**`GeoReference` resolution:**
- If `coordinates` is present, it is used directly.
- Otherwise the engine resolves `maidenhead` (4, 6, or 8-character locator, case-insensitive) to the locator's center coordinates.
- If neither is set, `INVALID_ARGUMENT` is returned.

**Response fields:**
- `GreatCirclePath path`:
  - `GeoPoint origin`, `target` — resolved coordinates (after Maidenhead → lat/lon expansion).
  - `double distance_km` — great-circle distance using a spherical Earth model with `R = 6371.0088 km`.
  - `optional double initial_bearing_deg`, `final_bearing_deg` — true-north bearings in `[0, 360)`. Both are absent when origin and target are the same point or antipodal (the great circle is non-unique in those cases).
  - `repeated GeoPoint samples` — `sample_count` evenly-spaced points along the geodesic, including both endpoints.

**Error semantics:**
- `INVALID_ARGUMENT` — missing reference, unresolvable Maidenhead locator, latitude/longitude out of range, NaN/Inf, or `sample_count` out of range.

**Behavior:**
- Computation is purely deterministic: identical inputs return bit-identical outputs across engine restarts and across the Rust and .NET implementations within `~1e-3` km / `~1e-3°`.
- No external I/O; the RPC must complete entirely from in-process math.
- This service is required, not optional. Engines that lack the math should still expose the RPC and return `UNIMPLEMENTED`, but the reference Rust and .NET engines both implement it.

### 3.11 DeveloperControlService

**Proto file:** `proto/services/developer_control_service.proto`

Developer-only live configuration overrides. Not intended for end-user UIs.

#### RPCs

| RPC | Request | Response | Mode |
|---|---|---|---|
| `GetRuntimeConfig` | `GetRuntimeConfigRequest` | `GetRuntimeConfigResponse` | Unary |
| `ApplyRuntimeConfig` | `ApplyRuntimeConfigRequest` | `ApplyRuntimeConfigResponse` | Unary |
| `ResetRuntimeConfig` | `ResetRuntimeConfigRequest` | `ResetRuntimeConfigResponse` | Unary |

#### GetRuntimeConfig

Returns the full runtime configuration snapshot.

**Behavior:**
- Return a discovery-driven `RuntimeConfigSnapshot` containing fields the engine guarantees it can validate and hot-apply. Engines are not required to advertise every startup-only setting.
- Each `RuntimeConfigDefinition` supplies type, description, allowed values, secret metadata, and `default_value` when a canonical default exists.
- Each `RuntimeConfigValue` reports a redacted display value and its `RuntimeConfigValueSource`: `DEFAULT`, `BASE_CONFIG`, `SESSION_OVERRIDE`, or `RUNTIME_OVERRIDE`.
- Secret values are never returned. A configured secret sets `has_value=true`, `redacted=true`, and a redacted `display_value`.
- Engines supporting QRZ lookup, QRZ Logbook, and rig control MUST advertise the common hot-apply keys `QSORIPPER_QRZ_XML_USERNAME`, `QSORIPPER_QRZ_XML_PASSWORD`, `QSORIPPER_QRZ_LOGBOOK_API_KEY`, and `QSORIPPER_RIGCTLD_ENABLED`. Other definitions may be implementation-specific.
- Startup-only settings such as the active storage backend are reported elsewhere and MUST NOT be advertised here unless the engine can safely hot-apply them.

#### ApplyRuntimeConfig

Applies one or more runtime configuration mutations.

**Behavior:**
1. Accept a list of `RuntimeConfigMutation` entries (field name + new value + mutation kind).
2. Validate each mutation against the field's allowed values and type.
3. Apply the mutations to the running engine state. A successful response guarantees the active integration observes the new value without restart.
4. Return the updated configuration snapshot.

**Error semantics:**
- `INVALID_ARGUMENT` — unknown field name, invalid value, or type mismatch.

#### ResetRuntimeConfig

Resets all runtime configuration overrides to the startup/base configuration.

**Behavior:**
- Discard all runtime mutations.
- Reveal the current base value established from persisted non-secret configuration, environment variables, process-session setup secrets, and defaults. Reset does not reread files or the process environment.
- Return the reset configuration snapshot.

### 3.12 StressControlService (Optional)

**Proto file:** `proto/services/stress_control_service.proto`

Load testing control plane. Implementation is optional; engines that do not support stress testing should return `UNIMPLEMENTED` for all RPCs.

#### RPCs

| RPC | Request | Response | Mode |
|---|---|---|---|
| `StartStressRun` | `StartStressRunRequest` | `StartStressRunResponse` | Unary |
| `StopStressRun` | `StopStressRunRequest` | `StopStressRunResponse` | Unary |
| `GetStressRunStatus` | `GetStressRunStatusRequest` | `GetStressRunStatusResponse` | Unary |
| `StreamStressRunEvents` | `StreamStressRunEventsRequest` | `stream StreamStressRunEventsResponse` | Server-streaming |
| `ListStressProfiles` | `ListStressProfilesRequest` | `ListStressProfilesResponse` | Unary |

#### StartStressRun

Starts a load test run with the specified profile and configuration.

#### StopStressRun

Stops a running stress test.

#### GetStressRunStatus

Returns the current state of a stress run (idle, running, completed, failed).

#### StreamStressRunEvents

Streams real-time events (log entries, metrics, vector state changes) from a running stress test.

#### ListStressProfiles

Returns available stress test profiles.

---

## 4. Storage Contract

### 4.1 Backend Selection

The engine must support at least two storage backends, selectable at startup:

| Backend | Env Value | Description |
|---|---|---|
| **Memory** | `memory` | In-process, non-persistent. Default. |
| **SQLite** | `sqlite` | File-backed, persistent across restarts. |

Selection is controlled by the `QSORIPPER_STORAGE_BACKEND` environment variable. If unset, the engine defaults to `memory`.

### 4.2 In-Memory Backend

- All data is stored in-process data structures (maps, vectors).
- Data is lost on engine restart.
- Suitable for testing, development, and conformance runs.
- Must implement the full `EngineStorage` trait (logbook + lookup snapshots).

### 4.3 SQLite Backend

- Data is persisted to a SQLite file at the path specified by `QSORIPPER_SQLITE_PATH` (or `QSORIPPER_STORAGE_PATH`).
- Must use WAL journal mode for concurrent read/write performance.
- Must set `busy_timeout = 5000` (5 seconds) to handle transient lock contention.
- Must enable `foreign_keys = ON`.
- Must implement the full `EngineStorage` trait.

### 4.4 Schema

The SQLite backend uses the following schema (defined in `src/rust/qsoripper-storage-sqlite/src/migrations/0001_initial.sql`):

#### `qsos` table

QSO records are stored as protobuf binary blobs in a `record` column, with indexed extraction columns for efficient querying.

| Column | Type | Constraints | Description |
|---|---|---|---|
| `local_id` | `TEXT` | `PRIMARY KEY NOT NULL` | UUID v4 identifier |
| `qrz_logid` | `TEXT` | | QRZ log ID after sync |
| `qrz_bookid` | `TEXT` | | QRZ book ID after sync |
| `station_callsign` | `TEXT` | `NOT NULL` | Station callsign (indexed) |
| `worked_callsign` | `TEXT` | `NOT NULL` | Worked callsign (indexed) |
| `utc_timestamp_ms` | `INTEGER` | | UTC timestamp in milliseconds (indexed) |
| `band` | `INTEGER` | `NOT NULL` | Proto Band enum value (indexed) |
| `mode` | `INTEGER` | `NOT NULL` | Proto Mode enum value (indexed) |
| `contest_id` | `TEXT` | | Contest identifier (indexed) |
| `created_at_ms` | `INTEGER` | | Creation timestamp in ms |
| `updated_at_ms` | `INTEGER` | | Last update timestamp in ms |
| `sync_status` | `INTEGER` | `NOT NULL` | Proto SyncStatus enum value (indexed) |
| `record` | `BLOB` | `NOT NULL` | Full QsoRecord serialized as protobuf |

**Design rationale:** The `record` BLOB stores the complete proto-serialized `QsoRecord`. Extraction columns duplicate key fields for efficient SQL-level filtering and indexing. When reading, the engine deserializes from the `record` BLOB to get the full domain object.

#### `sync_metadata` table

Singleton row tracking QRZ logbook sync state.

| Column | Type | Constraints | Description |
|---|---|---|---|
| `id` | `INTEGER` | `PRIMARY KEY CHECK (id = 1)` | Always 1 (singleton) |
| `qrz_qso_count` | `INTEGER` | `NOT NULL DEFAULT 0` | QSO count reported by QRZ |
| `last_sync_ms` | `INTEGER` | | Last sync timestamp in ms |
| `qrz_logbook_owner` | `TEXT` | | QRZ logbook owner callsign |

A seed row `(1, 0)` is inserted on creation.

#### `lookup_snapshots` table

Cached callsign lookup results.

| Column | Type | Constraints | Description |
|---|---|---|---|
| `callsign` | `TEXT` | `PRIMARY KEY NOT NULL` | Normalized callsign |
| `result` | `BLOB` | `NOT NULL` | Proto-serialized `LookupResult` |
| `stored_at_ms` | `INTEGER` | `NOT NULL` | Cache timestamp in ms |
| `expires_at_ms` | `INTEGER` | | Cache expiry timestamp in ms |

### 4.5 Migration Strategy

- Migrations are embedded in the engine binary and applied at startup.
- Each migration is a numbered SQL file (e.g., `0001_initial.sql`).
- The engine must track which migrations have been applied and only run new ones.
- Migrations must be idempotent where possible.
- Schema changes must be backward-compatible: add columns with defaults, never remove columns in active use.

### 4.6 Storage Trait

All backends must implement the `EngineStorage` trait, which decomposes into:

- **`LogbookStore`** — `insert_qso`, `update_qso`, `delete_qso`, `get_qso`, `list_qsos`, `qso_counts`, `get_sync_metadata`, `upsert_sync_metadata`
- **`LookupSnapshotStore`** — `get_lookup_snapshot`, `upsert_lookup_snapshot`, `delete_lookup_snapshot`
- **`backend_name()`** — returns the backend identifier string (e.g., `"memory"`, `"sqlite"`)

---

## 5. Integration Contracts

### 5.1 QRZ XML Lookup

**API endpoint:** `https://xmldata.qrz.com/xml/current/`

**Authentication:** Session-key based. The engine must:
1. Send a login request with `username` and `password` parameters.
2. Extract the `<Key>` element from the XML response.
3. Use the session key for subsequent lookup requests.
4. Handle session expiry by re-authenticating when the API returns an auth error.
5. Retry with a fresh session key on the first failure before reporting an error.

**Request format:** HTTP GET with query parameters:
- Login: `?username=<user>&password=<pass>&agent=<user_agent>`
- Lookup: `?s=<callsign>&callsign=<callsign>&agent=<user_agent>`

**Response format:** XML with namespace `http://xmldata.qrz.com`. The engine must use namespace-aware XML parsing. Key elements:
- `<Callsign>` — contains all station data fields
- `<Session>` — contains session key, error messages, subscription status

**Normalization:** Map QRZ XML fields to `CallsignRecord` proto fields immediately at the provider edge. Never expose raw XML structures beyond the QRZ adapter.

**Rate limiting:** Respect QRZ's rate limits. Implement exponential backoff on HTTP 429 or repeated failures.

**Credential env vars:**
- `QSORIPPER_QRZ_XML_USERNAME`
- `QSORIPPER_QRZ_XML_PASSWORD`
- `QSORIPPER_QRZ_USER_AGENT`
- `QSORIPPER_QRZ_XML_BASE_URL` (override for testing)

### 5.2 QRZ Logbook Sync

**API endpoint:** `https://logbook.qrz.com/api`

**Authentication:** API key passed as a `KEY` parameter in every request.

**Request format:** HTTP POST, form-encoded body.

**Operations:**

| Action | Parameters | Description |
|---|---|---|
| `STATUS` | `KEY`, `ACTION=STATUS` | Returns logbook metadata (QSO count, owner) |
| `FETCH` | `KEY`, `ACTION=FETCH`, `OPTION=ALL` | Downloads all QSOs as ADIF |
| `INSERT` | `KEY`, `ACTION=INSERT`, `ADIF=<record>` | Uploads a single QSO |
| `DELETE` | `KEY`, `ACTION=DELETE`, `LOGID=<id>` | Deletes a QSO by logid |

**Response format:** Ampersand-delimited key-value pairs. Check `RESULT` field for success/failure.

**ADIF interchange:** The logbook API uses ADIF format for QSO data. The engine must serialize/deserialize ADIF at this boundary. When exporting numeric ADIF fields for QRZ uploads, engines must normalize them to QRZ-compatible values; for example, `TX_PWR` must be sent as a numeric watt value and omitted if the local value cannot be normalized safely.

**Credential env vars:**
- `QSORIPPER_QRZ_LOGBOOK_API_KEY`
- `QSORIPPER_QRZ_LOGBOOK_BASE_URL` (override for testing)

### 5.3 Rig Control (rigctld)

**Protocol:** TCP text-based protocol (Hamlib rigctld).

**Connection:** TCP socket to `QSORIPPER_RIGCTLD_HOST`:`QSORIPPER_RIGCTLD_PORT` (default `localhost:4532`).

**Commands:**

| Command | Response | Description |
|---|---|---|
| `f\n` | Frequency in Hz (e.g., `14074000`) | Get current frequency |
| `m\n` | Mode and passband (e.g., `USB\n2400`) | Get current mode |
| `s\n` | Split enabled and TX VFO | Detect split operation |
| `i\n` | Split TX frequency in Hz | Get transmit frequency when split |
| `x\n` | Split TX mode and passband | Get transmit mode when split |
| `l RFPOWER\n` | Relative power from 0 through 1 | Get configured transmitter output level |
| `2 <level> <frequency> <mode>\n` | Power in milliwatts | Convert relative power to an absolute value |

**Polling model:**
- The engine polls rigctld at a configurable interval.
- The provider keeps one serialized TCP session and reads required frequency/mode plus supported optional split/power state on that session for
  each poll. It MUST NOT create a new TCP connection for every snapshot.
- A timeout, EOF, or transport failure discards the session and permits one bounded reconnect and
  retry. If that retry fails, the rig status transitions to `Disconnected` or `Error`.
- Each successful poll constructs a `RigSnapshot` and caches it.
- Optional command failures do not change the connection status and do not discard required fields.
- If the snapshot is older than `QSORIPPER_RIGCTLD_STALE_THRESHOLD_MS`, it is marked stale.

**Read timeout:** `QSORIPPER_RIGCTLD_READ_TIMEOUT_MS` controls the per-command TCP read timeout.

### 5.4 Space Weather (NOAA SWPC)

**Data sources:**

| Data | URL | Format |
|---|---|---|
| K-index (planetary) | `https://services.swpc.noaa.gov/json/planetary_k_index_1m.json` | JSON array |
| Solar indices | `https://services.swpc.noaa.gov/text/daily-solar-indices.txt` | Fixed-width text |

**Refresh model:**
- Background refresh at `QSORIPPER_NOAA_REFRESH_INTERVAL_SECONDS` (default: 900 seconds / 15 minutes).
- Cached snapshot expires after `QSORIPPER_NOAA_STALE_AFTER_SECONDS`.
- HTTP timeout controlled by `QSORIPPER_NOAA_TIMEOUT_SECONDS`.
- If refresh fails, the engine retains the last known good snapshot and reports the error in the snapshot status.

**Parsed fields:** K-index, A-index, solar flux (SFI), sunspot number, geomagnetic storm scale.

### 5.5 Contest Calendar (WA7BNM RSS)

**Data source:** `https://www.contestcalendar.com/calendar.rss`

**Usage policy:** The built-in runtime provider is limited to the public RSS metadata feed. It must not automate access to WA7BNM HTML detail pages or copy detail-page content while serving engine requests. Exchange, band, mode, and rules details are only populated from authorized, curated, or reviewed offline-generated sources.

**Refresh model:**
- Background refresh at `QSORIPPER_CONTEST_CALENDAR_REFRESH_INTERVAL_SECONDS` (default: 3600 seconds / 1 hour).
- Cached data becomes stale after `QSORIPPER_CONTEST_CALENDAR_STALE_AFTER_SECONDS` (default: 86400 seconds / 24 hours).
- HTTP timeout controlled by `QSORIPPER_CONTEST_CALENDAR_HTTP_TIMEOUT_SECONDS`.
- If refresh fails, the engine retains the last known good calendar and reports the error in the response status.

**Parsed fields:** contest name, UTC start time, UTC end time, source URL, source name, and deterministic `contest_id`.

**Optional enrichment:** `QSORIPPER_CONTEST_CALENDAR_DETAILS_PATH` may point to a reviewed JSON file. If unset, the engine uses `data\contest-calendar\contest-details.json` when the file exists:

```json
{
  "entries": [
    {
      "name": "Example Contest",
      "sourceUrl": "https://www.contestcalendar.com/weeklycontdetails.php?ref=example",
      "bands": [ "160m", "80m", "40m", "20m", "15m", "10m" ],
      "modes": [ "cw", "ssb" ],
      "exchange": "RST + serial",
      "rulesUrl": "https://example.test/rules",
      "detailsStatus": "full"
    }
  ]
}
```

The standalone generator creates a review file from the WA7BNM 12-month calendar by default. It follows calendar detail links as an offline build step, extracts factual fields such as mode, bands, exchange, and the official rules URL, then optionally fetches official rules pages to fill gaps. It can also consume RSS metadata or an optional seed catalog of known official rules URLs:

```powershell
dotnet run --project src\dotnet\QsoRipper.Tools.ContestCatalog -- --output artifacts\contest-calendar\contest-details.generated.json
dotnet run --project src\dotnet\QsoRipper.Tools.ContestCatalog -- --promote-candidates --output data\contest-calendar\contest-details.json
```

The default output is `artifacts\contest-calendar\contest-details.generated.json`. It is not loaded by the engine. By default, extracted facts are written as `candidateBands`, `candidateModes`, and `candidateExchange` so a maintainer can verify facts and copy only factual fields into `data\contest-calendar\contest-details.json`. `--promote-candidates` writes those factual candidates into canonical catalog fields for a reviewed local database and omits the review-only candidate fields. The generator may read WA7BNM detail pages only as offline calendar metadata, but it refuses to treat WA7BNM or ContestCalendar URLs as official rules pages and should not copy rules prose into the catalog.

---

## 6. Configuration

### 6.1 Environment Variables

All configuration is driven by environment variables prefixed with `QSORIPPER_`. The engine should also support loading from a `.env` file at the config path.

#### Global

| Variable | Type | Default | Description |
|---|---|---|---|
| `QSORIPPER_SERVER_ADDR` | String | Engine profile dependent | gRPC listen address (`127.0.0.1:50051` for Rust, `127.0.0.1:50052` for .NET) |
| `QSORIPPER_CONFIG_PATH` | Path | Platform-dependent | Configuration file directory |

#### Storage

| Variable | Type | Default | Description |
|---|---|---|---|
| `QSORIPPER_STORAGE_BACKEND` | Enum | `memory` | `memory` or `sqlite` |
| `QSORIPPER_STORAGE_PATH` | Path | | SQLite file directory |
| `QSORIPPER_SQLITE_PATH` | Path | | Full SQLite file path (overrides `STORAGE_PATH`) |

#### QRZ XML Lookup

| Variable | Type | Default | Description |
|---|---|---|---|
| `QSORIPPER_QRZ_XML_USERNAME` | String | | QRZ.com username |
| `QSORIPPER_QRZ_XML_PASSWORD` | String | | QRZ.com password (secret) |
| `QSORIPPER_QRZ_USER_AGENT` | String | | HTTP User-Agent for QRZ requests |
| `QSORIPPER_QRZ_XML_BASE_URL` | URL | `https://xmldata.qrz.com/xml/current/` | QRZ XML API base URL |

#### QRZ Logbook

| Variable | Type | Default | Description |
|---|---|---|---|
| `QSORIPPER_QRZ_LOGBOOK_API_KEY` | String | | QRZ logbook API key (secret) |
| `QSORIPPER_QRZ_LOGBOOK_BASE_URL` | URL | `https://logbook.qrz.com/api` | QRZ logbook API base URL |

#### Contest Calendar

| Variable | Type | Default | Description |
|---|---|---|---|
| `QSORIPPER_CONTEST_CALENDAR_ENABLED` | Bool | `true` | Enable engine-backed contest calendar lookup |
| `QSORIPPER_CONTEST_CALENDAR_RSS_URL` | URL | `https://www.contestcalendar.com/calendar.rss` | Contest calendar RSS source URL |
| `QSORIPPER_CONTEST_CALENDAR_HTTP_TIMEOUT_SECONDS` | Integer | `8` | Contest calendar HTTP timeout |
| `QSORIPPER_CONTEST_CALENDAR_REFRESH_INTERVAL_SECONDS` | Integer | `3600` | Provider refresh interval |
| `QSORIPPER_CONTEST_CALENDAR_STALE_AFTER_SECONDS` | Integer | `86400` | Age after which cached contest data is stale |
| `QSORIPPER_CONTEST_CALENDAR_DETAILS_PATH` | Path | `data\contest-calendar\contest-details.json` | Optional reviewed local JSON catalog for bands, modes, exchange, and rules URL |

#### CW Keying

The same keys may be persisted under `[cw_keying]` in the shared `config.toml`: `backend`, `winkeyer_port`, `winkeyer_baud`, `cathub_endpoint`, `cathub_client_name`, `speed_wpm`, `transmit_enabled`, and `max_tx_ms`. Environment variables in the table below override their corresponding persisted values.

| Variable | Type | Default | Description |
|---|---|---|---|
| `QSORIPPER_CW_KEYER_BACKEND` | Enum | `null` | CW keying backend: `null`, direct `winkeyer`, or shared `cathub`. `cwdaemon` is reserved for a future backend. |
| `QSORIPPER_CW_WINKEYER_PORT` | String | | Serial port for WinKeyer, such as `COM3` on Windows or `/dev/ttyUSB0` on Linux. Required when backend is `winkeyer`. |
| `QSORIPPER_CW_WINKEYER_BAUD` | Integer | `1200` | WinKeyer serial baud rate. Most WinKeyer devices use 1200 baud. |
| `QSORIPPER_CW_CATHUB_ENDPOINT` | URL | `http://127.0.0.1:50071` | Loopback WinKeyer broker endpoint. Used only by the `cathub` backend. |
| `QSORIPPER_CW_CATHUB_CLIENT_NAME` | String | `qsoripper-engine` | Stable broker client identity used for scoped queue cancellation and telemetry. |
| `QSORIPPER_CW_SPEED_WPM` | Integer | `25` | Default CW speed in words per minute. Valid range is 5 through 99. |
| `QSORIPPER_CW_TRANSMIT_ENABLED` | Bool | `false` | Explicit safety gate for hardware text transmission. The WinKeyer backend will not send text until this is `true`. |
| `QSORIPPER_CW_MAX_TX_MS` | Integer | `120000` | Maximum duration for one hardware send before a still-busy WinKeyer buffer is cleared. Valid range is 1000 through 300000 ms. |

#### Sync

| Variable | Type | Default | Description |
|---|---|---|---|
| `QSORIPPER_SYNC_AUTO_ENABLED` | Bool | `false` | Enable automatic background sync |
| `QSORIPPER_SYNC_INTERVAL_SECONDS` | Integer | `300` | Auto-sync interval in seconds |
| `QSORIPPER_SYNC_CONFLICT_POLICY` | Enum | `last_write_wins` | `last_write_wins` or `flag_for_review` |

#### Rig Control

| Variable | Type | Default | Description |
|---|---|---|---|
| `QSORIPPER_RIGCTLD_ENABLED` | Bool | `false` | Enable rigctld integration |
| `QSORIPPER_RIGCTLD_HOST` | String | `localhost` | rigctld TCP host |
| `QSORIPPER_RIGCTLD_PORT` | Integer | `4532` | rigctld TCP port |
| `QSORIPPER_RIGCTLD_READ_TIMEOUT_MS` | Integer | `2000` | Per-command read timeout |
| `QSORIPPER_RIGCTLD_STALE_THRESHOLD_MS` | Integer | `100` | Snapshot staleness threshold (kept at the fast interactive poll cadence so live UIs surface rig changes in the next refresh) |

#### Space Weather

| Variable | Type | Default | Description |
|---|---|---|---|
| `QSORIPPER_NOAA_SPACE_WEATHER_ENABLED` | Bool | `false` | Enable NOAA space weather |
| `QSORIPPER_NOAA_REFRESH_INTERVAL_SECONDS` | Integer | `900` | Background refresh interval |
| `QSORIPPER_NOAA_STALE_AFTER_SECONDS` | Integer | `3600` | Snapshot expiry |
| `QSORIPPER_NOAA_TIMEOUT_SECONDS` | Integer | `10` | HTTP request timeout |

#### Station Profile

| Variable | Type | Default | Description |
|---|---|---|---|
| `QSORIPPER_STATION_PROFILE_NAME` | String | | Default profile name |
| `QSORIPPER_STATION_CALLSIGN` | String | | Station callsign |
| `QSORIPPER_STATION_OPERATOR_CALLSIGN` | String | | Operator callsign (if different) |

### 6.2 Graceful Degradation Rules

The engine must start and function even when external integrations are unavailable. Degradation follows these rules:

| Missing Configuration | Behavior |
|---|---|
| QRZ XML credentials | QRZ lookups disabled. Lookup entry points return `LookupState.LOOKUP_STATE_ERROR` with a sanitized configuration message. |
| QRZ logbook API key | Logbook sync disabled. `SyncWithQrz` returns `FAILED_PRECONDITION`. |
| rigctld host/port | Rig control disabled. `GetRigStatus` returns `RIG_CONNECTION_STATUS_DISABLED`. |
| NOAA weather disabled | Space weather disabled. Weather RPCs return a snapshot with `SPACE_WEATHER_STATUS_ERROR`. |
| Contest calendar disabled | Contest lookup disabled. `GetActiveContests` returns `CONTEST_CALENDAR_STATUS_DISABLED`. |
| No station profile | QSO logging requires a profile. `LogQso` returns `FAILED_PRECONDITION` until a profile is set. |

**Core invariant:** Local QSO storage and CRUD always work, regardless of external integration state. The engine must never fail to start because an external service is unavailable.

### 6.3 Configuration Persistence

- Configuration is persisted as a shared TOML file in `QSORIPPER_CONFIG_PATH`.
- The `SaveSetup` RPC writes only non-secret configuration to this path.
- On startup, the engine loads persisted configuration and overlays environment variable overrides (env vars take precedence).
- Runtime config mutations (via `DeveloperControlService`) are ephemeral and do not persist across restarts unless explicitly saved.
- The persisted/default conflict policy is `LAST_WRITE_WINS`. When a present setup request explicitly supplies the proto zero value, engines normalize it to the safe `FLAG_FOR_REVIEW` policy before persistence.

#### Secret handling

- QRZ XML passwords, QRZ Logbook API keys, provider session keys, and future integration credentials MUST NOT be serialized to the shared TOML file.
- Secrets supplied by `SaveSetup` or `DeveloperControlService` are process-session values. Restart-persistent secrets come from environment variables or a platform secure configuration provider.
- Status and runtime configuration responses expose only configured/unconfigured state and redacted display text. Logs and error details never contain secret values or sensitive request payloads.
- On loading a legacy config that contains plaintext QRZ secrets, an engine MAY use those values for the current process so logging remains available, but it MUST immediately rewrite the owned config tables without the plaintext fields. That migration is idempotent.

---

## 7. Behavioral Requirements

### 7.1 Station Context

Every logged QSO must carry station identity data. The station context system works as follows:

1. **Station profiles** are named, persisted sets of station defaults: callsign, operator callsign, grid square, county, state, country, DXCC, CQ/ITU zones, latitude/longitude, and ARRL section.

2. **Active profile** — exactly one saved profile is active at any time. A session profile set through `SetSessionStationProfileOverride` fully replaces that saved profile for runtime station context until cleared. It is not a field overlay because the current protobuf request carries a complete `StationProfile` without patch presence semantics.

3. **Station snapshot** — when a QSO is logged, the engine captures the current station context as an immutable `StationSnapshot` and attaches it to the `QsoRecord`. This snapshot is never retroactively updated if the profile changes.

4. **Materialization** — the engine must implement a `station_snapshot_from_profile` function that converts the effective `StationProfile` into a `StationSnapshot` suitable for embedding in a QSO.

### 7.2 QSO Lifecycle

#### Creating a QSO

1. Client calls `LogQso` with the worked callsign, band, mode, signal reports, and optional fields.
2. Engine generates `local_id` as UUID v4.
3. Engine normalizes `worked_callsign`: `trim().to_uppercase()`.
4. Engine validates required fields: `worked_callsign` must be non-empty, `band` must be non-default, `mode` must be non-default, `utc_timestamp` must be present.
5. Engine stamps `station_callsign` and `station_snapshot` from the active station context.
6. Engine sets `created_at` = `updated_at` = now (UTC), `sync_status` = `LOCAL_ONLY`, clears QRZ linkage, and clears delete state regardless of caller-supplied values.
7. Engine persists the `QsoRecord` via the storage backend.
8. Engine returns its generated ID and per-operation sync result. The client uses `GetQso` to retrieve the persisted record.

#### Updating a QSO

1. Client calls `UpdateQso` with a complete replacement record containing `local_id`.
2. Engine loads the existing record, replaces all caller-owned fields, preserves engine-owned fields, and sets `updated_at` = now.
3. If previously synced, engine sets `sync_status` = `MODIFIED`.
4. Engine persists and returns success. The client uses `GetQso` to retrieve the updated record.

#### Deleting a QSO

1. Client calls `DeleteQso` with `local_id` (and optional `delete_from_qrz` flag).
2. Engine performs a **soft delete** — the row stays in storage with `deleted_at` set to now (UTC). See §7.8 for full semantics.
3. If `delete_from_qrz = true` and the row has a non-empty `qrz_logid`, the engine sets `pending_remote_delete = true` so a future sync can issue the QRZ DELETE; this RPC does **not** call QRZ inline.
4. Response includes `success`, `remote_delete_queued`. The legacy `qrz_delete_success`/`qrz_delete_error` fields stay false/empty (deprecated; do not consume).
5. Re-deleting an already soft-deleted row succeeds idempotently and may upgrade `pending_remote_delete` from false to true.

### 7.3 Sync Lifecycle

The QRZ logbook sync is a multi-phase operation:

#### Phase 0: Status and owner resolution

Call QRZ `STATUS` once before download. Use its owner callsign for upload rewriting and its count for the eventual metadata update. If `STATUS` fails transiently, use cached owner metadata and continue where safe.

#### Phase 0.5: Push local corrections before download

Before fetching remote QSOs, engines MUST resolve the QRZ logbook owner callsign (via `STATUS`, falling back to cached metadata on transient failure) and push local rows with `sync_status = MODIFIED` and a non-empty `qrz_logid` when the effective conflict policy is `CONFLICT_POLICY_FLAG_FOR_REVIEW` or `CONFLICT_POLICY_UNSPECIFIED`. This uses the documented replace form `ACTION=INSERT&OPTION=REPLACE`. QRZ identifies the existing record from the ADIF identity fields.

This pre-download upload prevents a stale QRZ copy from being downloaded first and converting a normal local correction into `CONFLICT` before the upload phase can update QRZ. Engines MUST remember the `qrz_logid` values successfully uploaded during this phase and ignore matching remote rows returned by the same sync's subsequent `FETCH`, because QRZ may briefly return the stale pre-replace copy.

#### Phase 1: Download

1. Call QRZ logbook API `FETCH` with `OPTION=ALL` (full sync) or `OPTION=ALL,MODSINCE:YYYY-MM-DD` (incremental).
2. Parse the ADIF response into QSO records. Engines MUST recognise QRZ-specific ADIF application fields and map them onto dedicated domain fields (see §7.5 Import).
3. For each remote QSO:
    a. Prefer a direct match on `qrz_logid` if one was returned for that record.
    b. Otherwise, fuzzy-match against local records: callsign (case-insensitive) + UTC timestamp (within a tolerance window, typically ±60s) + band + mode.
    c. If the remote `qrz_logid` was successfully uploaded during Phase 0.5 of this same sync, skip it rather than overwriting the corrected local row with a stale remote response.
    d. If matched, apply the configured `ConflictPolicy`:
       - `CONFLICT_POLICY_LAST_WRITE_WINS` — treat the remote record as authoritative and overwrite local fields; mark the merged row as `SYNCED`.
       - `CONFLICT_POLICY_FLAG_FOR_REVIEW` — when the local row was locally edited (`sync_status = MODIFIED`), preserve the local fields, set `sync_status = CONFLICT`, and increment the sync result's conflict counter so operators can reconcile manually. When the local row is already `SYNCED`, remote wins (no conflict).
       - `CONFLICT_POLICY_UNSPECIFIED` — engines MUST treat the zero value as `FLAG_FOR_REVIEW` (the safe, non-destructive default) per §6.3.
       - When the matched local row is `SYNC_STATUS_LOCAL_ONLY`, engines MUST link the QRZ identity and mark it `SYNCED` without overwriting locally logged contest/contact fields. Engines MUST fill missing worked-station enrichment fields from the remote QRZ ADIF record (for example `GRIDSQUARE`, `COUNTRY`, `DXCC`, `STATE`, `CNTY`, `CQZ`, `ITUZ`, `CONT`, worked-station lat/lon/altitude, and other remote-only ADIF extras) so QSOs uploaded by external loggers can adopt QRZ Logbook enrichment on the next sync.
    e. If unmatched, insert as a new local record with `sync_status = SYNCED` and populate `qrz_logid` from the remote record.
4. Filter ghost records: remote QSOs missing required fields (callsign, timestamp) are skipped without incrementing any counter.
5. **Soft-delete suppression:** before matching, engines MUST load the full local record set including soft-deleted rows (see §7.8) and build the set of `qrz_logid` values associated with locally soft-deleted QSOs. Any remote QSO whose `qrz_logid` is in that set MUST be skipped (no insert, no merge), and the engine MUST increment the `deletes_skipped_remote` counter on the sync result. This prevents resurrection of QSOs the operator has trashed locally before the queued remote-delete (Phase 2.5) has propagated.

#### Phase 2: Upload

1. Query local QSOs with `sync_status` in (`LOCAL_ONLY`, `MODIFIED`). Soft-deleted rows MUST NOT be uploaded as inserts/updates regardless of `sync_status`; they are handled by Phase 2.5.
2. For each QSO, serialize to ADIF and call the QRZ logbook API:
   - If `sync_status = LOCAL_ONLY` (new record, no `qrz_logid`), use `ACTION=INSERT`.
   - If `sync_status = MODIFIED` and the record has a `qrz_logid`, use the documented replace form `ACTION=INSERT&OPTION=REPLACE`. QRZ identifies the existing record from the ADIF identity fields. Engines MUST NOT append a `LOGID` selector to `OPTION` or use the undocumented `ACTION=REPLACE` form.
   - If `sync_status = MODIFIED` but no `qrz_logid` is available (e.g., first sync after upgrading from an engine that didn't persist the logid), fall back to `ACTION=INSERT`. Engines SHOULD additionally run the repair pass described in §7.7 before the first sync so modified-without-logid rows are rare.
3. Accept both `RESULT=OK` (insert) and `RESULT=REPLACE` (update) as success indicators when parsing the QRZ response.
4. On success, set `sync_status = SYNCED` and store the returned `LOGID` (or the supplied one for a REPLACE that echoes nothing) in `qrz_logid`.
5. On per-QSO failure, log the error and continue with remaining QSOs.

#### Phase 2.5: Push pending remote deletes

1. Query local QSOs where `deleted_at IS NOT NULL AND pending_remote_delete = true AND qrz_logid` is non-empty.
2. For each such row, call the QRZ logbook API `ACTION=DELETE&KEY=<api_key>&LOGID=<qrz_logid>`.
3. Treat the following responses as success (the remote row is gone or never existed — the operator's intent is satisfied):
   - `RESULT=OK`
   - `RESULT=FAIL&REASON=<text>` where `<text>` matches a not-found indicator (case-insensitive substring match on `not found`, `no such`, `does not exist`, or `no record`)
   - HTTP 404 from the QRZ endpoint
4. On success, the engine MUST clear `pending_remote_delete` and clear `qrz_logid` (so a future re-sync cannot re-target a now-detached logid) while leaving `deleted_at` set so the row remains in the trash view. Increment the `remote_deletes_pushed` counter.
5. On other failures (network, authentication, unrecognized REASON), the engine MUST leave `pending_remote_delete = true` and `qrz_logid` intact so the next sync retries. Append a description of the failure to the sync error summary.
6. Authentication errors MUST propagate from the QRZ adapter as auth-failure exceptions (not collapsed into "not found"); engines surface them in the same way as other Phase 2 auth errors.

#### Phase 3: Metadata

1. Reuse the Phase 0 QRZ `STATUS` result. Do not make a second status call.
2. Update `sync_metadata` with the count, timestamp, and owner. If Phase 0 `STATUS` failed, engines SHOULD fall back to cached owner metadata and locally computed counts rather than inventing a remote count.

**Resilience:** A failure in any phase should not prevent other phases from executing. The engine should report partial success/failure in the stream. A fatal failure in Phase 1 (e.g., metadata load failure, fetch failure) MUST short-circuit the rest of `execute_sync` so that `last_sync` is NOT advanced — the next attempt re-fetches the same window.

#### Per-Operation Sync (`sync_to_qrz=true` on LogQso/UpdateQso)

When `LogQso.sync_to_qrz=true` or `UpdateQso.sync_to_qrz=true`, engines MUST attempt to push the affected row to QRZ immediately after the local persist. This is independent of the bulk sync flow (`SyncWithQrz`) and lets clients log a QSO and get an authoritative QRZ logid back in a single round-trip.

**Selection rule (mirrors Phase 2):**
- If the local row has a non-empty `qrz_logid`, replace it using `ACTION=INSERT&OPTION=REPLACE`; QRZ identifies the existing record from the ADIF identity fields.
- Otherwise INSERT (`ACTION=INSERT`).

**Success path:** adopt the QRZ-assigned `LOGID`, set `sync_status=SYNC_STATUS_SYNCED`, write the row back to local storage, and populate the response's `sync_success=true` (and `qrz_logid` for `LogQsoResponse`).

**Failure path:** the local persist MUST happen before the remote call and remains successful regardless of remote outcome. The QRZ failure is reported, not raised: leave the row's `sync_status` untouched (`LOCAL_ONLY` or `MODIFIED`), set `sync_success=false`, and put a human-readable message in `sync_error`. The next bulk `SyncWithQrz` will retry the row via Phase 2.

**Configuration not present:** if no QRZ logbook API key is configured, return `sync_success=false` with `sync_error="QRZ Logbook API key is not configured."`. The local row still persists.

**Not requested:** when `sync_to_qrz=false`, return `sync_success=false` and leave `sync_error` absent. The boolean describes a successful remote operation, not local persistence.

### 7.4 Lookup Lifecycle

#### Single Lookup Flow

```
Client calls Lookup("W1AW")
  → Engine checks lookup_snapshots cache
    → Cache HIT (not expired) → return cached result (cache_hit=true)
    → Cache MISS or expired:
      → Check in-flight dedup map
        → Already in flight → wait for existing result
        → Not in flight → register in-flight
          → Call QRZ XML API
            → Parse XML response
            → Normalize to CallsignRecord
            → Enrich with DXCC entity data
            → Cache in lookup_snapshots
            → Remove from in-flight map
          → Return result (cache_hit=false)
```

#### Slash-Call Fallback

For callsigns with modifiers (e.g., `W1AW/7`, `VE3/W1AW`):

1. Attempt lookup with the full callsign.
2. If not found, extract the base callsign (strip the modifier).
3. Retry lookup with the base callsign.
4. On the result, populate:
   - `base_callsign` — the callsign used for the successful lookup
   - `modifier_text` — the modifier portion (e.g., `/7`)
   - `modifier_kind` — the type of modifier (`ModifierKind` enum)
   - `callsign_ambiguity` — flags if the callsign interpretation is ambiguous

#### Zone Cascade

When DXCC data is available, cascade zone information onto the lookup result if the source record lacks it:
- CQ zone from DXCC entity if not on the callsign record
- ITU zone from DXCC entity if not on the callsign record

### 7.5 ADIF Import/Export

**ADIF is the Amateur Data Interchange Format**, used exclusively for external file interchange and QRZ API communication. Internal engine IPC always uses protobuf.

#### Import

1. Parse the ADI-format input (header + records delimited by `<eor>`).
2. Map ADIF field names to `QsoRecord` proto fields.
3. Map QRZ-specific application fields to dedicated domain fields — not generic `extra_fields` — so sync can round-trip them:
   - `APP_QRZLOG_LOGID` (canonical) and the legacy alias `APP_QRZ_LOGID` → `qrz_logid`
   - `APP_QRZLOG_QSO_ID` (canonical) and the legacy alias `APP_QRZ_BOOKID` → `qrz_bookid`
   Engines MUST NOT leave these app keys in `extra_fields` once a dedicated domain field carries the value, otherwise downstream sync will treat the record as unlinked and re-upload it as a duplicate.
4. Map normalized ADIF fields to their dedicated proto slots rather than `extra_fields`:
   - `BAND_RX` → `band_rx` (Band enum)
   - `FREQ_RX` → `frequency_rx_hz` (MHz → Hz via string math for sub-kHz precision)
   - `LAT` / `LON` → `worked_latitude` / `worked_longitude` (parsed from `[NSEW]DDD MM.MMM` to signed decimal degrees)
   - `ALTITUDE` → `worked_altitude_meters`
   - `GRIDSQUARE_EXT` → `worked_gridsquare_ext`
   - `OWNER_CALLSIGN` → `owner_callsign`
   - `QSO_COMPLETE` → `qso_complete` (`Y`/`N`/`NIL`/`?` → `QsoCompletion` enum)
   - `MY_ALTITUDE` → `station_snapshot.altitude_meters`
   - `MY_GRIDSQUARE_EXT` → `station_snapshot.gridsquare_ext`
   - `APP_QSORIPPER_RX_WPM` → `cw_decode_rx_wpm` (parsed as unsigned integer; non-numeric values fall back to `extra_fields`)
   - `APP_QSORIPPER_CW_TRANSCRIPT` → `cw_decode_transcript` (decoded CW transcript snapshot for the QSO; empty values are dropped)
   - `ARRL_SECT` and `SKCC` → worked-station `arrl_section` and `skcc_number`
   - `QSL_SENT`, `QSL_RCVD`, `QSLSDATE`, and `QSLRDATE` → typed QSL status/date fields
   - `LOTW_QSL_SENT`, `LOTW_QSL_RCVD`, `LOTW_QSLSDATE`, and `LOTW_QSLRDATE` → typed LoTW status/date fields
   - `EQSL_QSL_SENT`, `EQSL_QSL_RCVD`, `EQSL_QSLSDATE`, and `EQSL_QSLRDATE` → typed eQSL status/date fields
   - `MY_LAT`, `MY_LON`, `MY_ARRL_SECT`, `MY_CQ_ZONE`, and `MY_ITU_ZONE` → dedicated `station_snapshot` fields
   Unrecognized values (e.g., malformed LAT, unknown `QSO_COMPLETE` literal) fall back to `extra_fields` under the original key.
5. Preserve any other unrecognized ADIF fields in the `extra_fields` map for lossless round-trip.
6. Generate a `local_id` for each imported record.
7. Normalize callsigns and validate required fields.
8. Insert into storage with `sync_status = LOCAL_ONLY`.

See `docs/integrations/adif-specification.md` for the authoritative field-name table.

#### Export

1. Generate an ADIF header with program name and version.
2. For each QSO, serialize proto fields back to ADIF field names.
3. Emit QRZ app fields whenever the corresponding domain field is populated:
   - `qrz_logid` → `APP_QRZLOG_LOGID`
   - `qrz_bookid` → `APP_QRZLOG_QSO_ID`
   When iterating `extra_fields`, skip keys already covered by these dedicated emissions (`APP_QRZLOG_LOGID`, `APP_QRZ_LOGID`, `APP_QRZLOG_QSO_ID`, `APP_QRZ_BOOKID`) to avoid duplicate ADIF fields.
4. Emit normalized ADIF fields from their dedicated proto slots whenever populated, including the worked/station, QSL, LoTW, eQSL, SKCC, ARRL section, zone, and coordinate mappings listed above. When iterating `extra_fields`, skip every key handled by a dedicated field so the dedicated proto value always wins and the ADIF output never contains the same field twice. Engines MUST sanitize `cw_decode_transcript` to printable ASCII (plus CR/LF/tab) before emitting `APP_QSORIPPER_CW_TRANSCRIPT` so the .NET char-count length and Rust byte-count length agree across runtimes.
5. Include other `extra_fields` to preserve data from previous imports.
6. Output records delimited by `<eor>`.

### 7.6 Error Handling

#### General Principles

- Use standard gRPC status codes (see individual RPC documentation).
- Include descriptive error messages in the gRPC status detail.
- Never leak credentials, API keys, or session tokens in error messages.
- Log actionable errors server-side with enough context to diagnose issues.
- External integration failures must never crash the engine or prevent local operations.

#### Standard gRPC Status Code Usage

| Code | Usage |
|---|---|
| `OK` | Success |
| `INVALID_ARGUMENT` | Malformed request, missing required fields, invalid values |
| `NOT_FOUND` | Requested entity does not exist |
| `FAILED_PRECONDITION` | Operation cannot proceed due to system state (e.g., no credentials, no active profile) |
| `UNAVAILABLE` | External service unreachable |
| `UNIMPLEMENTED` | RPC is defined but not yet implemented |
| `INTERNAL` | Unexpected server error (storage failure, serialization bug) |

### 7.7 Startup Data-Repair Pass

Engines that persist `extra_fields` as an opaque blob MUST run a best-effort data-repair pass on startup against the active logbook store. The pass:

1. **Backfills dedicated domain fields from legacy `extra_fields`.** Scans every QSO and, for each record where `qrz_logid`/`qrz_bookid` are empty but a legacy key exists in `extra_fields` (e.g. `APP_QRZ_LOGID`, `APP_QRZLOG_LOGID`, `APP_QRZ_BOOKID`, `APP_QRZLOG_QSO_ID`), moves the value into the dedicated field and removes the legacy key from `extra_fields`.
2. **Collapses duplicate rows that share a `qrz_logid`.** After the backfill, any group of QSOs with the same non-empty `qrz_logid` represents a historical duplicate-import bug. The engine keeps the oldest row as the winner, merges non-empty string fields from the losing rows into the winner, and deletes the losers. The winner keeps `sync_status = SYNCED`.
3. **Logs a summary** of how many rows were backfilled, how many duplicates were collapsed, and any per-row errors, but does not fail engine startup on per-row errors.

This pass exists because an earlier engine revision failed to map QRZ app fields onto dedicated domain columns (see §7.5 Import), causing subsequent syncs to re-upload every QSO as a new record and multiplying the logbook. The repair pass is idempotent; engines that have already cleaned their data will do no work on subsequent startups.

### 7.8 Soft-Delete and Restore

QSOs are soft-deleted: `DeleteQso` marks the row with a tombstone instead of removing it. This preserves user data for an undo flow and lets a future sync push a corresponding remote delete to QRZ.

#### Schema

Every `QsoRecord` carries two soft-delete fields:

- `deleted_at` (optional `Timestamp`): when set, the row is considered deleted. Null on active rows.
- `pending_remote_delete` (bool): set to true when the local delete should be propagated to QRZ on the next sync. Cleared once the remote delete completes (or if the row is restored).

Storage backends MUST persist both fields and MUST default `deleted_at` to null and `pending_remote_delete` to false for records written before this contract existed (idempotent migration on startup).

#### DeleteQso semantics

1. Resolve the row by `local_id`.
2. Set `deleted_at = now (UTC)`.
3. If `delete_from_qrz = true` AND the row has a non-empty `qrz_logid`, set `pending_remote_delete = true`. Otherwise leave it false.
4. Persist via `SoftDeleteQso` storage path (no row removal).
5. Return `success = true`, `remote_delete_queued = pending_remote_delete`.
6. If `delete_from_qrz = true` but `qrz_logid` is empty, the response surfaces an explanatory `qrz_delete_error` — the row is still soft-deleted locally, just not queued for remote delete.
7. Re-deleting an already soft-deleted row is an idempotent success. If the second call sets `delete_from_qrz = true` and the row has a logid, it MAY upgrade `pending_remote_delete` from false to true.
8. The engine MUST NOT call the QRZ `ACTION=DELETE` API inline from this RPC. Remote delete is performed by the sync engine (§7.3 Phase 2).

#### RestoreQso semantics

1. Resolve the row by `local_id`. If not found, return `NOT_FOUND`.
2. Clear both `deleted_at` and `pending_remote_delete` via the `RestoreQso` storage path.
3. If the restored row has no `qrz_logid` and its `sync_status` was `SYNCED` (i.e., the QRZ-side state never actually existed for this local row), demote `sync_status` to `LOCAL_ONLY` so the next sync re-uploads it. Update `updated_at = now`.
4. Return `success = true` and the restored `QsoRecord`.
5. Restoring a row that is not soft-deleted is an idempotent success (no-op).
6. Engines MAY refuse `RestoreQso` with `FAILED_PRECONDITION` while a sync is in flight.

#### UpdateQso on a soft-deleted row

`UpdateQso` MUST reject any attempt to modify a soft-deleted row with `FAILED_PRECONDITION`. The client must call `RestoreQso` first.

#### Listing semantics

- `ListQsos` defaults to `DeletedRecordsFilter::ACTIVE_ONLY` when the filter is `UNSPECIFIED`. Engines MUST exclude soft-deleted rows from the default list.
- `DELETED_ONLY` returns only soft-deleted rows (the trash view).
- `ALL` returns both. Trash UIs should request `DELETED_ONLY`; standard logbook views should rely on the default.
- `GetQso` MUST return a soft-deleted row by id (so a trash UI can fetch a single deleted row by its `local_id`).

#### Import / Export interaction

- `ImportAdif` duplicate matching uses the default (active-only) listing so that soft-deleted rows do not block re-import of a corrected QSO. (A deleted dup is treated as absent.)
- `ExportAdif` uses the default (active-only) listing so soft-deleted rows are not exported.

#### Sync interaction

- Sync Phase 1 (download) MUST skip a remote row whose `qrz_logid` matches a soft-deleted local row. The local user's delete intent wins. The sync summary SHOULD report skipped count.
- Sync Phase 2 (upload) MUST, after the normal upload pass, iterate rows where `pending_remote_delete = true`, call QRZ `ACTION=DELETE&KEY=…&LOGID=…`, and on success or HTTP 404 ("logid not found") clear both `qrz_logid` and `pending_remote_delete` while leaving `deleted_at` set. On other failures, leave the flags and surface the error in the sync summary.
- Permanent purge of soft-deleted rows is handled by `PurgeDeletedQsos` (§7.9), which reuses the Phase 2 remote delete flow when `include_pending_remote_deletes = true`.

---

### 7.9 Purge Deleted QSOs

`PurgeDeletedQsos` permanently removes soft-deleted rows from local storage ("empty trash"). This is a destructive, non-recoverable operation that is intentionally distinct from the recoverable `DeleteQso`.

#### Contract

- Request: `PurgeDeletedQsosRequest` with `local_ids`, `older_than`, `include_pending_remote_deletes`, and `confirm`.
- Response: `PurgeDeletedQsosResponse` with `purged_count`, `remote_deletes_pushed`, `remote_deletes_failed`, and `error_summary`.

#### Preconditions

1. `confirm` MUST be `true`. If `false`, the engine MUST return `INVALID_ARGUMENT`.
2. The engine MUST return `FAILED_PRECONDITION` if a sync is currently in progress. Purging a row mid-sync is racy and could lead to inconsistent state.

#### Eligibility

Only rows with `deleted_at IS NOT NULL` are eligible for purge. Rows that are not soft-deleted MUST be silently ignored (never purged).

- If `local_ids` is non-empty, only those IDs are eligible (still must be soft-deleted).
- If `older_than` is set, only rows with `deleted_at <= older_than` are eligible.
- If both filters are set, both must match (AND semantics).
- If both are empty/unset, all soft-deleted rows are purged.

#### Remote delete behavior

When `include_pending_remote_deletes = true`:

1. Before the local hard-delete, the engine MUST iterate eligible rows that have `pending_remote_delete = true` and a non-empty `qrz_logid`.
2. For each such row, the engine SHOULD issue a QRZ `ACTION=DELETE` call using the same flow as sync Phase 2 (§7.3).
3. On success or HTTP 404 ("logid not found"): count toward `remote_deletes_pushed`. The row is then eligible for local purge.
4. On other failures: count toward `remote_deletes_failed`. The row MUST NOT be purged locally; the operator can retry.
5. If QRZ is not configured: rows needing remote deletes are counted as failed and are not purged locally.

When `include_pending_remote_deletes = false`:

- The engine skips the remote delete entirely. Rows with `pending_remote_delete = true` are purged locally regardless. The remote QSO on QRZ survives — this is the operator's explicit choice.

#### Storage operation

The engine delegates to a `purge_deleted_qsos` storage path that performs `DELETE FROM qsos WHERE deleted_at IS NOT NULL` (with the applicable ID and timestamp filters). This is a physical row removal, not a soft-delete.

#### Idempotency

Purging an already-purged row (or a non-existent ID) is a no-op: `purged_count` simply does not include it. There is no error.

#### Sync metadata

The engine MUST NOT attempt to adjust `qrz_qso_count` or other sync metadata inline during a purge. The next `SyncWithQrz` will recompute the remote count via QRZ `STATUS` naturally. This avoids drift from partial remote-delete results.

#### Cross-references

- Soft-delete semantics: §7.8
- Sync Phase 2 remote delete flow: §7.3
- Storage trait: `purge_deleted_qsos` on `LogbookStore` / `ILogbookStore`

---

## 8. Capability Reporting

### 8.1 GetEngineInfo Contract

Every engine must implement `GetEngineInfo` to report its identity and capabilities. This is the first RPC a client calls after connecting.

**Required response fields:**

| Proto field | Example (Rust) | Example (.NET) |
|---|---|---|
| `engine_id` | `rust-tonic` | `dotnet-aspnet` |
| `display_name` | `QsoRipper Rust Engine` | `QsoRipper .NET Engine` |
| `version` | SemVer package version, for example `0.1.0` | Assembly informational/version string, which may contain four numeric components |
| `capabilities` | List of supported capability strings | List of supported capability strings |

> **Note:** Earlier drafts of this spec referenced `engine_language` and `storage_backend` fields. These were never added to the `EngineInfo` proto message. Use `engine_id` to infer the implementation language if needed.

**Capability strings** indicate which optional features the engine supports. Clients use these to enable or disable UI features.

Both engines currently report the following capabilities:

| Capability | Description |
|---|---|
| `engine-info` | Engine metadata / health check |
| `logbook` | Core QSO CRUD |
| `lookup-cache` | Cached callsign lookup |
| `lookup-callsign` | Live callsign lookup |
| `lookup-stream` | Streaming callsign lookup |
| `setup` | First-run setup wizard |
| `station-profiles` | Station profile management |
| `runtime-config` | Runtime configuration updates |
| `rig-control` | rigctld integration |
| `space-weather` | NOAA space weather data |
| `contest-calendar` | Contest calendar lookup |
| `cw-keying` | CW macro expansion and keyer dispatch |
| `purge` | Permanent removal of soft-deleted QSOs (§7.9) |

> **Note:** Earlier drafts listed aspirational names (`sync`, `rig_control`, `stress`, `adif_import`, `adif_export`) that were never adopted. The canonical names above use kebab-case and match what both engines actually report. New capabilities should follow this convention.

Engines currently report a static set of capabilities. Configuration-gated capability reporting (e.g., hiding `lookup-callsign` when no QRZ credentials are configured) is a planned enhancement.

---

## 9. Conformance Testing

### 9.1 Conformance Harness

The black-box conformance harness lives at `tests/Run-EngineConformance.ps1`. It is a PowerShell script that:

1. Starts each reference engine with both memory and SQLite storage.
2. Runs the QsoRipper CLI against each engine to exercise client-visible setup, status, CW, CRUD, restore, purge, filtering, ADIF export, and ADIF import workflows.
3. Compares normalized results across engine implementations and storage backends for field-level parity.
4. Writes a structured JSON summary to `artifacts/conformance/<run-id>/`.

The protobuf files and this specification define conformance. The black-box harness is a required executable acceptance layer, but it is not the only test layer. Network-dependent QRZ behavior, precise gRPC status codes, streaming cancellation/timing, scheduler state, startup repair, runtime configuration, rig, weather, contest, station-profile, and Great Circle contracts are also covered by the reference engines' service-level integration and unit suites. A new engine must provide equivalent automated coverage for required services it cannot exercise through the CLI.

### 9.2 Required Test Scenarios

A conformant engine must pass all of the following scenarios:

#### Setup and Status

1. `setup --from-env` succeeds and reports `setupComplete = true`.
2. `status` reports the correct engine identity and storage backend.
3. Station callsign is correctly persisted and reported.

#### QSO CRUD

4. `LogQso` creates a QSO with a generated `local_id` and correct station stamping.
5. `GetQso` returns the logged QSO with all fields intact.
6. `ListQsos` returns exactly the expected QSOs with correct ordering.
7. `UpdateQso` modifies the specified fields and updates `updated_at`.
8. `DeleteQso` soft-deletes the QSO; default `ListQsos` hides it, and `GetQso` returns it with `deleted_at` set.
9. `RestoreQso` clears the tombstone and returns the row to the default list.
10. `PurgeDeletedQsos(confirm=true)` physically removes a re-deleted row, after which `GetQso` returns `NOT_FOUND`.
11. Inclusive time boundaries and case-insensitive station/worked callsign filters produce identical results on memory and SQLite.
12. Unary success and failure responses with optional scalar fields serialize cleanly at the service boundary without handler exceptions.

#### ADIF Round-Trip

13. `ExportAdif` produces valid ADIF output containing all logged QSOs.
14. `ImportAdif` with previously exported ADIF creates equivalent records.
15. Typed QRZ/QSL fields and `extra_fields` survive a full import → export → import round-trip.

#### Cross-Engine Parity

16. Given the same sequence of operations, the Rust and .NET engines produce field-identical `GetQso`, `ListQsos`, `ExportAdif`, and re-import results.
17. Each engine's memory and SQLite backends produce the same normalized results.
18. Both engines report `localQsoCount == 1` after logging one QSO.

#### Lookup (if credentials available)

19. `Lookup` for a known callsign returns a populated `CallsignRecord`.
20. `GetCachedCallsign` returns the cached result after a successful lookup.
21. `Lookup` for an unknown callsign returns `LOOKUP_STATE_NOT_FOUND`.

#### Degradation

22. Engine starts successfully with no QRZ credentials configured.
23. Engine starts successfully with no rigctld configured.
24. `LogQso` works when external integrations are unavailable.

---

## 10. Reference Implementations

### 10.1 Rust Engine (qsoripper-server)

| Property | Value |
|---|---|
| **Location** | `src/rust/qsoripper-server/` |
| **Core library** | `src/rust/qsoripper-core/` |
| **Language** | Rust |
| **gRPC framework** | tonic + prost |
| **Storage backends** | `qsoripper-storage-memory`, `qsoripper-storage-sqlite` |
| **Build** | `cargo build --manifest-path src/rust/Cargo.toml -p qsoripper-server` |
| **Run** | `cargo run --manifest-path src/rust/Cargo.toml -p qsoripper-server` |
| **Test** | `cargo test --manifest-path src/rust/Cargo.toml` |

**Architecture notes:**
- `qsoripper-core` owns reusable engine logic: domain mapping, proto bindings, storage traits, QRZ adapters, rig control, space weather, and ADIF parsing.
- `qsoripper-server` owns the tonic server bootstrap, runtime configuration registry, and gRPC service implementations.
- Storage backends are separate crates (`qsoripper-storage-memory`, `qsoripper-storage-sqlite`) that implement the `EngineStorage` trait from `qsoripper-core`.
- Proto generation happens in `qsoripper-core/build.rs`.

### 10.2 .NET Engine (QsoRipper.Engine.DotNet)

| Property | Value |
|---|---|
| **Location** | `src/dotnet/QsoRipper.Engine.DotNet/` |
| **Language** | C# |
| **gRPC framework** | Grpc.Tools + ASP.NET Core |
| **Storage backend** | In-memory (managed state) or SQLite (`QsoRipper.Engine.Storage.Sqlite`) |
| **Build** | `dotnet build src/dotnet/QsoRipper.Engine.DotNet/QsoRipper.Engine.DotNet.csproj` |
| **Run** | `dotnet run --project src/dotnet/QsoRipper.Engine.DotNet/QsoRipper.Engine.DotNet.csproj` |
| **Test** | `dotnet test src/dotnet/QsoRipper.Engine.DotNet.Tests/` |

**Architecture notes:**
- `GrpcServices.cs` maps gRPC service interfaces to the managed engine state.
- `ManagedEngineState.cs` implements core engine logic: QSO CRUD, station context, lookup orchestration.
- `ManagedAdifCodec.cs` handles ADIF serialization/deserialization.
- `ManagedQsoParity.cs` ensures QSO normalization and station stamping matches the Rust engine.
- Proto generation uses `Grpc.Tools` configured in the `.csproj`.

---

## Appendix A: Key Domain Types Quick Reference

| Proto File | Type | Description |
|---|---|---|
| `proto/domain/qso_record.proto` | `QsoRecord` | The core logged-contact entity |
| `proto/domain/callsign_record.proto` | `CallsignRecord` | Normalized callsign lookup result |
| `proto/domain/dxcc_entity.proto` | `DxccEntity` | DXCC entity reference data |
| `proto/domain/lookup_result.proto` | `LookupResult` | Lookup outcome with metadata |
| `proto/domain/qso_history_entry.proto` | `QsoHistoryEntry` | Compact prior-QSO summary returned with lookup results |
| `proto/domain/station_profile.proto` | `StationProfile` | Durable station defaults |
| `proto/domain/station_snapshot.proto` | `StationSnapshot` | Immutable per-QSO station capture |
| `proto/domain/rig_snapshot.proto` | `RigSnapshot` | Normalized rig frequency, mode, split, and transmitter-power snapshot |
| `proto/domain/space_weather_snapshot.proto` | `SpaceWeatherSnapshot` | Space weather indices |
| `proto/domain/contest_calendar_entry.proto` | `ContestCalendarEntry` | Normalized contest calendar entry |
| `proto/domain/sync_config.proto` | `SyncConfig` | Sync policy configuration |
| `proto/domain/band.proto` | `Band` | Band enumeration (ADIF-aligned) |
| `proto/domain/mode.proto` | `Mode` | Mode enumeration (ADIF-aligned) |
| `proto/domain/sync_status.proto` | `SyncStatus` | QSO sync state |
| `proto/domain/lookup_state.proto` | `LookupState` | Lookup result state |
| `proto/domain/conflict_policy.proto` | `ConflictPolicy` | Sync conflict resolution policy |
| `proto/domain/qso_completion.proto` | `QsoCompletion` | ADIF `QSO_COMPLETE` enum (Y/N/NIL/?) |
| `proto/domain/rig_connection_status.proto` | `RigConnectionStatus` | Rig connection state |
| `proto/domain/space_weather_status.proto` | `SpaceWeatherStatus` | Space weather data state |
| `proto/domain/contest_calendar_status.proto` | `ContestCalendarStatus` | Contest calendar cache/provider state |
| `proto/domain/contest_details_status.proto` | `ContestDetailsStatus` | Contest entry detail completeness |

## Appendix B: Proto File Conventions

- **1-1-1 rule:** One top-level message, enum, or service per `.proto` file.
- **Per-RPC envelopes:** Every RPC gets unique `XxxRequest` and `XxxResponse` messages.
- **Service declarations** contain only the `service` block; all message types live in separate files.
- **Domain types** live in `proto/domain/`; transport/service support types live in `proto/services/`.
- **Reusable payloads** are extracted into dedicated messages and wrapped from each response — never reuse one RPC's response as another's.
- Run `buf lint` to validate proto files. Run `buf breaking` to guard against incompatible schema changes.

See `docs/architecture/data-model.md` for the complete proto conventions and field-addition guide.

## Appendix C: Contract Evolution

Known reference-engine divergences must be tracked as issues and must not be presented here as accepted exceptions to normative behavior. Any future behavior change updates protobuf, this specification, both reference implementations, and conformance coverage in the same change.
