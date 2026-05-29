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

Each pair has two COM numbers: a **daemon side** (the lower, even number COM10/20/30 that the
hub opens via `transport`) and an **application side** (the partner COM11/21/31). Point each
application at its application-side port. The daemon-side port is held open by the hub, so it
typically will **not** appear in an application's COM-port dropdown at all -- that is expected,
and the partner port (COM11/21/31) is the one to select.

## 3. Configure the daemon

The daemon settings live in the unified per-user `config.toml` shared with the engine and the
launcher, under a `[cat_hub]` table:

- Windows: `%APPDATA%\qsoripper\config.toml`
- Linux/macOS: `$XDG_CONFIG_HOME/qsoripper/config.toml` (or `~/.config/qsoripper/config.toml`)
- Override the location for every component with the `QSORIPPER_CONFIG_PATH` environment variable.

Settings nest under `[cat_hub]`, for example `[cat_hub.radio]`, `[cat_hub.poll]`,
`[cat_hub.ptt]`, `[cat_hub.events]`, `[[cat_hub.face]]`, and `[[cat_hub.hamlib_net]]`. The
engine and launcher own other top-level tables in the same file (`[station_profile]`,
`[launcher]`, `[rig_control]`, …); each component preserves the others' tables when it saves,
so the file is safe to share.

For a standalone setup you can still keep a separate file (the repo ships
`config\cathub.toml` with top-level `[radio]` … tables) and point the daemon at it with
`-Config`. Validate either layout without touching hardware:

    .\scripts\Start-CatHub.ps1 -DryRun

When `-Config` is omitted the script uses the unified `config.toml` if it contains a
`[cat_hub]` section, otherwise it falls back to the `config\cathub.toml` sample. The dry run
prints the resolved radio, poll, PTT, events, faces, and Hamlib NET endpoints. Adjust COM port
numbers and baud to match your com0com pairs and the TS-590's CAT baud, then re-run the dry run
until it is clean.

The `[radio].baud` value **must match the radio's own PC/CAT port speed** (TS-590 menu 62;
e.g. 57600). If they differ, the daemon opens COM4 but cannot talk to the radio. The
`[[face]].baud` values are nominal only -- com0com virtual pairs pass data regardless of baud,
so a client app can use any baud on its side of the pair.

## 4. Start the hub

    .\scripts\Start-CatHub.ps1

The daemon opens COM4, enables and owns the TS-590 `AI2;` native push stream, starts the
baseline poller (which backs off to heartbeat once push covers a field), opens each serial
face, and binds each Hamlib NET endpoint. Watch the log in another terminal:

    .\scripts\Get-CatHubLog.ps1 -Follow

Stop the hub with Ctrl+C in its window, or:

    .\scripts\Stop-CatHub.ps1

### Cold-start workflow (build + launch everything)

For a clean start after logon, build all artifacts and then launch the hub together with the
engines and UIs from one command:

    .\build.ps1                  # publishes qsoripper-cathub alongside the engines/UIs
    .\launcher.ps1 -WithCatHub   # starts the CAT hub first, then the launcher TUI

`-WithCatHub` brings the radio daemon up in its own window (reading the unified `config.toml`)
before the launcher starts engines and UIs, so everything connects to the hub's rigctld face.

## 5. Point each application at the hub

### HDSDR (via OmniRig)
- OmniRig: Rig type `Kenwood TS-2000`, port **COM11**, baud 115200, 8-N-1.
- The `hdsdr-omnirig` face is `dialect = "ts2000"`, `perms = ["read", "write"]`. The panadapter
  follows the radio, and click-to-tune on the waterfall sets the radio frequency/mode
  (`FA`/`FB`/`MD`). VFO-target writes (`FR`/`FT`) from the TS-2000 dialect are still rejected by
  design, so HDSDR can tune but can never oscillate the TS-590's A/B VFO selection.

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
- **Mode:** set the WSJT-X *Mode* selector to **None** and put the TS-590 in its data
  position (DATA-USB) on the front panel. The hub speaks the native Kenwood `MD`
  command set, which exposes SSB/CW/FSK/AM/FM but not the radio's DATA sub-mode, so a
  WSJT-X *Mode = Data/Pkt* setting would send `PKTUSB`, which the hub can only honor as
  plain `USB`; the read-back would then disagree with what WSJT-X asked for. *Mode =
  None* leaves the operator-selected data mode untouched and avoids that mismatch.
  *Mode = USB* also round-trips cleanly if you prefer WSJT-X to own the SSB mode.
