# Data Model Architecture

## Overview

QsoRipper uses **Protocol Buffers (proto3)** for all shared domain types and service contracts.
Proto files are the single source of truth.
The build generates Rust structures and C# classes from these files.
Thus, engine hosts and clients use the same cross-language contracts.

## Why Protocol Buffers?

| Concern | How protobuf addresses it |
|---|---|
| Cross-language type safety | Code generation for Rust (`prost`) and C# (`Grpc.Tools`) from one schema |
| Wire format | Binary protobuf over gRPC for inter-process communication |
| Forward compatibility | Proto3 ignores unknown fields - matches QRZ API's own forward-compat model |
| Schema evolution | Adding optional fields is non-breaking. `buf breaking` enforces rules |
| Performance | Binary serialization is fast and compact - no JSON parsing overhead on the hot path |

## Architecture Alignment

The proto-first approach directly supports these architecture principles:

- **Principle #6 (Normalize Data Immediately)**: Providers parse QRZ XML or ADIF responses and map them to proto domain types. Internal communication always uses normalized types.
- **Principle #8 (Consumer-Driven Interfaces)**: UI requirements control proto message design. For example, `LookupResult` contains state, data, and latency.
- **Stable core, volatile edges**: Proto domain types are the stable core. QRZ XML parsing, ADIF parsing, and HTTP concerns are edge adapters that produce proto types.

## Proto Layout Rules

QsoRipper treats the protobuf 1-1-1 guidance as an architectural rule, not a style preference:

- **One top-level entity per file by default**: messages, enums, and services each get their own `.proto` file.
- **Service declaration files contain only the service**: request, response, stream item, enum, and support payload messages live in separate files under `proto/services/`.
- **Every RPC gets unique request/response envelopes**: unary and streaming methods both use method-specific `XxxRequest` / `XxxResponse` messages.
- **Keep reusable business payloads separate from envelopes.**
  Put shared models inside the method-specific envelopes.
  Examples include `LookupResult`, `QsoRecord`, `SetupStatus`, and `ActiveStationContext`.
- **Exceptions are rare and explicit**: The schema review must document and justify each change from 1-1-1. Do not use RPC envelopes for exceptions.

## Directory Structure

```
proto/
├── domain/
│   ├── callsign_record.proto
│   ├── dxcc_entity.proto
│   ├── lookup_result.proto
│   ├── lookup_state.proto
│   ├── qso_record.proto
│   ├── station_profile.proto
│   ├── station_snapshot.proto
│   └── ... one top-level reusable domain type per file
└── services/
    ├── lookup_service.proto
    ├── lookup_request.proto
    ├── lookup_response.proto
    ├── stream_lookup_request.proto
    ├── stream_lookup_response.proto
    ├── log_qso_request.proto
    ├── log_qso_response.proto
    └── ... one top-level service envelope/support type per file
```

## Engine and client split

| Component | Language | Rationale |
|---|---|---|
| Rust engine host (`qsoripper-server`) | Rust | Main high-performance engine/runtime path |
| .NET engine host (`QsoRipper.Engine.DotNet`) | C# / .NET | Second real engine implementation behind the same contracts |
| TUI client | Rust (ratatui) | Keyboard-first client proving Rust can consume the gRPC seam too |
| CLI / GUI / DebugHost clients | C# / .NET | Rich client and debugging surfaces on the shared contracts |

**Key rule:** the protobuf/gRPC contract is the stable core. Any process that implements it can be an engine host, and any process that consumes it can be a client. Rust is no longer "the engine" as an architectural requirement. It is one engine implementation in the current repository.

## Core Domain Types

### CallsignRecord (`callsign_record.proto`)

Normalized representation of a ham radio operator/station. Derived from QRZ XML lookup data (40+ fields) but owned by QsoRipper. Field groups:

- **Identity**: callsign, aliases, previous_call, dxcc_entity_id
- **Name**: first_name, last_name, nickname, formatted_name
- **Address**: attention, addr1, addr2, state, zip, country, country_code
- **Location**: latitude, longitude, grid_square, county, fips, geo_source
- **License**: license_class, effective_date, expiration_date, license_codes
- **Contact**: email, web_url, qsl_manager
- **QSL preferences**: eqsl, lotw, paper_qsl (tri-state enum)
- **Zone**: cq_zone, itu_zone, iota
- **Metadata**: qrz_serial, last_modified, bio_length, image_url, and other items

