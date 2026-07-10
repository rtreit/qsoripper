# CW keying setup

QsoRipper exposes the same engine-backed CW macro and keyer behavior through the Rust and .NET engines. The default `null` backend expands and accepts macros without touching radio hardware. Use it to verify station context and contest exchanges before enabling a WinKeyer.

## Safety model

Hardware text transmission requires both of these settings:

```text
QSORIPPER_CW_KEYER_BACKEND=winkeyer
QSORIPPER_CW_TRANSMIT_ENABLED=true
```

Selecting `winkeyer` alone does not authorize transmission. With the transmit gate off, status and speed commands can probe the device, but `cw send` and `cw macro` fail with `FAILED_PRECONDITION` before opening the hardware for transmission.

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

Connect the WinKeyer, close other applications that own its serial port, and start with transmission disabled:

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

## Troubleshooting

| Symptom | Check |
|---|---|
| Backend is `null` | Set `QSORIPPER_CW_KEYER_BACKEND=winkeyer` and restart the engine. |
| Port is missing | Set `QSORIPPER_CW_WINKEYER_PORT` to the device's serial port. |
| Access denied or port busy | Close other logging/keyer software using the port and verify OS permissions. Only one process can own the WinKeyer session. |
| Host Open times out | Verify the port, cable, device power, and baud rate. Most WinKeyer devices use 1200 baud. |
| Send is rejected as disabled | Set `QSORIPPER_CW_TRANSMIT_ENABLED=true` only after completing the safe probe workflow, then restart. |
| Safety ceiling error | The keyer was still busy at `QSORIPPER_CW_MAX_TX_MS`; inspect the queued macro and keyer/radio state before retrying. |
| Wrong callsign or exchange | Verify the active station profile and pass the required `--his-call`, `--exchange`, or `--nr` context. |
| Invalid speed | Use an integer from 5 through 99 WPM. |

The WinKeyer backend never silently falls back to `null`. Serial failures appear as `UNAVAILABLE` RPC errors and in the next status response so hardware problems cannot look like successful transmission.
