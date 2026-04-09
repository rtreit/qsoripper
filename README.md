# LogRipper

High-performance ham radio logging system built for speed, clean workflows, and keyboard-first operation.

## Architecture

LogRipper separates the **core engine** from all UI surfaces. The engine handles logging, lookups, caching, and sync — UIs are just consumers of that functionality.

```
┌─────────────────────────────────────────────┐
│  Rust Process (engine)                      │
│                                             │
│  logripper-core:                            │
│    Log storage, QSO CRUD, QRZ lookups,      │
│    cache, ADIF parser, gRPC server (tonic)  │
│                                             │
│  logripper-tui:                             │
│    Terminal UI (ratatui) — in-process        │
└──────────────────┬──────────────────────────┘
                   │ gRPC (protobuf)
┌──────────────────▼──────────────────────────┐
│  .NET Process (rich client)                 │
│    Avalonia GUI, reporting, analytics       │
│    gRPC client (Grpc.Net.Client)            │
└─────────────────────────────────────────────┘
```

**Rust** owns the core engine, TUI, QRZ providers, and gRPC server. **.NET** owns the GUI, reporting, and acts as a gRPC client. A future web UI would just be another gRPC client — the engine doesn't know or care what's calling it.

### Protocol Buffers

Proto files under `proto/` are the **single source of truth** for all shared types (`QsoRecord`, `CallsignRecord`, `LookupResult`, bands, modes, etc.). Code is generated for both languages — zero hand-duplicated types:

- **Rust**: `prost` + `tonic-build` generate structs and gRPC server stubs
- **C#**: `Grpc.Tools` generates classes and gRPC client stubs
- **Schema quality**: `buf lint` and `buf breaking` enforce conventions and backward compatibility

### gRPC Services

| Service | Purpose |
|---|---|
| **LookupService** | Callsign lookups — single, streaming, batch, cached, DXCC |
| **LogbookService** | QSO CRUD, QRZ logbook sync, ADIF import/export |

### ADIF

ADIF (Amateur Data Interchange Format) is used **only at the edges** — QRZ API calls and file I/O. Internal communication always uses protobuf. The Rust ADIF parser converts to/from proto types at the boundary, with an `extra_fields` map for lossless round-tripping.

## Project Structure

```
proto/
  domain/       CallsignRecord, QsoRecord, LookupResult, enums
  services/     LookupService, LogbookService gRPC definitions
crates/
  logripper-core/   Engine: storage, lookups, cache, ADIF, gRPC server
  logripper-tui/    Terminal UI (depends on logripper-core)
docs/
  architecture/     Data model docs, design decisions
  integrations/     ADIF spec reference, provider notes
```

## License

MIT
