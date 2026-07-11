# CatHub multi-client WinKeyer broker

## Decision

CatHub is the sole owner of the physical WinKeyer serial port. QsoRipper engines use a typed loopback gRPC API. Each unmodified legacy program receives a dedicated virtual WinKeyer serial face. The keyer subsystem is independent of the radio CAT actor, but both use the station PTT ownership manager.

```text
physical WinKeyer <-- 8-N-2 --> CatHub WinKeyer actor
                                      |-- typed loopback API --> Rust/.NET engine
                                      |-- virtual COM face ----> N1MM
                                      `-- virtual COM face ----> maintenance tool
```

## Protocol and session model

One incremental parser consumes every WinKeyer command from a virtual face. It retains partial fixed and variable-length commands across reads, bounds the largest command to the 258-byte EEPROM load frame, and emits ordinary Morse data separately. Host Open, Host Close, firmware revision, status requests, and speed-pot requests are virtualized per client. A virtual close never closes the physical session during routine operation.

The parser includes one narrowly scoped N1MM Logger+ compatibility rule. N1MM may emit `00 1C <wpm>` when beginning a message, inserting an extra Admin prefix before the standard buffered-speed command. CatHub normalizes that captured wire sequence to `1C <wpm>`. Other invalid or disruptive Admin commands remain subject to the normal fail-closed maintenance policy.

Device bytes are classified by the WinKeyer tag bits as status, speed-pot, or echo events. Status and pot events are fanned out. Echo bytes are visible only to the active stream owner, or to the primary face during physical paddle break-in. Maintenance response bytes are private to the maintenance owner and never enter typed event streams.

Virtual sessions keep independent WK1/WK2/WK3 modes so one client's pushbutton/status choice does not alter another client's status format.

## Scheduling and transient state

Typed sends are atomic jobs. A virtual face receives a raw stream lease when its first data reaches the scheduler; more data from that face appends to the same active stream. Jobs use one global arrival-order FIFO, which preserves each client's FIFO and prevents starvation by later submissions. No client bytes or per-client commands are interleaved inside another job.

The scheduler stores each client's speed and transient register profile. Before a job it applies that profile and the requested speed. After the queue drains it restores the primary face's profile and fixed or pot-controlled speed. Physical paddles retain the keyer's native priority and can break into machine-sent Morse.

A speed-pot byte carries `actual WPM - MIN_WPM`. The broker tracks the active Speed Pot Setup minimum, retains the raw offset for protocol-compatible faces, and publishes actual WPM through the typed status/event contract.

## Abort and safety rules

- A queued cancel removes only jobs owned by that client.
- Clear Buffer is accepted only from the active stream owner or primary idle controller.
- An active-client disconnect clears the physical buffer, forces key-up, cancels that client's job, and records the safety action.
- The broker owns one maximum-transmit watchdog across all clients.
- CAT PTT and WinKeyer transmit jobs acquire the same station lease. A conflicting owner receives a failed-precondition response.
- USB/read/write failure cancels queued work, releases station PTT, marks status disconnected, and reopens the physical port with bounded backoff. The radio hub and client API stay alive.
- Graceful shutdown clears the buffer, forces key-up, sends physical Host Close, and releases station PTT.

## Maintenance

Reset, calibration, EEPROM dump/load, firmware update, high-baud switching, and other disruptive administrative commands require `config_write`. `config_write` itself requires `status` and `control`. A lease is granted only with no active or queued transmission.

On acquisition, CatHub clears/dekeys and closes the physical host session before forwarding the administrative command, as required by the WinKeyer protocol. Other sends receive deterministic busy errors. Replies route only to the owner. On virtual Host Close or client loss, CatHub sends physical Host Open, waits for the firmware byte, reapplies safe initialization and the foreground transient profile, then resumes normal scheduling.

Routine N1MM and QsoRipper operation never sends EEPROM writes. The normal N1MM face should not receive `config_write`.

## Configuration

Unified configuration uses `[cat_hub.winkeyer]` and `[[cat_hub.winkeyer_face]]`; standalone CatHub files omit the `cat_hub.` prefix. The API must bind to loopback. Physical and virtual transports must be distinct, only one face may be primary, and dependent permission combinations are validated by CatHub and both setup engines.

See [CW keying setup](../integrations/cw-keying.md) for the complete operator workflow.
