# CatHub integration

CatHub is a standalone station service maintained at
<https://github.com/treitforge/cathub>. It lets QsoRipper, N1MM, WSJT-X, Log4OM,
panadapter software, and other clients share one radio and WinKeyer safely.

QsoRipper does not require CatHub. Without it, QsoRipper can use an ordinary external
`rigctld`, a directly connected WinKeyer, or no rig and keyer integration at all. Logging
continues normally when CatHub is stopped or unavailable.

## Install the CatHub daemon

QsoRipper needs the CatHub executable, not the `cathub-protocol` Rust crate or the
`CatHub.Protocol` NuGet package. Those packages are for applications that develop against
CatHub's typed WinKeyer API.

Download the CatHub 0.2.0 Windows or Linux archive and adjacent SHA-256 checksum from the
[GitHub release](https://github.com/treitforge/cathub/releases/tag/v0.2.0). Verify the
checksum, extract the executable, and either add its directory to `PATH` or set
`CATHUB_EXECUTABLE`.

If Rust 1.88 or newer is installed, install the same release from crates.io:

```powershell
cargo install cathub --version 0.2.0
```

Cargo downloads `cathub-protocol` automatically while building the daemon. The protocol
crate is not a separate runtime installation step for the operator.

To build from source instead:

```powershell
git clone https://github.com/treitforge/cathub.git
Set-Location cathub
cargo build --release -p cathub
$env:CATHUB_EXECUTABLE = (Resolve-Path .\target\release\cathub.exe)
```

QsoRipper resolves the executable in this order:

1. The path in `CATHUB_EXECUTABLE`.
2. A binary bundled at `artifacts\publish\cathub\Release\cathub.exe` on Windows, or the
   equivalent platform path.
3. `cathub` on `PATH`.

QsoRipper does not download, install, or update CatHub during startup.
The current supported executable range is `>=0.2.0 <0.3.0`. The launcher checks
`cathub --version` and reports an incompatible version separately from a spawn failure.

## Configuration modes

### Unified configuration

Existing `[cat_hub]` settings in QsoRipper's per-user `config.toml` remain supported.
The launcher gives the unified file to CatHub with `--section cat_hub`.
CatHub ignores unrelated QsoRipper tables.

This mode keeps one station configuration file.
QsoRipper treats `[cat_hub]` as opaque data and preserves it during setup saves.
QsoRipper does not parse, validate, migrate, or write this table.
A malformed or newer CatHub section does not prevent either QsoRipper engine from starting.

### Externally managed CatHub

Standalone CatHub uses its own configuration by default:

- Windows: `%APPDATA%\cathub\cathub.toml`
- Linux: `$XDG_CONFIG_HOME/cathub/cathub.toml`, or `~/.config/cathub/cathub.toml`
- Override: `CATHUB_CONFIG_PATH`

In this mode, QsoRipper stores only its client connection settings. Configure the QsoRipper
rig-control provider to use CatHub's read endpoint, normally `127.0.0.1:4532`. Configure the
CW backend with the CatHub broker endpoint, normally `http://127.0.0.1:50071`, and a stable
client name.

QsoRipper does not parse or rewrite the external CatHub file.

## Validate and migrate configuration

CatHub owns its configuration parser, defaults, migration, and semantic validation:

```powershell
cathub --section cat_hub config validate --config "$env:APPDATA\qsoripper\config.toml"
cathub --section cat_hub config print-effective --config "$env:APPDATA\qsoripper\config.toml"
```

Extract `[cat_hub]` into a standalone file without modifying the source:

```powershell
cathub config migrate `
  --from "$env:APPDATA\qsoripper\config.toml" `
  --output "$env:APPDATA\cathub\cathub.toml"
```

Migration refuses to overwrite an existing destination unless the command includes `--force`.
Source removal is opt-in and creates a `.bak` file first.

## Start with the QsoRipper launcher

Select **CatHub standalone service** in `launcher.ps1`. The launcher:

1. Chooses the unified QsoRipper file when it contains `[cat_hub]`. Otherwise it uses the
   external CatHub path.
2. Resolves the configured, bundled, or installed CatHub executable.
3. Gives CatHub `127.0.0.1:0` for the typed WinKeyer API.
4. Starts CatHub before either engine.
5. Waits for CatHub to publish its effective endpoints.
6. Gives the selected WinKeyer endpoint to each engine.
7. Starts the selected engines and UIs only after CatHub is ready.

Windows selects an available loopback port for the typed API.
This avoids conflicts with ports that Docker, WSL, Hyper-V, or another service reserves.
The launcher validates the runtime file against the CatHub process ID.
The configured Hamlib NET port remains stable for external radio clients.

If the readiness port is already served by an externally started CatHub, the launcher leaves
that process running and treats the service as externally managed.

The compatibility wrappers remain available:

```powershell
.\scripts\Start-CatHub.ps1 -DryRun
.\scripts\Get-CatHubLog.ps1 -Follow
```

Stopping processes requires confirmation:

```powershell
.\scripts\Stop-CatHub.ps1
```

## QsoRipper client settings

For rig state, enable QsoRipper's `rigctld` provider and set its host and port to a CatHub
Hamlib NET endpoint with `read` permission. QsoRipper uses the same protocol whether the
server is CatHub or Hamlib's `rigctld`.

For shared CW keying, use these settings in `[cw_keying]` or their matching environment
variables:

```toml
[cw_keying]
backend = "cathub"
cathub_endpoint = "http://127.0.0.1:50071"
cathub_client_name = "qsoripper-engine"
transmit_enabled = false
```

The launcher overrides `cathub_endpoint` with CatHub's effective runtime endpoint.
The configured value still applies when an engine starts without the launcher.

Keep hardware transmission disabled until status and speed operations confirm the expected
keyer and broker. Enable transmission only during an attended hardware test.

## Protocol compatibility

CatHub 0.2 uses the CatHub-owned `cathub.services` WinKeyer broker wire-package identifier.
The authoritative contract lives in the CatHub repository.

The Rust engine consumes `cathub-protocol` 0.2.0. The .NET engine consumes
`CatHub.Protocol` 0.2.0. CatHub owns these packages and the source protocol files.

QsoRipper records the dependency pin in `config\cathub-dependency.json`.

See the CatHub repository for the complete radio topology, virtual serial setup, permissions,
safety behavior, WinKeyer maintenance rules, releases, and troubleshooting guide.
