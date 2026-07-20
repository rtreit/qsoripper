# LookupService Reference

The `LookupService` is the app-facing callsign lookup interface. It maps to the lookup architecture:

```
Client → LookupService → LookupCoordinator → CallsignProvider → QrzProvider
```

Proto definition: [`proto/services/lookup_service.proto`](../../proto/services/lookup_service.proto)

Service envelopes: `lookup_request.proto`, `lookup_response.proto`, `stream_lookup_request.proto`, `stream_lookup_response.proto`, `get_cached_callsign_request.proto`, `get_cached_callsign_response.proto`, `get_dxcc_entity_request.proto`, `get_dxcc_entity_response.proto`, `batch_lookup_request.proto`, `batch_lookup_response.proto`

Domain payloads: [`proto/domain/lookup_result.proto`](../../proto/domain/lookup_result.proto), [`proto/domain/lookup_state.proto`](../../proto/domain/lookup_state.proto), [`proto/domain/callsign_record.proto`](../../proto/domain/callsign_record.proto), [`proto/domain/dxcc_entity.proto`](../../proto/domain/dxcc_entity.proto), [`proto/domain/debug_http_exchange.proto`](../../proto/domain/debug_http_exchange.proto)

All RPCs use unique request/response envelopes. Shared domain payloads stay nested inside those envelopes so each RPC can evolve independently.

## Implementation Status

| RPC | Status | Notes |
|---|---|---|
| `Lookup` | Implemented | Unary callsign lookup via coordinator |
| `StreamLookup` | Implemented | Server-streaming with `Loading → Found/Error` state transitions |
| `GetCachedCallsign` | Implemented | L1 in-memory cache check only, no network call |
| `GetDxccEntity` (by `dxcc_code`) | Implemented | Returns the entity for a numeric DXCC code, or `NOT_FOUND` |
| `GetDxccEntity` (by `prefix`) | Unimplemented | Returns `UNIMPLEMENTED` in both hosts |
| `BatchLookup` | Implemented | Bounded-concurrency parallel lookup over the coordinator (max 5 in-flight) |

Source-of-truth references for parity checks:

- Rust: [`src/rust/qsoripper-server/src/main.rs`](../../src/rust/qsoripper-server/src/main.rs) (`get_dxcc_entity`, `batch_lookup`)
- .NET: [`src/dotnet/QsoRipper.Engine.DotNet/GrpcServices.cs`](../../src/dotnet/QsoRipper.Engine.DotNet/GrpcServices.cs) (`GetDxccEntity`, `BatchLookup`)

When changing or adding RPCs, update this table and the matching capability list in the engine specification in the same change. A lightweight parity test lives at [`tests/Docs.LookupParity.Tests.ps1`](../../tests/Docs.LookupParity.Tests.ps1). Run `Invoke-Pester -Path tests/Docs.LookupParity.Tests.ps1` to confirm the docs above still match both the Rust and .NET hosts.

## RPCs

### Lookup

Single unary callsign lookup. Resolves through the cache then provider.

```
rpc Lookup(LookupRequest) returns (LookupResponse)
```

**Request:** `LookupRequest`

| Field | Type | Description |
|---|---|---|
| `callsign` | `string` | Callsign to look up (for example, `"W1AW"`) |
| `skip_cache` | `bool` | If `true`, bypasses the L1 in-memory cache and forces a fresh provider fetch |

**Response:** `LookupResponse`

| Field | Type | Description |
|---|---|---|
| `result` | `LookupResult` | Final lookup outcome payload |

**Behavior:**
- Always returns a single `LookupResponse` envelope whose `result` field carries the lookup outcome.
- If the provider is not configured (no QRZ credentials), returns `state == ERROR` with a configuration error message.
- If the callsign is in the L1 cache and `skip_cache` is false, serves the cached result with `cache_hit == true`.
- Provider results can include redacted `debug_http_exchanges` entries for login and lookup HTTP calls. Cache-only responses leave this list empty.

**Debug capture payload:**

Each `DebugHttpExchange` is an additive, provider-agnostic transport capture with:

- `provider_name`
- `operation`
- `started_at_utc`
- `duration_ms`
- `attempt`
- `method`
- `url`
- `request_headers`
- `request_body` (optional)
- `response_status_code` (optional)
- `response_headers`
- `response_body` (optional)
- `error_message` (optional)

The engine redacts sensitive values before it returns the exchange to clients. For QRZ XML, it redacts keys, passwords, tokens, and authentication headers.

**Notable status codes:**
- `OK` - returned in all cases (including not-found). The `state` field carries the semantic outcome.
- `INTERNAL` - unexpected server error. The engine reports most errors in `LookupResult.state`.

---

### StreamLookup

Server-streaming lookup that emits progressive state updates as the lookup progresses.

```
rpc StreamLookup(StreamLookupRequest) returns (stream StreamLookupResponse)
```

**Request:** `StreamLookupRequest`

| Field | Type | Description |
|---|---|---|
| `callsign` | `string` | Callsign to look up (for example, `"W1AW"`) |
| `skip_cache` | `bool` | If `true`, bypasses the L1 in-memory cache and forces a fresh provider fetch |

**Response stream:** One or more `StreamLookupResponse` messages, terminated by the server.

Each streamed envelope carries a `result: LookupResult` payload.

**State transition sequence:**

```
LOADING → (STALE)? → FOUND | NOT_FOUND | ERROR
```

