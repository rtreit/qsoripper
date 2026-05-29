# qsoripper-cathub operator setup

`qsoripper-cathub` is a single daemon that owns the radio's CAT serial port and fans it out
to every application over that application's native protocol. Because only the daemon talks
to the radio, the classic multi-app failures disappear:

- no VFO A/B oscillation (baseline polling never emits VFO-select/retarget commands),
- no frequency drift (one serialized writer, ordered writes, native-push reconciliation),
- no PTT contention (single-owner PTT lease with a hard transmit-time ceiling),
- no auto-info stomping (each app's auto-information is virtualized per connection).

This runbook brings up the hub for six applications sharing one Kenwood TS-590:
HDSDR (via OmniRig), QsoRipper TUI and GUI, ARCP-590, N1MM Logger+, WSJT-X, and Log4OM.

See `docs/design/cathub-multi-client-cat-hub.md` for the architecture and behavior contracts.

## 1. Retire the legacy chain

The old stack was rigctld + a Python safe-bridge + rigctlcom. Stop all of it before starting
the hub. Only one process may own the radio's COM port (COM4 on this station — the
Silicon Labs CP210x USB-UART bridge that fronts the TS-590's USB CAT port).

    Get-Process rigctld, rigctlcom -ErrorAction SilentlyContinue | Stop-Process
    # also stop any safe-bridge Python process and any app still bound directly to COM4

Confirm nothing else holds COM4 before continuing.

## 2. Create virtual serial pairs (com0com)

Serial clients connect through a virtual null-modem pair. The daemon binds the first port of
each pair; the application binds the second. Using the com0com "setupc" tool, create:

    install PortName=COM10 PortName=COM11    # HDSDR / OmniRig  (daemon COM10, app COM11)
    install PortName=COM20 PortName=COM21    # N1MM Logger+     (daemon COM20, app COM21)
    install PortName=COM30 PortName=COM31    # ARCP-590         (daemon COM30, app COM31)

WSJT-X, Log4OM, and the QsoRipper engine use the Hamlib NET (TCP) endpoints instead and need
no serial pair.

## 3. Configure the daemon

The station config is `config\cathub.toml`. Validate it without touching hardware:

    .\scripts\Start-CatHub.ps1 -DryRun

This prints the resolved radio, poll, PTT, events, faces, and Hamlib NET endpoints. Adjust
COM port numbers and baud to match your com0com pairs and the TS-590's CAT baud, then re-run
the dry run until it is clean.

## 4. Start the hub

    .\scripts\Start-CatHub.ps1

The daemon opens COM4, enables and owns the TS-590 `AI2;` native push stream, starts the
baseline poller (which backs off to heartbeat once push covers a field), opens each serial
face, and binds each Hamlib NET endpoint. Watch the log in another terminal:

    .\scripts\Get-CatHubLog.ps1 -Follow

Stop the hub with Ctrl+C in its window, or:

    .\scripts\Stop-CatHub.ps1

## 5. Point each application at the hub

### HDSDR (via OmniRig)
- OmniRig: Rig type `Kenwood TS-2000`, port **COM11**, baud 115200, 8-N-1.
- The `hdsdr-omnirig` face is `dialect = "ts2000"`, `perms = ["read"]`. The panadapter
  follows the radio; VFO-target writes from the TS-2000 dialect are rejected by design, so
  HDSDR can never oscillate the TS-590's VFO.

### N1MM Logger+
- Configurer > Hardware: radio `Kenwood`, port **COM21**, 115200, 8-N-1, no flow control.
- The `n1mm` face is `dialect = "ts590"`, `perms = ["read", "write", "ptt"]`.

### ARCP-590
- Set ARCP-590's COM port to **COM31**, 115200, 8-N-1.
- The `arcp590` face is `dialect = "ts590"`, `perms = ["read", "write", "ptt", "config_write"]`
  so the full faceplate (including EX-menu writes) works.

### WSJT-X
- Settings > Radio: Rig `Hamlib NET rigctl`, Network Server **127.0.0.1:4533**.
- PTT method `CAT`. The `wsjtx` endpoint is `perms = ["read", "write", "ptt"]`.

### Log4OM
- CAT interface: Hamlib `NET rigctl`, host **127.0.0.1**, port **4534**.
- The `log4om` endpoint is `perms = ["read", "write"]`.

### QsoRipper engine (TUI and GUI)
- The engine's `RigctldProvider` points at the read-only endpoint **127.0.0.1:4532**.
- The TUI and GUI both consume the engine over gRPC; neither talks to the radio directly, so
  both get a consistent view fed by the same hub. TCP allows the engine and any other NET
  client to share an endpoint simultaneously.

## 6. Verify (bench)

With the hub running and all six apps connected:

1. Turn the physical VFO knob. Every app's displayed frequency tracks it within a poll/push
   cycle, and the cathub log shows `NativePush` (not `PollDiff`) events once `AI2;` is active.
2. Change band in N1MM. HDSDR's waterfall recenters without HDSDR ever talking to N1MM, and
   the TS-590 never bounces between VFO A and B.
3. Set frequency from WSJT-X. N1MM, Log4OM, ARCP-590, and the engine all converge.
4. Key WSJT-X (Tune). The PTT lease is granted; while held, an attempt to key from N1MM is
   rejected (Hamlib `RPRT` busy) and logged. Releasing WSJT-X frees the lease.
5. Leave one over running longer than expected: confirm a transmit never exceeds
   `ptt_max_tx_ms` (the safety ceiling force-releases and logs loudly).

Live transmit verification requires the operator and real hardware; do not key the
transmitter from automation. Watch `Get-CatHubLog.ps1 -Follow` throughout.

## 7. Troubleshooting

- "Access denied" / port busy on COM4: the legacy chain or another app still owns the radio
  port (step 1).
- An app sees no data on its serial port: the com0com pair is reversed or the app is on the
  daemon's half of the pair. The app binds the **second** port of each pair (COM11/21/31).
- A NET client cannot connect: confirm the bind address/port in `config\cathub.toml` matches
  the app, and that the hub log shows the endpoint listening.
- Set `CATHUB_LOG=debug` before starting for verbose tracing.

## 8. Known v1 limitations

- **No automatic radio reconnect yet.** If the radio transport drops mid-session (USB
  unplugged, radio powered off), the daemon does not yet retry the serial link or serve a
  `stale` flag (design §8.7). Restart the hub after restoring the radio. Client faces and
  NET endpoints are unaffected by this and stay up.
- **Hamlib NET bind errors surface in the log, not at startup.** A serial face that fails to
  open aborts startup with a clear error, but a `[[hamlib_net]]` endpoint whose bind address
  is already in use logs the error from its listener task rather than failing the whole
  daemon. If a NET client cannot connect, check the log for that endpoint.
- On shutdown (Ctrl+C) the daemon makes a best-effort `RX;` to unkey the transmitter. A hard
  crash cannot run that path; the `ptt_max_tx_ms` ceiling and the radio's own TX timeout are
  the ultimate stuck-transmitter backstops.

