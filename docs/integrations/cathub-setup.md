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

Download the CatHub 0.1.1 Windows or Linux archive and adjacent SHA-256 checksum from the
[GitHub release](https://github.com/treitforge/cathub/releases/tag/v0.1.1). Verify the
checksum, extract the executable, and either add its directory to `PATH` or set
`CATHUB_EXECUTABLE`.

If Rust 1.88 or newer is installed, install the same release from crates.io:

```powershell
cargo install cathub --version 0.1.1
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
3. A source build in a sibling CatHub checkout at `..\cathub\target\release\cathub.exe`,
   or the equivalent platform path.
4. `cathub` on `PATH`.

QsoRipper does not download, install, or update CatHub during startup.
The current supported executable range is `>=0.1.0 <0.2.0`. The launcher checks
`cathub --version` and reports an incompatible version separately from a spawn failure.

## Configuration modes

### QsoRipper-managed unified configuration

Existing `[cat_hub]` settings in QsoRipper's per-user `config.toml` remain supported.
The QsoRipper settings UI can edit this section. The launcher passes the unified file to
CatHub explicitly with `--section cat_hub`. CatHub ignores unrelated QsoRipper tables.

This mode keeps one station configuration file. QsoRipper setup saves preserve `[cat_hub]`
verbatim unless the request explicitly contains a complete CatHub settings replacement. A
malformed or newer CatHub section does not prevent either QsoRipper engine from starting.

### Externally managed CatHub

Standalone CatHub uses its own configuration by default:

- Windows: `%APPDATA%\cathub\cathub.toml`
- Linux: `$XDG_CONFIG_HOME/cathub/cathub.toml`, or `~/.config/cathub/cathub.toml`
- Override: `CATHUB_CONFIG_PATH`

In this mode, QsoRipper stores only its client connection settings. Configure the QsoRipper
rig-control provider to use CatHub's read endpoint, normally `127.0.0.1:4532`. Configure the
CW backend with the CatHub broker endpoint, normally `http://127.0.0.1:50071`, and a stable
client name.

QsoRipper does not rewrite the external CatHub file.

## Validate and migrate configuration

CatHub owns its configuration parser and semantic validation:

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
3. Starts CatHub before either engine.
4. Reads the first configured `[[hamlib_net]]` bind and waits on that readiness port.
5. Starts the selected engines and UIs only after CatHub is ready.

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

Keep hardware transmission disabled until status and speed operations confirm the expected
keyer and broker. Enable transmission only during an attended hardware test.

## Protocol compatibility

CatHub 0.1 retains the existing `qsoripper.services` WinKeyer broker wire-package identifier.
The identifier is part of the wire contract and does not make CatHub part of QsoRipper. The
authoritative contract lives in the CatHub repository.

QsoRipper currently carries a pinned protocol snapshot so its shared Rust and .NET protocol
generation remains integrated with the rest of QsoRipper's contract build. The snapshot must
track the supported CatHub protocol version exactly. Installing QsoRipper or running CatHub
does not require the published `cathub-protocol` Rust crate or `CatHub.Protocol` NuGet
package. Those packages are for independent applications that develop against CatHub's typed
API.

QsoRipper records the dependency pin in `config\cathub-dependency.json`. When both repositories are
checked out as siblings, verify the temporary QsoRipper protocol snapshot with:

```powershell
.\scripts\Test-CatHubProtocol.ps1
```

See the CatHub repository for the complete radio topology, virtual serial setup, permissions,
safety behavior, WinKeyer maintenance rules, releases, and troubleshooting guide.