- **Split Operation:** `Rig` or `Fake It` both work; the hub tracks split and TX-VFO
  state and never retargets VFO A/B on a poll.
- Frequencies are sent by Hamlib as a `%f` value (e.g. `14074000.000000`); the hub
  accepts both that decimal form and a plain integer, so `Test CAT` and band changes
  set the dial correctly.

### Log4OM
- CAT interface: Hamlib `NET rigctl`, host **127.0.0.1**, port **4534**.
- The `log4om` endpoint is `perms = ["read", "write"]`.

### QsoRipper engine (TUI and GUI)
- The engine's `RigctldProvider` points at the read-only endpoint **127.0.0.1:4532**.
- The TUI and GUI both consume the engine over gRPC; neither talks to the radio directly, so
  both get a consistent view fed by the same hub. TCP allows the engine and any other NET
  client to share an endpoint simultaneously.
- Keep `[rig_control].stale_threshold_ms` low (e.g. **200**) when reading through cathub. The hub
  serves reads from its in-memory state cache (kept current by the radio's native AI2 push), so a
  short freshness window is cheap and makes the GUI/TUI frequency display follow knob turns almost
  immediately. A large value such as 5000 makes the engine reuse a stale snapshot for that many
  milliseconds and the UI lags behind the radio by up to that interval.

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
- Daemon starts but never reads valid data from the radio (timeouts / stale): the `[radio].baud`
  does not match the radio's PC/CAT port speed. Check TS-590 menu 62 and set `[radio].baud` to
  the same value (e.g. 57600). A client app proves the rig's real baud quickly via a direct
  connection.
- Daemon starts, the port opens, but every poll times out at the *correct* baud: some radios
  (notably the Kenwood TS-590) only reply when the **RTS modem-control line is asserted**. The
  hub now asserts RTS and DTR automatically when it opens a serial radio port, so this should
  not occur with current builds. If you see it on an older build, update the daemon.
- An app's COM dropdown does not list the daemon-side port (COM10/20/30): expected. The hub
  holds that port open, so applications only see the partner port. Select COM11/21/31 instead.
- An app sees no data on its serial port: the com0com pair is reversed or the app is on the
  daemon's half of the pair. The app binds the **second** port of each pair (COM11/21/31).
- A NET client cannot connect: confirm the bind address/port in `config\cathub.toml` matches
  the app, and that the hub log shows the endpoint listening.
- An app that relies on Kenwood auto-information (notably **ARCP-590**) connects but never
  tracks the dial / shows "BUSY": such apps poll `AI;` as a keepalive and depend entirely on
  the radio pushing `FA;`/`IF;` frames. The hub virtualizes auto-info per connection — an `AI;`
  read reports the face's current state (`AI0;`/`AI2;`) without disabling it, and the hub fans
  out native-push frames to any face that has enabled `AI2;`. This works in current builds; if
  an older build froze ARCP-590 on connect, update the daemon.
- Set `CATHUB_LOG=debug` before starting for verbose tracing. Use
  `CATHUB_LOG=qsoripper_cathub::serial_face=trace` to see each face's request/reply/notify
  frames, which is the fastest way to diagnose a client handshake.

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
- **Data sub-modes (PKTUSB/PKTLSB) are not mapped to the TS-590 DATA mode.** The hub
  models the native Kenwood `MD` mode set (LSB/USB/CW/CWR/FSK/FSKR/AM/FM). A NET client
  that requests `PKTUSB`/`PKTLSB` (e.g. WSJT-X with *Mode = Data/Pkt*) is accepted but
  honored as plain `USB`/`LSB`, so a mode read-back will not echo the data variant.
  Configure such clients with *Mode = None* (operator selects DATA on the radio) or a
  plain SSB mode. See the WSJT-X notes in section 5.

