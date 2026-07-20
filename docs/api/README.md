# QsoRipper Engine API

This document is the main reference for the shared QsoRipper gRPC and protobuf interface.
Use it to build a client or a new engine host.

QsoRipper is **contract-first**.
`proto/` is the stable interface.
Engine hosts implement it, and clients use it.
The Rust and .NET engine hosts implement the same contracts.
The Rust TUI and the .NET clients use these contracts independently.

## Services

| Service | Purpose | Reference |
|---|---|---|
| **EngineService** | Engine identity, version, and runtime capability discovery | [`proto/services/engine_service.proto`](../../proto/services/engine_service.proto) |
| **SetupService** | First-run setup, persisted config status, bootstrap storage/station defaults | [setup-service.md](setup-service.md) |
| **StationProfileService** | Persisted station profile CRUD, active selection, bounded session override state | [station-profile-service.md](station-profile-service.md) |
| **LookupService** | Callsign lookups - single, streaming, batch, cached, DXCC | [lookup-service.md](lookup-service.md) |
| **LogbookService** | QSO CRUD, QRZ logbook sync, ADIF import/export | [logbook-service.md](logbook-service.md) |
| **DeveloperControlService** | Developer-only runtime config overrides and diagnostics | [`proto/services/developer_control_service.proto`](../../proto/services/developer_control_service.proto) |
| **SpaceWeatherService** | Current space-weather snapshot plus explicit refresh | [`proto/services/space_weather_service.proto`](../../proto/services/space_weather_service.proto) |
| **ContestCalendarService** | Active contest lookup plus explicit calendar refresh | [`proto/services/contest_calendar_service.proto`](../../proto/services/contest_calendar_service.proto) |

## Contract Source of Truth

The files in `proto/` define all service and domain types. QsoRipper uses protobuf 1-1-1 and one envelope for each RPC:

```
proto/
├── domain/
│   ├── callsign_record.proto
│   ├── dxcc_entity.proto
│   ├── lookup_result.proto
│   ├── qso_record.proto
│   ├── station_profile.proto
│   └── ... one reusable domain type per file
└── services/
    ├── setup_service.proto                # service declaration only
    ├── station_profile_service.proto      # service declaration only
    ├── lookup_service.proto               # service declaration only
    ├── logbook_service.proto              # service declaration only
    ├── developer_control_service.proto    # service declaration only
    ├── lookup_request.proto               # per-RPC envelope
    ├── lookup_response.proto              # per-RPC envelope
    └── ... one envelope/support type per file
```

Rules of thumb:

- Every RPC uses a unique `XxxRequest` and `XxxResponse` envelope, including streaming RPCs.
- Dedicated domain or service messages contain shared business payloads. RPC envelopes contain those messages.
- Service files contain only the `service`. Request/response/support messages live beside them as their own files.

The `.proto` files are the durable reference source. Comments inside them document individual field and RPC semantics. The reference docs in this directory provide higher-level integration guidance and implementation-status tables on top of those definitions.

## Transport

Engine hosts speak native gRPC (HTTP/2 + binary protobuf).

Built-in local engine profiles:

| Profile | Engine ID | Default endpoint |
|---|---|---|
| `local-rust` | `rust-tonic` | `http://127.0.0.1:50051` |
| `local-dotnet` | `dotnet-aspnet` | `http://127.0.0.1:50052` |

| Client type | Recommended transport | Notes |
|---|---|---|
| **Native desktop / TUI / CLI** | Native gRPC (HTTP/2) | Any gRPC client library works directly |
| **Browser / web** | gRPC-Web via proxy | Browsers cannot issue raw HTTP/2 gRPC frames - see [client-integration.md](client-integration.md#browser-and-web-clients) |

> **Browser clients** require an intermediate proxy or gateway (for example, Envoy with the gRPC-Web filter, or a gRPC-Web-aware reverse proxy). Direct raw gRPC from a browser is not supported without this layer. See the integration guide for details.

## Implementation Status

Current engine hosts do not implement every contract entry. Each service document contains an RPC status table. `EngineService.GetEngineInfo` supplies host capability strings.

In general:

- Both built-in engine hosts implement the common conformance features.
  These features include engine information, setup, station profiles, runtime configuration, and logbook CRUD.
  They also include sync status, ADIF import/export, rig status, space weather, and contest calendar lookup.
  Callsign lookup supports unary, stream, and cache RPCs.
- Both built-in hosts also implement `LookupService.BatchLookup` and `LookupService.GetDxccEntity` for the `dxcc_code` query case. The `prefix` query case of `GetDxccEntity` still returns `UNIMPLEMENTED` in both hosts.
- The built-in engine hosts report fine-grained lookup capabilities (`lookup-callsign`, `lookup-stream`, `lookup-cache`) instead of a broad `lookup` bucket so discovery matches the actually implemented surface.

Treat the current proto contract as the stable **post-1-1-1 baseline**.
PR [#74](https://github.com/rtreit/qsoripper/pull/74) intentionally changed the contract.
Prefer additive changes after this baseline.
Generated client code must compile after a schema adds fields or RPCs.
See [client-integration.md](client-integration.md#schema-evolution-and-compatibility) for field tolerance guidance.

## Quick Links

- [Client Integration Guide](client-integration.md) - generating stubs, connecting, browser transport
- [Workflow Examples](workflows.md) - request/response shapes for common flows
- [SetupService Reference](setup-service.md)
- [StationProfileService Reference](station-profile-service.md)
- [LookupService Reference](lookup-service.md)
- [LogbookService Reference](logbook-service.md)
- [Data Model Architecture](../architecture/data-model.md) - architecture-oriented context for domain types