### QsoRecord (`qso_record.proto`)

The core QSO (contact) entity. Every logged contact is a QsoRecord.

- **Identity**: local_id (UUID assigned by QsoRipper), qrz_logid (from QRZ sync)
- **Core**: station_callsign, worked_callsign, utc_timestamp, utc_end_timestamp, band, mode, submode, frequency_hz
- **Signal**: rst_sent, rst_received (structured RstReport), tx_power
- **QSL**: sent/received status for card, LoTW, eQSL
- **Enrichment**: worked_operator_name, worked_grid, worked_country, worked_dxcc, worked_continent
- **Contest**: contest_id, serial_sent/received, exchange_sent/received
- **Propagation**: prop_mode, sat_name, sat_mode
- **Sync**: sync_status (local_only → synced → modified → conflict)
- **ADIF overflow**: extra_fields map preserves unrecognized ADIF fields for lossless round-trip

### LookupResult (`lookup_result.proto`)

Wraps the async state machine for callsign lookups:

```
Loading → Found | NotFound | Error | Stale | Cancelled
```

It contains a state enum, optional `CallsignRecord`, `cache_hit`, and `lookup_latency_ms`.
Each non-`Loading` result also contains local prior-QSO history.
The history uses `prior_qsos` and `prior_qso_total_count`.

### QsoHistoryEntry (`qso_history_entry.proto`)

This message is a compact summary of a prior QSO with the queried callsign.
It accompanies `LookupResult`.
Thus, a UI can show a "worked before" badge without a second request.
Its fields are a strict subset of `QsoRecord`:

- `local_id`
- `utc_timestamp`
- `band`
- `mode`
- `submode`
- `frequency_hz`
- `frequency_rx_hz`
- `contest_id`

The message supports normal-mode badges and future contest duplicate rules.
`(band, mode, contest_id)` supports the common duplicate rules.
`contest_id` separates current-contest contacts from past contacts.
Contest mode is not implemented.
The message supports a later additive implementation.

### Supporting Enums

