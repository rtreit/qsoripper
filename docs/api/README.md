# QsoRipper Engine API

This is the canonical entry point for anyone integrating with the shared QsoRipper gRPC/protobuf surface — whether you are building a client or implementing a new engine host.

QsoRipper is **contract-first**. `proto/` is the stable seam. Engine hosts implement it. Clients consume it. The repository already uses that model in both directions: Rust and .NET engine hosts sit behind the same contracts, and the Rust TUI plus the .NET CLI/GUI/DebugHost consume those contracts as independent clients.

## Services

| Service | Purpose | Reference |
|---|---|---|
| **EngineService** | Engine identity, version, and runtime capability discovery | [`proto/services/engine_service.proto`](../../proto/services/engine_service.proto) |
| **SetupService** | First-run setup, persisted config status, bootstrap storage/station defaults | [setup-service.md](setup-service.md) |
| **StationProfileService** | Persisted station profile CRUD, active selection, bounded session override state | [station-profile-service.md](station-profile-service.md) |
| **LookupService** | Callsign lookups — single, streaming, batch, cached, DXCC | [lookup-service.md](lookup-service.md) |
| **LogbookService** | QSO CRUD, QRZ logbook sync, ADIF import/export | [logbook-service.md](logbook-service.md) |
| **DeveloperControlService** | Developer-only runtime config overrides and diagnostics | [`proto/services/developer_control_service.proto`](../../proto/services/developer_control_service.proto) |
| **SpaceWeatherService** | Current space-weather snapshot plus explicit refresh | [`proto/services/space_weather_service.proto`](../../proto/services/space_weather_service.proto) |

## Contract Source of Truth

All service and domain types are defined in `proto/`. QsoRipper treats protobuf 1-1-1 and per-RPC envelopes as an architectural rule:

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
- Shared business payloads live in dedicated domain or service support messages and are nested inside envelopes.
- Service files contain only the `service`; request/response/support messages live beside them as their own files.

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
| **Browser / web** | gRPC-Web via proxy | Browsers cannot issue raw HTTP/2 gRPC frames — see [client-integration.md](client-integration.md#browser-and-web-clients) |

> **Browser clients** require an intermediate proxy or gateway (e.g., Envoy with the gRPC-Web filter, or a gRPC-Web-aware reverse proxy). Direct raw gRPC from a browser is not supported without this layer. See the integration guide for details.

## Implementation Status

Not every contract entry is implemented by every current engine host. The reference docs for each service include per-RPC status tables, and `EngineService.GetEngineInfo` exposes capability strings so clients can inspect what a running host actually supports.

In general:

- Both built-in engine hosts implement the common first slice used by the conformance harness: engine info, setup, station profiles, runtime config, logbook CRUD, sync status, ADIF import/export, rig status, space weather, and callsign lookup via unary/stream/cache RPCs.
- Both built-in hosts still leave `LookupService.GetDxccEntity` and `LookupService.BatchLookup` unimplemented in this slice.
- The built-in engine hosts intentionally report fine-grained lookup capabilities (`lookup-callsign`, `lookup-stream`, `lookup-cache`) instead of a broad `lookup` bucket so discovery matches the actual implemented surface.

The current proto contract should now be treated as the stable **post-1-1-1 baseline**. PR [#74](https://github.com/rtreit/qsoripper/pull/74) was a deliberate breaking-contract cutover while the project is still early. From this baseline forward, additive changes are preferred and client code generated from the current proto files should continue to compile as new fields and RPCs are added. See [client-integration.md](client-integration.md#schema-evolution-and-compatibility) for field tolerance guidance.

## Quick Links

- [Client Integration Guide](client-integration.md) — generating stubs, connecting, browser transport
- [Workflow Examples](workflows.md) — request/response shapes for common flows
- [SetupService Reference](setup-service.md)
- [StationProfileService Reference](station-profile-service.md)
- [LookupService Reference](lookup-service.md)
- [LogbookService Reference](logbook-service.md)
- [Data Model Architecture](../architecture/data-model.md) — architecture-oriented context for domain types
