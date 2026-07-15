# CatHub integration

CatHub is a standalone station service maintained at
<https://github.com/treitforge/cathub>. It lets QsoRipper, N1MM, WSJT-X, Log4OM,
panadapter software, and other clients share one radio and WinKeyer safely.

QsoRipper does not require CatHub. Without it, QsoRipper can use an ordinary external
`rigctld`, a directly connected WinKeyer, or no rig and keyer integration at all. Logging
continues normally when CatHub is stopped or unavailable.

## Install CatHub

Build or install CatHub from its standalone repository. QsoRipper resolves its executable in
this order:

1. The path in `CATHUB_EXECUTABLE`.
2. A binary bundled at `artifacts\publish\cathub\Release\cathub.exe` on Windows, or the
   equivalent platform path.
3. `cathub` on `PATH`.

QsoRipper never downloads or updates CatHub during startup.
The current supported executable range is `>=0.1.0 <0.2.0`; the launcher checks
`cathub --version` and reports an incompatible version separately from a spawn failure.

## Configuration modes

### QsoRipper-managed unified configuration

Existing `[cat_hub]` settings in QsoRipper's per-user `config.toml` remain supported. The
QsoRipper settings UI may edit this section, and the launcher passes the unified file to
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
cathub config validate --config "$env:APPDATA\qsoripper\config.toml"
cathub config print-effective --config "$env:APPDATA\qsoripper\config.toml"
```

Extract `[cat_hub]` into a standalone file without modifying the source:

```powershell
cathub config migrate `
  --from "$env:APPDATA\qsoripper\config.toml" `
  --output "$env:APPDATA\cathub\cathub.toml"
```

Migration refuses to overwrite an existing destination unless `--force` is supplied.
Source removal is opt-in and creates a `.bak` file first.

## Start with the QsoRipper launcher

Select **CatHub standalone service** in `launcher.ps1`. The launcher:

1. Chooses the unified QsoRipper file when it contains `[cat_hub]`; otherwise it uses the
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

The first standalone CatHub release preserves the existing `qsoripper.services` WinKeyer
broker wire package. The authoritative contract now lives in the CatHub repository. The
QsoRipper contract copy remains temporarily pinned for the initial compatibility release and
must track the supported CatHub protocol version exactly.

The dependency pin is recorded in `config\cathub-dependency.json`. When both repositories are
checked out as siblings, verify the temporary QsoRipper protocol snapshot with:

```powershell
.\scripts\Test-CatHubProtocol.ps1
```

See the CatHub repository for the complete radio topology, virtual serial setup, permissions,
safety behavior, WinKeyer maintenance rules, and troubleshooting guide.