1. The server always emits a `LOADING` result first, so the client can show an in-progress indicator immediately.
2. A stale cached entry can exist while the server gets fresh data.
   The server can send the cached `record` in a `STALE` result.
3. The stream closes after the terminal result (`FOUND`, `NOT_FOUND`, or `ERROR`).

**Typical stream for a fresh lookup (no cache):**
```
{ state: LOADING, queried_callsign: "W1AW" }
{ state: FOUND,   queried_callsign: "W1AW", record: { ... }, lookup_latency_ms: 240 }
```

**Typical stream for a cache hit:**
```
{ state: LOADING, queried_callsign: "W1AW" }
{ state: FOUND,   queried_callsign: "W1AW", record: { ... }, cache_hit: true, lookup_latency_ms: 1 }
```

**Use case:** TUI/GUI clients that want to show an in-progress spinner while the lookup is running. Subscribe to the stream and update the UI on each received `LookupResult`.

**Notable status codes:**
- `OK` - stream completed normally.
- `CANCELLED` - client cancelled the stream (expected for type-ahead debounce scenarios).

---

### GetCachedCallsign

Returns the cached `LookupResult` for a callsign without making any network call.

```
rpc GetCachedCallsign(GetCachedCallsignRequest) returns (GetCachedCallsignResponse)
```

**Request:** `GetCachedCallsignRequest`

| Field | Type | Description |
|---|---|---|
| `callsign` | `string` | Callsign to check in the L1 cache |

**Response:** `GetCachedCallsignResponse`

- `result` contains the cached `LookupResult`.
- If the callsign is in the L1 cache: `result.state == FOUND` (or the cached state), `result.cache_hit == true`.
- If the callsign is not cached: `result.state == NOT_FOUND`, `result.cache_hit == false`.

**No network calls are made.** This RPC is safe to call speculatively and at high frequency.

**Use case:** A type-ahead display first checks the cache. It can then call `StreamLookup` for a current result.

**Notable status codes:**
- `OK` - always returned. Outcome is in `LookupResult.state`.

---

### GetDxccEntity

Look up a DXCC (DX Century Club) entity by numeric code or callsign prefix.

```
rpc GetDxccEntity(GetDxccEntityRequest) returns (GetDxccEntityResponse)
```

> **Status:** Implemented for the `dxcc_code` query case. The `prefix` query case still returns `UNIMPLEMENTED` in both built-in hosts.

**Request:** `GetDxccEntityRequest` (oneof)

| Field | Type | Description |
|---|---|---|
| `dxcc_code` | `uint32` | Numeric DXCC entity code |
| `prefix` | `string` | Callsign prefix - reserved for future QRZ-style 4→3→2 letter reduction. Currently `UNIMPLEMENTED` |

**Response:** `GetDxccEntityResponse`

| Field | Type | Description |
|---|---|---|
| `entity` | `DxccEntity` | The matched DXCC payload |

**Notable status codes:**
- `NOT_FOUND` - `dxcc_code` does not match any known DXCC entity.
- `UNIMPLEMENTED` - `prefix` query case is not yet supported.
- `INVALID_ARGUMENT` - the request supplies neither `dxcc_code` nor `prefix`.

---

### BatchLookup

Look up multiple callsigns in a single request. Intended for contest prefetch scenarios.

```
rpc BatchLookup(BatchLookupRequest) returns (BatchLookupResponse)
```

> **Status:** Implemented in both built-in hosts. Runs the supplied callsigns through the lookup coordinator in parallel with a bounded concurrency cap (currently 5 in-flight).

**Request:** `BatchLookupRequest`

| Field | Type | Description |
|---|---|---|
| `callsigns` | `repeated string` | List of callsigns to look up |

**Response:** `BatchLookupResponse`

| Field | Type | Description |
|---|---|---|
| `results` | `repeated LookupResult` | One result per requested callsign, in request order |

**Use case:** Populate the cache before a contest session. Look up the expected callsigns in one call.

**Notable status codes:**
- `OK` - normal response, including the empty-input case (returns an empty `results` list).
- `INTERNAL` - surfaced if a per-callsign worker task fails unexpectedly.

---

## LookupState Values

| Value | Meaning |
|---|---|
| `LOOKUP_STATE_UNSPECIFIED` | Default or zero value. Normal responses do not contain this value. |
| `LOOKUP_STATE_LOADING` | Request is in flight. Used as the initial `StreamLookup` emission |
| `LOOKUP_STATE_FOUND` | The lookup found the callsign. The response contains `record`. |
| `LOOKUP_STATE_NOT_FOUND` | Callsign does not exist in the provider |
| `LOOKUP_STATE_ERROR` | Provider error (network failure, auth failure, rate limit) |
| `LOOKUP_STATE_STALE` | Returning cached data while a background refresh is pending |
| `LOOKUP_STATE_CANCELLED` | A newer request replaced the lookup. |

## Error Handling Notes

- `LookupService` returns gRPC `OK` for most responses. The semantic outcome is always in `LookupResult.state`.
- Treat `LOOKUP_STATE_ERROR` as a soft error. Record the `error_message` and show user feedback. The provider can recover on the next request.
- `LOOKUP_STATE_STALE` means the engine returns old data during a cache refresh. Display the old data. Update it after the next result.
- Clients must accept `CANCELLED` responses. These responses occur when a type-ahead request replaces an older request.
