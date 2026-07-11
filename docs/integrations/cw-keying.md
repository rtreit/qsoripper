# CW keying setup

QsoRipper exposes the same engine-backed CW macro and keyer behavior through the Rust and .NET engines. The default `null` backend expands and accepts macros without touching radio hardware. Use it to verify station context and contest exchanges before enabling a WinKeyer. Use the `cathub` backend when QsoRipper and N1MM need the same physical keyer; retain the direct `winkeyer` backend for single-client stations.

## Safety model

Hardware text transmission requires both of these settings:

```text
QSORIPPER_CW_KEYER_BACKEND=winkeyer
QSORIPPER_CW_TRANSMIT_ENABLED=true
```

Selecting `winkeyer` or `cathub` alone does not authorize transmission. With the transmit gate off, status and speed commands can probe the selected backend, but `cw send` and `cw macro` fail with `FAILED_PRECONDITION` before transmitting.

Every accepted hardware send has a maximum duration. At the deadline QsoRipper requests WinKeyer status and, if the keyer is still busy, sends Clear Buffer and records the safety action in status. The default is 120 seconds. Allowed values are 1,000 through 300,000 milliseconds.

`cw abort` cancels the current watchdog and sends Clear Buffer immediately. It is safe to use when nothing is being sent.

## Dry-run verification

Start an engine with the default backend and check expansion without a radio:

```powershell
$env:QSORIPPER_CW_KEYER_BACKEND = "null"
qsoripper cw status
qsoripper cw list
qsoripper cw send "CQ TEST {MYCALL}"
qsoripper cw macro exchange --his-call W1AW --exchange WA --nr 12
qsoripper cw speed 28
```

`{MYCALL}` requires an active station profile. The other supported tokens are `{HISCALL}`, `{RST}`, `{EXCH}`, and `{NR}`. CW speed must be 5 through 99 WPM.

## WinKeyer configuration

### Shared keyer through CatHub (recommended for N1MM)

Create a dedicated com0com pair such as `COM40 <-> COM41`. CatHub opens `COM40`; N1MM opens `COM41`. Neither N1MM nor a QsoRipper engine opens physical `COM3`.

Create a second pair such as `COM42 <-> COM43` for device maintenance. CatHub opens `COM42`; WKTools opens `COM43`. Do not make WKTools and N1MM share COM41.

Add this to the unified configuration:

```toml
[cat_hub.winkeyer]
port = "COM3"
baud = 1200
max_tx_ms = 30000
api_bind = "127.0.0.1:50071"

[[cat_hub.winkeyer_face]]
name = "n1mm-cw"
transport = "COM40"
baud = 1200
primary = true
perms = ["status", "send", "control", "ptt"]

[[cat_hub.winkeyer_face]]
name = "wktools-maintenance"
transport = "COM42"
baud = 1200
primary = false
perms = ["status", "control", "config_write"]

[cw_keying]
backend = "cathub"
cathub_endpoint = "http://127.0.0.1:50071"
cathub_client_name = "qsoripper-engine"
speed_wpm = 20
transmit_enabled = false
max_tx_ms = 30000
```

Start CatHub before either engine. Configure N1MM's WinKeyer port as `COM41`, 1200 baud. Configure WKTools for `COM43`, 1200 baud. QsoRipper talks to the typed API and can remain connected while N1MM uses the virtual serial face.

CatHub schedules complete jobs without interleaving. N1MM Escape clears only N1MM's active stream; QsoRipper cancellation affects only its named client. The station-wide emergency stop remains global. A fixed-speed QsoRipper job temporarily overrides the keyer and restores the primary face's fixed or pot-controlled speed afterward.

The speed-pot notification contains an offset from the active `MIN_WPM` setting. CatHub tracks each client's Speed Pot Setup command, reports the canonical actual WPM through the typed API, and forwards the original protocol byte unchanged to N1MM.

