# cathub live TS-590 test runbook

These tests exercise `qsoripper-cathub` against a real Kenwood TS-590. They are for
hardware discovery and regression hunting; they are ignored by default and never run in CI.

The deterministic unit and integration tests remain the merge gate. When a live-radio test
finds a bug, capture the observed frame/state behavior and add a hardware-free regression
before changing production code.

## Default station assumptions

- Radio: Kenwood TS-590
- CAT port: `COM3`
- CAT baud: `115200`
- Hub backend: native `ts590`
- Test Hamlib endpoint: temporary loopback TCP ports allocated by the test

Both the port and baud can be overridden with environment variables or script parameters.

## Safety rules

- Stop every other process that might own the radio CAT port before running the tests.
- Do not run these tests while operating on the air unless you are intentionally validating
  receive-only CAT behavior.
- The current live suite does not key PTT. Future transmit/PTT tests must remain behind a
  separate explicit transmit flag.
- Use `--nocapture` so operator prompts and diagnostic output are visible.

## Quick run

From the repository root:

```powershell
.\scripts\Test-CatHubLiveTs590.ps1
```

For the operator-assisted VFO matrix:

```powershell
.\scripts\Test-CatHubLiveTs590.ps1 -Interactive
```

Override the radio port or baud if needed:

```powershell
.\scripts\Test-CatHubLiveTs590.ps1 -Port COM7 -Baud 57600
```

## Direct cargo command

```powershell
$env:QSORIPPER_CATHUB_LIVE_TS590 = '1'
$env:QSORIPPER_CATHUB_LIVE_PORT = 'COM3'
$env:QSORIPPER_CATHUB_LIVE_BAUD = '115200'
cargo test --manifest-path src\rust\Cargo.toml -p qsoripper-cathub --test live_ts590 -- --ignored --nocapture
```

To include the operator-assisted scenario:

```powershell
$env:QSORIPPER_CATHUB_LIVE_INTERACTIVE = '1'
cargo test --manifest-path src\rust\Cargo.toml -p qsoripper-cathub --test live_ts590 -- --ignored --nocapture
```

## What the tests cover

`live_ts590_startup_snapshot_is_coherent`

- Starts the built `qsoripper-cathub` binary against the real TS-590.
- Waits for the Hamlib endpoint to become reachable.
- Reads `v`, `f`, `m`, and `\get_vfo_info <active VFO>`.
- Asserts the active snapshot is coherent: `f` and `m` match the active VFO info block.

`live_ts590_manual_vfo_switch_matrix`

- Requires `QSORIPPER_CATHUB_LIVE_INTERACTIVE=1`.
- Prompts the operator to set VFO A and VFO B to intentionally different frequencies and modes.
- Confirms selecting VFO A makes Hamlib `v`, `f`, and `m` report VFO A state.
- Prompts the operator to switch to VFO B.
- Confirms Hamlib `v`, `f`, and `m` report VFO B state, not stale VFO A state.
- Prompts the operator to switch back to VFO A and verifies the original A state returns.

## When a live test fails

1. Save the test output and cathub log.
2. Identify the smallest radio-frame or Hamlib transcript that demonstrates the bug.
3. Add a deterministic test in `src\rust\qsoripper-cathub\src\...` or a normal integration
   test that reproduces the transcript without hardware.
4. Confirm the deterministic test fails.
5. Fix production code.
6. Re-run the normal cathub test suite and the live test.

This keeps hardware tests useful for discovery while ensuring regressions are caught by CI.