- **Band**: 2190m through submm (33 values, full ADIF 3.1.7 enumeration with frequency ranges)
- **Mode**: 45 modes matching the complete ADIF 3.1.7 Mode enumeration. A string field stores submodes.
- **GeoSource**: user, geocode, grid, zip, state, dxcc, none (maps to QRZ geoloc values)
- **SyncStatus**: local_only, synced, modified, conflict
- **QslStatus**: no, yes, requested, queued, ignore (aligned with ADIF QSL Sent/Rcvd enums)
- **QslPreference**: unknown, yes, no (tri-state for QRZ's 0/1/blank)

## gRPC Services

### LookupService

The app-facing lookup interface from the architecture diagram:

```
Client → LookupService → Engine-specific lookup coordinator/provider chain
```

Key RPCs:
- `Lookup` - single request/response
- `StreamLookup` - server-streaming progressive updates (Loading → Stale → Found)
- `GetCachedCallsign` - L1 cache-only check
- `GetDxccEntity` - DXCC entity lookup
- `BatchLookup` - contest prefetch

Each RPC returns a unique service envelope.
Examples include `LookupResponse`, `StreamLookupResponse`, and `GetCachedCallsignResponse`.
Shared payloads remain inside these envelopes.
Thus, each RPC can change independently.
Both built-in hosts implement unary, stream, and cache lookup.
They advertise `lookup-callsign`, `lookup-stream`, and `lookup-cache`.

Both hosts also implement `BatchLookup`.
They implement `GetDxccEntity` for numeric `dxcc_code` queries.
The callsign `prefix` query still returns `UNIMPLEMENTED`.
See [`docs/api/lookup-service.md`](../api/lookup-service.md) for the RPC support table.

### LogbookService

QSO lifecycle management:

- `LogQso` / `UpdateQso` / `DeleteQso` - CRUD with optional immediate QRZ sync
- `ListQsos` - filtered/paginated query with server-streaming response
- `SyncWithQrz` - full or incremental sync, streams progress updates
- `ImportAdif` / `ExportAdif` - client-streaming import, server-streaming export

Logbook RPCs use the same envelope rule.
`ListQsos` streams `ListQsosResponse` envelopes that contain `QsoRecord`.
`ExportAdif` streams `ExportAdifResponse` envelopes that contain `AdifChunk`.
Unary RPCs use method-specific envelopes for single shared payloads.

## ADIF as External Format

QsoRipper uses ADIF (Amateur Data Interchange Format) only for:

1. **QRZ Logbook API** - INSERT/FETCH use ADIF-encoded QSO data
2. **File import/export** - standard `.adi` files from other logging programs
3. **Contest log submission** - Cabrillo/ADIF export

ADIF is **never** used for internal IPC. Engine-specific ADIF adapters convert to/from proto `QsoRecord` at the edge.

### ADIF Round-Trip Strategy

QsoRecord includes an `extra_fields` map (`map<string, string>`) for ADIF fields without dedicated proto fields.
Examples are satellite information, propagation conditions, and application-defined fields.
Core local-station ADIF fields now flow through `station_snapshot` instead of `extra_fields`.
During import:

1. Recognized fields → mapped to dedicated QsoRecord fields
2. Unrecognized fields → stored in `extra_fields` (keyed by uppercase ADIF field name)
3. During export, emit dedicated fields first.
4. Then append `extra_fields`.

This ensures no data loss when round-tripping ADIF files through QsoRipper.

See `docs/integrations/adif-specification.md` for the complete ADIF 3.1.7 reference including all 150+ QSO fields, data types, enumerations, and field-to-proto mapping table.

## Code Generation

### Tooling

- **buf** - schema linting (`buf lint`) and breaking change detection (`buf breaking`)
- **prost + tonic-build** - Rust struct/gRPC generation during Cargo builds
- **Grpc.Tools** - C# class/gRPC generation during MSBuild

### Build integration

```
# Lint proto files
buf lint

# Check for breaking changes against main branch
buf breaking --against '.git#branch=main'

# Regenerate Rust bindings
cargo build --manifest-path src/rust/Cargo.toml

# Regenerate C# bindings
dotnet build src/dotnet/QsoRipper.slnx
```

### Generated output locations

| Language | Output path | Notes |
|---|---|---|
| Rust | Cargo `OUT_DIR` under `src/rust/target/` | Generated at build time by `src/rust/qsoripper-core/build.rs`. Not checked in |
| C# | MSBuild intermediate output under `src/dotnet/**/obj/` | Generated at build time by `Grpc.Tools`. Not checked in |

## Adding a New Field

1. Add the field to the appropriate `.proto` file with the next available field number
2. Run `buf lint` to verify naming conventions
3. Run `buf breaking` to verify backward compatibility
4. Rebuild the Rust workspace and .NET consumers so generated bindings refresh
5. Update the provider adapter (for example, QRZ XML parser) to populate the new field
6. Update the UI components that display the field.

**Important:** Never reuse or reassign proto field numbers. Mark deleted fields with `reserved`.

## Conventions

- **Field numbering**: Group related fields in ranges (identity: 1-9, name: 10-19, address: 20-29, and other items)
- **Field naming**: snake_case in proto files (auto-converted to PascalCase in C#, snake_case in Rust)
- **Optional fields**: Use the `optional` keyword for fields that the provider can omit.
- **Enums**: Prefer `_UNSPECIFIED = 0` when the schema has a neutral default. An operational default can keep a domain-specific zero value.
- **Timestamps**: Use `google.protobuf.Timestamp` for all date/time fields
- **C# namespace**: Set via `option csharp_namespace = "QsoRipper.Domain"` or `"QsoRipper.Services"`
- **Packages**: Keep the current `proto/domain` and `proto/services` layout with `qsoripper.domain` / `qsoripper.services` packages until the project deliberately introduces versioned external contracts
- **1-1-1 layout**: Default to one top-level message, enum, or service per `.proto` file
- **RPC message shapes**: Give each RPC unique `XxxRequest` and `XxxResponse` envelopes.
  Put shared payloads inside these envelopes.