Routine operation never writes EEPROM. Reset, EEPROM, firmware, calibration, and baud-changing commands require a face with `config_write`, an empty transmit queue, and an exclusive maintenance lease. During maintenance, CatHub safely closes the physical host session, keeps replies private to the maintenance face, rejects other transmission, then reopens the host session and restores foreground transient state. Close WKTools when maintenance is complete so it releases COM43. Do not grant `config_write` to the everyday N1MM face.

### Direct single-client connection

Connect the WinKeyer, close other applications that own its serial port, and add a safety-first configuration to the shared `config.toml`:

```toml
[cw_keying]
backend = "winkeyer"
winkeyer_port = "COM3"
winkeyer_baud = 1200
speed_wpm = 25
transmit_enabled = false
max_tx_ms = 120000
```

The equivalent environment variables are useful for temporary overrides:

```powershell
$env:QSORIPPER_CW_KEYER_BACKEND = "winkeyer"
$env:QSORIPPER_CW_WINKEYER_PORT = "COM3"
$env:QSORIPPER_CW_WINKEYER_BAUD = "1200"
$env:QSORIPPER_CW_SPEED_WPM = "25"
$env:QSORIPPER_CW_TRANSMIT_ENABLED = "false"
$env:QSORIPPER_CW_MAX_TX_MS = "120000"
```

On Linux, the port is typically `/dev/ttyUSB0` or `/dev/ttyACM0`. The account running the engine must have permission to open it.

Start the selected engine, then run:

```powershell
qsoripper cw status
```

A healthy response reports `winkeyer`, `Available: True`, the configured port, and a firmware revision. QsoRipper sends Host Open once and retains the serial connection across RPCs. It sends Host Close when the engine shuts down or abandons a failed session.

After confirming the port and connecting the keyer to a safe test load or disabling the transmitter, restart the engine with:

```powershell
$env:QSORIPPER_CW_TRANSMIT_ENABLED = "true"
```

Recheck `cw status`, set a conservative speed, send a short test, and verify abort:

```powershell
qsoripper cw speed 20
qsoripper cw send "TEST"
qsoripper cw abort
```

Environment configuration is read when the engine starts. Restart the engine after changing CW settings.

Persisted TOML is also read at engine startup. A `QSORIPPER_CW_*` environment variable overrides only its corresponding `[cw_keying]` value, so temporary changes do not require editing the shared file. Remove the environment variable to return to the persisted value on the next restart.

## Troubleshooting

| Symptom | Check |
|---|---|
| Backend is `null` | Set `QSORIPPER_CW_KEYER_BACKEND=winkeyer` or `cathub` and restart the engine. |
| Port is missing | Set `QSORIPPER_CW_WINKEYER_PORT` to the device's serial port. |
| Access denied or port busy | Close other logging/keyer software using the port and verify OS permissions. Only one process can own the WinKeyer session. |
| CatHub API unavailable | Start CatHub, verify `api_bind`, and confirm the engine endpoint includes the `http://` scheme. |
| N1MM cannot open its keyer | N1MM must open the application side of the virtual pair (for example COM41), never physical COM3 or CatHub's COM40 side. |
| Pot speed is offset | The raw protocol byte is relative to the active Speed Pot Setup minimum; use `cw status` for canonical WPM. |
| Host Open times out | Verify the port, cable, device power, and baud rate. Most WinKeyer devices use 1200 baud. |
| Send is rejected as disabled | Set `QSORIPPER_CW_TRANSMIT_ENABLED=true` only after completing the safe probe workflow, then restart. |
| Safety ceiling error | The keyer was still busy at `QSORIPPER_CW_MAX_TX_MS`; inspect the queued macro and keyer/radio state before retrying. |
| Wrong callsign or exchange | Verify the active station profile and pass the required `--his-call`, `--exchange`, or `--nr` context. |
| Invalid speed | Use an integer from 5 through 99 WPM. |

The WinKeyer backend never silently falls back to `null`. Serial failures appear as `UNAVAILABLE` RPC errors and in the next status response so hardware problems cannot look like successful transmission.
