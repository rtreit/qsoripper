# CW Decoder Experiment

This folder contains experiments that improve QsoRipper CW decoding for real off-air audio.

The project has two goals:

1. Keep a **simple append-only event-stream foundation** that agrees with the audio and visual output.
2. Add signal-processing experiments without a regression in that foundation.

Rolling transcript windows, overlap stitching, and commit heuristics did **not** give the best live behavior.

The best path consumes the stable dit/dah/gap event stream that paints the Visualizer bars. It appends each mature event once in audio order.

This path is the reference baseline. Decode, Labeling, Tuning, Bench, and Visualizer use it first. Tests compare experimental decoders with it.

> **Foundational baseline (May 2026): region-isolated streaming transcript.**
>
> A full QEX-style technical article about this baseline is available at
> [`docs/cw-decoder-architecture.html`](docs/cw-decoder-architecture.html).
> The article describes the architecture, design reasons, results, and future work.
> Open it in a browser. It is the primary reference for this architecture.
>
> The append-only event stream drives the visualizer. It supplies the waveform, lock state, pitch, WPM, and classified events.
>
> Real QSO audio can contain several bursts at different speeds. Background static can separate operator pauses, TX/RX cycles, and different QSO types.
>
> The transcript path detects each burst as a region. It decodes each region with an independent pitch, WPM, and k-means lock.
>
> It appends the region text after the text is stable for a trailing-static window.
>
> The reference implementation is in `src\region_streamer.rs` (`RegionStreamer`) and `src\region_stream.rs` (`decode_region_stream`).
> Three interfaces expose it:
>
> - **Batch mode:** `cw-decoder stream-region --file <path>` detects regions in the complete file.
>   It emits one transcript for each region. This output is the reference output.
> - **Live streaming:** `cw-decoder stream-live-v3 --region-transcript` runs `RegionStreamer` with the V3 envelope streamer.
>   The envelope path continues to supply visualizer events.
>   Region decode supplies only the cumulative `transcript` field on `text`, `appended`, and `end` events.
>   `RegionStreamer::trim_committed` limits memory use in long sessions.
>   `RegionStreamer::reset` processes stdin-control `ResetLock` requests without a process restart.
> - **User interfaces:** Both interfaces use `--region-transcript` by default:
>   - Production logger GUI (F7 live microphone):
>     `src\dotnet\QsoRipper.Gui\Services\CwDecoderProcessSampleSource.cs`
>   - Experimental CW visualizer GUI (live microphone and file replay):
>     `experiments\cw-decoder\gui\Services\CwDecoderProcess.cs`
>
> **Reference success metric.** This baseline gives a 100% exact-match transcript for this real multi-burst sample:
> `cw-samples\training-set-b\radio-20260502-105714.mp3`.
>
> The truth file is `radio-20260502-105714.truth.txt`.
>
> ```text
> IHU NVCHU 7QP W7N 7QP W7N
> ```
>
> Both `stream-region` and `stream-live-v3 --region-transcript` give that exact text with `RegionStreamerConfig::default()`.
> The configuration is `merge_gap=0.5s`, `min_region=0.3s`, `threshold_factor=0.30`, `pad=0.15s`, `min_tonal_prominence_ratio=8.0`, and `stable_latency=0.6s`.
> The command also uses `--decode-every-ms 250`.
>
> `region_streamer.rs` includes deterministic synthetic impairment tests:
>
> - a clean four-burst copy
> - quiet static gaps
> - noise without ghost text
> - moderate white noise, QSB, and offset QRM for `CQ TEST KC7AVA 73`
>
> `synthetic_qso.rs` includes a regression test for an isolated final over marker.
> Previously, the decoder calibrated `K` (`-.-`) as `S` (`...`) in a short independent region.
>
> For broader QSO-debug traffic, use the synthetic suite generator:
>
> ```powershell
> cargo run --manifest-path experiments\cw-decoder\Cargo.toml --bin cw-decoder -- `
>   gen-qso-suite --output .artifacts\cw-qso-suite --ragchew 6 --contest 6
> ```
>
> The generator creates deterministic ragchew and contest WAV files.
> It also creates `.truth.txt` sidecars and `manifest.ndjson`.
> It then validates each sample with the region decoder.
>
> Use `--require-exact` to get a nonzero exit code for a transcript mismatch.
> Do not use it for exploratory tests that intentionally find failures.
>
> **Multi-pitch burst discovery (June 2026).** `region_stream::discover_burst_pitches` now uses both windowed and mean Goertzel sweeps.
>
> A whole-buffer mean can merge long ragchew turns at different pitches into one false peak.
> The windowed source finds each turn independently. It uses a 10-second window, a 5-second hop, and a noise-floor confidence gate.
>
> The mean source continues to support contest audio with dense pitch groups.
> The combined synthetic suite now gives 12/12 exact copies. The earlier result was 6/12.
>
> Tests covered `decode_region_stream` and `RegionStreamer` at 12 kHz, 16 kHz, and 48 kHz.
>
> **Future experiments must not regress this baseline.**
> Run this cross-validation before you merge a related change.
>
> This requirement applies to `region_streamer.rs`, `region_stream.rs`, `stream-live-v3`, GUI launchers, and decoder primitives.
>
> ```powershell
> cd C:\Users\randy\Git\qsoripper
> $exe  = "experiments\cw-decoder\target\release\cw-decoder.exe"
> $f    = "C:\Users\randy\OneDrive\Documents\Home\Hobbies\Ham Radio\QSORipper\QSO Audio Samples\cw-samples\training-set-b\radio-20260502-105714.mp3"
> $truth = (Get-Content "C:\Users\randy\OneDrive\Documents\Home\Hobbies\Ham Radio\QSORipper\QSO Audio Samples\cw-samples\training-set-b\radio-20260502-105714.truth.txt" -Raw).Trim()
> & $exe stream-region --file $f --json --no-realtime --decode-every-ms 250 |
>     Select-String '"type":"end"' | Select-Object -Last 1
> & $exe stream-live-v3 --file $f --json --region-transcript --decode-every-ms 250 |
>     Select-String '"type":"end"' | Select-Object -Last 1
> # Both `transcript` fields must equal $truth.
> cargo test --manifest-path experiments\cw-decoder\Cargo.toml region_streamer::tests::synthetic
> ```
>
> For rollback, use the commits from PR #375 and PR #376.
> PR #375 added the region-based batch decoder. PR #376 added live streaming and GUI integration.
>
> The reference branch is `u/randy/region-based-streaming-cw`.
>
> The append-only event stream in `append_decode.rs` remains the source for visualizer bars.
> It also supplies the `cw_decode_rx_wpm` aggregator for the production logger.
> Region-isolated decode supplies only transcript text.

> **Integration status (round 1, issue #321).** The production GUI runs `cw-decoder` as a subprocess.
> It automatically sets `QsoRecord.cw_decode_rx_wpm` on logged CW QSOs.
> The value is a time-weighted mean for the QSO start and end window.
> The ADIF field is `APP_QSORIPPER_RX_WPM`.
>
> See `src\dotnet\QsoRipper.Gui\Services\CwDecoderProcessSampleSource.cs` and `src\dotnet\QsoRipper.Gui\Services\CwQsoWpmAggregator.cs`.
> Also see **Settings > Display > Monitor radio** in the main window.
>
> Round 2 will put the decoder behind an engine-side `CwDecodeService`.
> All clients will then use the same stream.
>
> **Radio monitor prerequisites:**
>
> 1. Go to `experiments\cw-decoder\`.
> 2. Run `cargo build --release`.
> 3. Keep the binary at `experiments\cw-decoder\target\release\cw-decoder.exe`.
>    Alternatively, set `CW_DECODER_EXE` to an absolute path.
> 4. Permit the application to use a capture device.
> 5. Open **Settings > Display**.
> 6. Select *Enable radio monitor (auto-fills CW WPM on logged QSOs)*.
> 7. Select a capture device.
> 8. Save the settings.
>
> This folder is an independent Cargo workspace. The main `cargo build` in `src\rust\` does not build it.
>
> The GUI searches parent directories for the default binary.
> The decoder uses the default operating-system capture device unless you select another device.
>
> Physical inputs have normal device names.
> WASAPI loopback outputs have the suffix `(system output / loopback)`.
> Use a loopback output to test with audio from the speakers.
>
> Select *Show CW WPM in the status bar* to see the live WPM value.
> Use `Ctrl+Shift+W` to change this option.
> The value becomes dim when the monitor is off.
>
> Use `Ctrl+Alt+W` if the decoder does not follow a large speed change.
> This command restarts the decoder and resets its confidence state.
>
> If the GUI cannot find the binary, it clears the monitor option.
> The status row shows `CW WPM: decoder not built`.
>
> If launch fails, the status row shows the error and clears the option.
> For example, cpal can fail to open the capture device.
>
> **Validate the GUI without a radio:**
>
> 1. Build the decoder in `experiments\cw-decoder` with `cargo build --release`.
> 2. Open **Settings > Display > Monitor radio**.
> 3. Enable the monitor.
> 4. Select a `(system output / loopback)` capture device.
>    For example, select *Speakers (Realtek) (system output / loopback)*.
> 5. Play a CW practice clip through the speakers.
> 6. Use `Ctrl+Shift+W` to show the live WPM value.
> 7. Log a CW QSO during playback.
> 8. Confirm that the recent-QSO row contains `cw_decode_rx_wpm`.
>
> The GUI detects WASAPI loopback devices automatically.
>
> For a cross-platform option, install VB-Audio Cable or equivalent software.
> Route system output to the cable. Select the cable input in the capture-device list.
>
> Run `experiments\cw-decoder\target\release\cw-decoder.exe devices` to list candidate devices.
> Add `--json` for machine-readable output.
> The output includes input devices and output devices that support loopback.

## Current architecture

### Core binaries

#### `cw-decoder`

Main experiment executable. It currently exposes several surfaces:

- **Offline decode**
  - `file` - single-pass or sliding-window whole-file decode through `ditdah`
- **Live capture**
  - `devices` - list available CPAL input devices (add `--json` for machine-readable output that includes both inputs and loopback-capable outputs)
  - `live` - TUI-driven capture + rolling-window `ditdah` decode (legacy interactive surface)
- **Custom streaming decoder**
  - `stream-file` - file-driven streaming decode with optional NDJSON event output and live `--stdin-control` config updates
  - `stream-live` - live capture through the streaming Goertzel decoder, with optional `--record` WAV mirror and `--stdin-control`
  - `stream-live-v2` - whole-growing-buffer `ditdah` replay.
    This reference showed that complete-buffer decode was better than sliding-window transcript stitching.
    It is not the GUI default, but it remains useful for A/B tests.
  - `stream-live-v3` - **current GUI foundation.**
    This in-house envelope decoder drives Decode, Labeling, Tuning, Bench, and the **VISUALIZER** tab.
    Its stages are Goertzel envelope, percentile floors, hysteresis, and k-means dot/dah classification.
    It emits NDJSON `viz` frames for these values:
    - envelope curve
    - noise and signal floors
    - hysteresis bands
    - classified events
    - on-duration histogram
    - k-means centroids
    - current and locked WPM
    - SNR
    The operator can use these values to examine decoder operation.
    The append-only decoder in `src\append_decode.rs` supplies the transcript.
    It consumes each mature `on_dit`, `on_dah`, `off_char`, and `off_word` bar once.
    It appends the bars to a raw Morse stream in sample-time order.
    The raw stream uses `.`, `-`, `/`, and `//`.

    Decode produces one growing text line with spaces for word gaps.
    `--pin-wpm` fixes the initial `locked_wpm` to the operator value.
    `--pin-hz` bypasses automatic pitch detection.
    Use it when the detector selects noise or a harmonic.
    `--min-snr-db` has a default value of 6.0.

    Below this value, the decoder emits visualizer frames but suppresses text.
    This control stops noise-locked dit spam from a high-tone harmonic.
    A dynamic-range bimodality gate also rejects high-variance noise.
    Its default expression is `(signal_floor - noise_floor) / envelope_max >= 0.55`.
    When either gate rejects text, the visualizer shows a red `LOW SNR` badge.

    Live captures go to `experiments\cw-decoder\captures\viz-yyyyMMdd-HHmmss.wav`.
    Use these captures for later labeling.
    During file replay, `stream-live-v3 --file --play` uses the output playback cursor.
    Thus, the bars and transcript stay synchronized with the audio.
- **Causal ditdah baseline**
  - `stream-file-ditdah` - file-driven causal whole-window `ditdah` replay
  - `stream-live-ditdah` - live capture through the rolling-window causal baseline, with an optional `--record` WAV mirror.
    **Deprecated.** Use it only for A/B tests. The GUI now uses the `stream-live-v3` append foundation.
- **Labeling helpers**
  - `harvest-file` - find candidate "golden copy" windows by intersecting offline `ditdah` and the streaming decoder, optional `--needle` anchors
  - `preview-window` - render a slowed WAV preview of a window for human verification
  - `profile-window` - emit a tone-energy profile for the labeling UI's signal-profile editor
- **Playback helper**
  - `play-file` - play an audio file through the default output device and emit JSON progress for the GUI's inline transport
- **Tone diagnostics**
  - `probe-fisher` - sweep candidate pitches across an audio file and rank them by trial-decode Fisher score
- **Cold-start + lock-stability benchmark**
  - `bench-latency` - runs a deterministic synthetic matrix or a real recording through the streaming decoder.
    The synthetic matrix has silence, noise, voice lead-ins, and long clean CW.
    For a real recording, use `--from-file --truth --cw-onset-ms`.
    The command reports cold-start acquisition latency and lock stability.
    Acquisition latency ends at the first stable, correct run of N characters.
    Lock metrics include uptime ratio, `PitchLost` count, relock cycles, and the longest unlocked gap.
    The main metric is `lat_ms = t_stable_N - cw_onset_ms`.

    Add `--foundation` to score the GUI append-only transcript path.
    This mode reports transcript quality for regression tests. Its latency-specific lock metrics are empty.

All `--json` and `--record` flags are what the Avalonia GUI uses to drive the engine over stdout/stderr NDJSON.

### `eval`

Corpus scorer and sweep harness (`src\bin\eval.rs`).

Current uses:

- exact-window scoring against saved `*.labels.jsonl`
- full-stream scoring by replaying whole recordings causally and intersecting transcript state at label boundaries
- foundation strategy scoring (`--strategy-sweep --strategies foundation`) so tuning can compare experimental modes against the same append-only path the GUI uses
- fast parameter sweeps for the causal `ditdah` baseline (`--sweep-ditdah`, optionally `--wide-sweep`)
- a built-in synthetic regression suite (silence, white/bursty/colored noise, clean and noisy synthesized CW at multiple SNRs) when no label flags are supplied

### `validate-corpus` (region path, end-to-end)

This command finds `*.truth.txt` sidecars in a directory.
It pairs each sidecar with its audio file.
It prefers `.wav`, then `.mp3`, `.m4a`, or `.flac`.
It sends each entry through the production region decoder (`region_stream`).
It prints pass status, character error rate, and ghost-character count for each entry.
It also prints a summary.

The command connects the PR #381 Visualizer "Save Truth" workflow to the batch decode core.
`RegionStreamer` also uses this decode core for live commits.

```powershell
# Validate every operator-saved truth pair under your local corpus root.
# Audio files are gitignored (.wav/.mp3), so this runs against a path
# outside the repo (typically OneDrive). Entries with a missing audio
# file are reported as SKIPPED rather than failing.
cargo run --release --manifest-path experiments\cw-decoder\Cargo.toml `
    --bin cw-decoder -- validate-corpus `
    --dir "C:\path\to\cw-samples"

# CI-style exit code: non-zero if any entry mismatches or errors
# (skipped entries do not count as failures).
cargo run --release --manifest-path experiments\cw-decoder\Cargo.toml `
    --bin cw-decoder -- validate-corpus `
    --dir "C:\path\to\cw-samples" --require-exact

# Machine-readable NDJSON: one JSON object per entry plus a final
# "summary" row. Pipes cleanly into `jq` / dashboards.
cargo run --release --manifest-path experiments\cw-decoder\Cargo.toml `
    --bin cw-decoder -- validate-corpus `
    --dir "C:\path\to\cw-samples" --json
```

The relative path is the namespace for a subdirectory entry.
For example, an entry can use `training-set-a/cw_30wpm_abbrev`.
Thus, duplicate basenames in different folders keep distinct IDs in the output table and JSON stream.
Pass
`--no-recursive` to limit the walk to the top-level directory.

### Stress-test harness (`scripts\stress-gen.ps1` + `scripts\stress-eval.ps1`)

The scripts use `ffmpeg` to generate a deterministic matrix of stressed copies from a clean WAV file.
They run cw-decoder on each variant. They write a degradation summary and a CSV file.

Use the harness to find regressions in acquisition or decode logic.
It also measures the lowest SNR at which the decoder can find and decode the known signal.

The matrix currently covers (per baseline):

- **clean** passthrough at the decoder's native 12 kHz mono s16
- **attenuation ladder**: -6, -12, -18, -24, -30 dB (no added noise)
- **white noise** SNR ladder: 20, 10, 6, 3, 0 dB
- **pink noise** SNR ladder: 20, 10, 6, 3, 0 dB (closer to band hiss)
- **brown / red noise**: 10, 6, 3 dB (atmospheric / QRN-like)
- **narrow IF**: 250-1100 Hz bandpass (simulates a narrow CW filter)
- **QRM**: steady carrier at 850 Hz mixed at -16 dB
- **combined weak-signal presets**: `weak_pink_snr6` (-18 dB signal + pink @6 dB SNR), `weak_pink_snr3` (-24 dB + pink @3 dB SNR)

Generate and score:

```powershell
# Produces 23 .wav variants in data\cw-stress\30wpm\ (gitignored).
.\experiments\cw-decoder\scripts\stress-gen.ps1 `
    -InputWav   data\cw-samples\cw_30wpm_youtube_70s_2min_12k.wav `
    -OutputDir  data\cw-stress\30wpm

# Decodes every variant with the current decoder (default purity 3.0) and
# prints colored per-variant {pitch, WPM, char count, transcript preview}
# plus a stress-results.csv next to the inputs. Add -Truth "..." to also
# get CER vs an operator-supplied ground-truth string.
.\experiments\cw-decoder\scripts\stress-eval.ps1 -StressDir data\cw-stress\30wpm
```

Current observed behavior on the 30 WPM youtube baseline (sender at ~30 WPM):

- The decoder reports **29.5 WPM** for `clean`.
- It stays between 28 WPM and 30 WPM through the complete attenuation ladder, including -30 dB.
- It stays in that range with brown, pink, or white noise at SNR values of 6 dB or more.
- It also stays in that range with the narrow IF and QRM 250 Hz from the CW pitch.
- Pitch lock moves to a side bin near 574 Hz for `pink_snr6`, `white_snr6`, and `weak_pink_snr3`.
- Top-K candidate tracking and oracle-tone evaluation can help these cases.

Stress audio is large and reproducible from the script, so `data/cw-stress/` is gitignored. Commit only the script changes and any operator-curated `TRUTH.txt` files.

## Decoder families

### 1. Custom streaming decoder

Implemented in `src\streaming.rs`.

Current shape:

- live/file audio is resampled to 12 kHz
- band-limited
- pitch is selected from candidate tones
- Goertzel power is tracked at the chosen tone
- adaptive thresholding + SNR gating produce key-up/key-down state
- on/off durations are classified into dits, dahs, letters, and words

Recent custom-streaming changes on this branch added:

- **keying-aware pitch picking** to resist strong continuous carriers
- **trial-decode Fisher scoring** to rank candidate tones
- **auto-threshold tuning** from running SNR margin so threshold follows QSB
- **post-lock quality watchdog** so weak/dirty locks can be dropped instead of drifting forever
- **adjacent-bin tone purity gate** to suppress broadband impulses (finger snaps, key clicks, splatter) at the source
- **wide-bin sniff** (`--wide-bin-count`) to integrate energy across ±N Goertzel bins for acoustically re-captured CW
- **force-pitch override** (`--force-pitch-hz`) that bypasses acquisition when the operator already knows the target
- **min-pulse / min-gap dot-fraction filters** reject sub-dot pulses and fill sub-dot gaps.
  File and microphone modes disable them by default. The microphone preset enables them.
- **WASAPI loopback capture** (`stream-live --loopback`) for same-machine digital playback (YouTube, browsers, local files) - separates "speaker→mic acoustic recapture" (research) from "render→loopback digital pipe" (operational)
- **centroid pitch picking** as a tiebreaker so locks centre on the energy ridge instead of edge-locking on a side bin
- **mic-mode preset** that bundles wider bins, lower purity, and the min-pulse/min-gap filters in one toggle
- **lockstep decode-and-play** (`stream-file --decode-and-play` / GUI **DECODE+PLAY**) uses one audio cursor.
  The cursor controls decode, playback, pause, seek, and region trim.
- **confidence state machine + held-event buffer** uses `hunting`, `probation`, and `locked` states.
  Characters do not reach the operator until the first quality check succeeds.
  The decoder discards false locks from voice formants or impulse noise.
  It buffers genuine CW during verification. It flushes the events in order when it confirms the lock.
  The GUI shows this state in the status bar.

This path is the more ambitious live decoder, but it still needs better corpus-driven measurement.

### 2. Append-only event-stream foundation

Implemented in `src\append_decode.rs` and surfaced by `stream-live-v3`.

This is intentionally simple:

- the envelope streamer classifies bars as `on_dit`, `on_dah`, `off_intra`, `off_char`, or `off_word`
- each event is anchored to the audio sample range that produced it
- a short trailing stability guard lets gaps mature before they are emitted
- each event is consumed once in audio order
- dits/dahs accumulate into one pending Morse character
- `off_char` flushes that character
- `off_word` flushes that character and appends a real space

The raw debug representation does not apply Morse lookup.
It uses `.` for a dit and `-` for a dah.
It uses `/` for a character gap and `//` for a word gap.

This representation permits comparison with the colored Visualizer bars.
It does not depend on Avalonia redraws.
Redraw logging repeated rolling windows and produced false text.
Event-stream logging showed the audio sequence.

This is the reference path for live decoding and regression prevention.
Complex pipelines can improve event classification, pitch selection, preprocessing, or spacing.
They must preserve this append-only contract or show a measurable improvement.

### 3. Causal ditdah baseline

Implemented in `src\ditdah_streaming.rs`.

This is intentionally simpler:

- keep a rolling audio window
- repeatedly run whole-window `ditdah`
- commit only the prefix that stabilizes across repeated snapshots

This baseline exists because it is:

- understandable
- reproducible
- easier to sweep
- easier to score against human labels

It remains a useful historical reference and comparison strategy for label-driven tuning, but it is no longer the GUI foundation.

## Signal processing architecture

The custom streaming decoder (`src\streaming.rs`) is a chain of stages, each addressing a specific failure mode the project has hit on real off-air audio. Read top-to-bottom - each stage assumes the previous one has done its job.

```
                    raw input audio (file or capture device)
                                    │
                                    ▼
                     ┌───────────────────────────────┐
                     │  resample to 12 kHz mono f32  │  rubato
                     └───────────────┬───────────────┘
                                     ▼
                     ┌───────────────────────────────┐
                     │   HP / LP biquad chain        │  300-1500 Hz CW band
                     └───────────────┬───────────────┘
                                     ▼
                ┌────────────────────┴────────────────────┐
                │ pitch_locked == None?                   │
                └────┬──────────────────────────┬─────────┘
                     │ yes                      │ no
                     ▼                          ▼
        ┌──────────────────────┐   ┌────────────────────────┐
        │ ACQUISITION          │   │ TRACKING               │
        │  • emit "hunting"    │   │  • Goertzel @ pitch    │
        │  • build pre-lock    │   │  • + side bins for     │
        │    audio buffer      │   │    instantaneous       │
        │    (PITCH_LOCK_S or  │   │    tone-purity ratio   │
        │    RELOCK_S after a  │   │  • + wide-bin sniff    │
        │    recent loss)      │   │    (mic-mode integ.)   │
        │  • try_acquire:      │   │                        │
        │     trial_decode     │   │  per-sample gates:     │
        │     Fisher per cand  │   │    amplitude > thr ∧   │
        │     pitch (ditdah    │   │    smoothed SNR ok ∧   │
        │     re-decode)       │   │    tone_purity > k ∧   │
        │  • centroid tiebreak │   │    not impulse         │
        │  • commit best ≥     │   │                        │
        │    MIN_LOCK_FISHER   │   │  on/off → durations →  │
        │  • emit "probation"  │   │  ditdah symbol classif │
        └──────────┬───────────┘   │  → Char / Word events  │
                   │ lock          └─────────┬──────────────┘
                   ▼                         │
                                             ▼
                              ┌──────────────────────────────┐
                              │ CONFIDENCE FILTER            │
                              │   hunting   → drop chars     │
                              │   probation → hold chars     │
                              │   locked    → pass chars     │
                              └──────────┬───────────────────┘
                                         ▼
                              ┌──────────────────────────────┐
                              │ QUALITY WATCHDOG             │
                              │  every QUALITY_CHECK_S over  │
                              │  QUALITY_WINDOW_S buffer:    │
                              │    Fisher < FAST_DROP        │
                              │      → drop, "hunting",      │
                              │        discard held          │
                              │    Fisher in [FAST_DROP,     │
                              │      MIN_HOLD) for N checks  │
                              │      → drop, hysteresis      │
                              │    Fisher ≥ MIN_HOLD         │
                              │      → if probation:         │
                              │          promote, flush held │
                              │        else: keep            │
                              └──────────┬───────────────────┘
                                         ▼
                                  StreamEvent stream
                            (PitchUpdate, Char, Word, Garbled,
                             WpmUpdate, Power, PitchLost,
                             Confidence)
```

### Key design properties

- **Two-stage detection.** Acquisition uses `trial_decode_score`, which performs a ditdah pass on a candidate window.
  Thus, acquisition locks only on tones that look like CW.
  Tracking uses a less expensive Goertzel and gate path for each sample.
  This design keeps the steady-state CPU cost low.
- **Acquisition-first hypothesis.** The custom decoder was originally
  the bottleneck. It is not the bottleneck now. The remaining hard cases on the
  9-label corpus are dominated by **wrong-tone lock**, **late lock**,
  and **lock on noise**, not by symbol classification errors. The
  oracle-tone eval mode and the planned top-K tracker (Phase 3) target
  these directly.
- **Confidence machine = first-class operator UX.** The GUI shows the decoder state in a colored status badge.
  The states are `Hunting`, `Probation`, and `Locked`.
  The state controls character events. Thus, voice and transient false locks do not reach the transcript.
  The watchdog can take several seconds to confirm a false lock.
- **Held-event buffer keeps probation honest.** While in `Probation`,
  decoded characters are held (not dropped). If the lock survives,
  the held buffer is flushed in order so genuine CW that started
  during the verification window is preserved. If the lock is rejected,
  the held buffer is discarded.
- **Two acquisition surfaces, one engine.** Microphone and loopback
  audio use the same streaming decoder. The microphone path uses the
  wide-bin, min-pulse, min-gap, and lower-purity preset. Loopback uses
  the default file-mode settings because its audio is identical to the
  source.

### Confidence state machine

```
       (start)
          │
          ▼
   ┌─────────────┐    pitch_lock acquired
   │  Hunting    ├──────────────────────────┐
   │ (red badge) │                          │
   └─────▲───────┘                          ▼
         │                            ┌──────────────┐
         │                            │  Probation   │
         │ watchdog drop              │ (amber badge)│
         │ (Fisher < FAST_DROP, or    └──────┬───────┘
         │  MIN_HOLD failed N times)         │
         │                                   │ first watchdog check
         │                                   │ Fisher ≥ MIN_HOLD
         │ ←─────────── watchdog drop ───────┤
         │                                   ▼
         │                            ┌──────────────┐
         │                            │   Locked     │
         │                            │ (green badge)│
         │                            └──────┬───────┘
         │                                   │
         └─── watchdog drop ─────────────────┘
```

Char-class events (`Char`, `Garbled`, `Word`, `WpmUpdate`):

| State      | Treatment of decoded chars |
|------------|----------------------------|
| Hunting    | Dropped at the gate (lock not even attempted yet, or just lost) |
| Probation  | Buffered in `held_events`, awaiting verdict |
| Locked     | Passed through unchanged. Held buffer is flushed on the transition |

Confidence transitions emit `StreamEvent::Confidence { state }`, which serializes as `{"type":"confidence","state":"hunting|probation|locked"}` over NDJSON. The Avalonia GUI reads this on the Decode tab and updates the status badge plus colour scheme.

The YouTube reference clip `cw_30wpm_youtube_12k.wav` caused this change.
Its pre-CW voice produced false text such as `MI U I EIE N`.
The decoder also missed `CQ DE K UR` because the false voice lock expired slowly.

The confidence machine puts the false lock into Probation without output.
The watchdog rejects the lock and discards its held events.
The operator sees no text until the real lock near 604 Hz passes its check.
The decoder then flushes the genuine `73 TNX RST R TU = OM FB ...` transcript.

## GUI architecture

The Avalonia application in `gui\` is named **CW SCOPE**. It is the main operator interface.
It starts the Rust `cw-decoder` and `eval` binaries.
It searches upward from `AppContext.BaseDirectory` for `experiments\cw-decoder\target\{release,debug}\`.
Debug and release binaries work. The GUI selects the release binary when both exist.

The GUI does **not** build the Rust engine.
If it finds no binary, it tells the operator to run `cargo build`.
Usually, run `cargo build --release` in `experiments\cw-decoder`.

The GUI is organized into three tabs: **Decode**, **Labeling**, and **Tuning**.

### Decode tab

The GUI now supports two decoder modes:

| Mode | Backing path | Primary use |
|---|---|---|
| `Custom streaming` | `stream-file` / `stream-live` | primary live-decoder experiment |
| `Baseline ditdah` | `stream-file-ditdah` / `stream-live-ditdah` | honest label-driven A/B reference |

Current decode-tab workflow also includes:

- explicit source control semantics:
  - **DECODE FILE...** opens and immediately decodes a chosen file
  - **START LIVE** captures from the selected input device
  - **DECODE+PLAY** opens a file, decodes it, and plays the same audio in lockstep so what you hear is exactly what is being processed
- live capture
- optional recording of live audio to `data\cw-recordings\`
- offline replay of the last opened source (live recording or file decode)
- **Replay & Score** live-vs-offline comparison with a visible CER chip
- inline audio playback with a shared transport / progress surface
- a real-time playback signal view driven by the same broad-band profile pipeline used in labeling
- an explicit **CURRENT TONE** readout during live decode and playback
- a prominent confidence badge that shows **LOCKED**, **VERIFYING SIGNAL**, or **ACQUIRING TARGET**
  - decoded characters appear only when the badge is green
  - while the badge is amber, the engine buffers candidate output and waits for the quality check
- a **Mic mode** preset toggle that bundles wide-bin sniff, lower tone-purity threshold, and the min-pulse/min-gap dot filters into one click for acoustically re-captured CW
- **WASAPI loopback** capture (`stream-live --loopback`) for playback on the same computer
  - supports YouTube, browsers, and local files
  - reads audio from the system render endpoint and bypasses acoustic microphone capture
- an experimental **RANGE LOCK** mode for custom streaming, so live/file decode can prefer the strongest tone inside a chosen Hz window
- an experimental **TONE PURITY** gate
  - compares target-bin power with off-band noise bins for the same sample
  - uses q25 of bins at offsets of 150, 300, 500, and 700 Hz
  - gives a real CW tone an instantaneous purity value from 5 to more than 20
  - gives a broadband impulse a value near 1 and rejects it
  - uses `min_tone_purity = 3.0` by default
  - uses a value of 0 to disable the gate
  - reuses existing noise bins and runs before smoothing
- an optional **SHOW CHAR HZ** overlay that shows the locked tone for each character
  - **SHOW PURITY** adds the peak tone-purity ratio below the frequency
  - broadband impulses usually have `purity ~1`
  - real CW usually has `purity 5-20+`
- a **FORCE PITCH (Hz)** acquisition override that locks the streaming decoder to an exact pitch instead of running auto-acquisition (0 = auto). The Fisher quality watchdog AND the confidence machine are both bypassed when forced - the decoder goes straight to Locked and stays there. Useful when the operator already knows the target tone, or as a diagnostic ("does the decoder fail because of acquisition or downstream?")
- a **WIDE BINS** control from 0 to 8
  - adds Goertzel bins at `pitch ± k * bin_width`
  - sums their power into the main signal estimate
  - uses one 40 Hz bin when the value is 0
  - gives approximately 200 Hz of integration bandwidth when `N=2`
  - supports CW audio that passes from a speaker through a room to a microphone
  - compensates for speaker response, room reverberation, and small pitch changes
  - prevents envelope flicker when one 40 Hz bin captures only about 30% of the signal energy
  - uses CLI option `--wide-bin-count <N>` on `stream-file` and `stream-live`
  - uses NDJSON field `"wide_bin_count": <N>`
  - can be combined with `--force-pitch-hz` for live microphone capture
  - example: `--force-pitch-hz 620 --wide-bin-count 2`

The tone-purity gate replaces a recent-audio detection guard that ran when a character was emitted.
The earlier guard did not detect a completed transient impulse.
The new gate runs for each Goertzel power sample.
It operates with the amplitude and smoothed-SNR gates.
A sample is key-down only when the locked bin is sufficiently louder than adjacent bins.

Use replay to compare the live result with an offline result for the same captured audio.

### Labeling tab

The labeling workflow is now built around exact-window truth:

- harvest candidate regions from recordings
- if harvest finds no regions, fall back to a single whole-file candidate so faint recordings can still be labeled
- preview slowed audio inline inside CW SCOPE instead of shelling out to an external player
- view signal profile
- drag exact start/end handles
- save uppercase verified copy to JSONL

Saved labels retain:

- exact adjusted window
- original harvested window
- `clip_start`
- `clip_end`
- decoder snapshots used during labeling

Signal-profile rendering now also works without a usable pitch lock by falling back to a broadband activity profile. That keeps the editor usable on faint files where neither decoder can confidently lock a tone yet.

Harvest caching is now both:

- in-memory while you stay in the current GUI session, and
- stored in the local application-data cache

After a restart, a harvested file opens with its cached candidate list.
Select **HARVEST** again to replace the list.

The strong-signal W1AW path now uses warmup-aware harvest windows for streaming.
Thus, a cold decoder on each four-second slice does not force a complete-file fallback.

### Tuning tab

The tuning workflow is now first-class in the GUI:

- score the current label file, the full corpus, or any checked subset of available `*.labels.jsonl` files
- run parameter sweeps with a coarse pass plus a local refinement pass around the best baseline candidate
- inspect score cards, failure-breakdown bars, and per-label truth-vs-decoded detail instead of raw console text
- inspect sweep rankings with exact-match progress bars plus average / worst CER
- **Apply Top Result**
- score the experimental custom-streaming range-lock path against labels by enabling **RANGE LOCK** on the Tuning tab

Baseline sweep still tunes the causal `ditdah` reference only. When **RANGE LOCK** is enabled, use **Score Labels** to measure the streaming experiment rather than **Sweep Baseline**.

When Decode mode = **Baseline ditdah**, the Decode tab uses the same shared tuning settings as the Tuning tab.

That gives the branch an honest loop:

`label -> score/sweep -> apply top result -> decode tab file/live A/B`

## Labeling model and whether it still makes sense

Yes - **the labeling approach still makes sense and is still worth pursuing**.

The current exact-window + clipped-edge scheme has already paid off because it separated two very different classes of problems:

- **boundary / warmup / commit issues**
- **hard-signal isolation / segmentation issues**

Without the labels, most misses just looked like “decoder bad.”

With the labels, we can already see that:

- strong W1AW-style copy is mostly recoverable
- some misses are specifically leading-edge or commit-policy failures
- the harder contest-style recordings are a different problem class

That said, the current label schema is **necessary but not sufficient** for the hardest recordings.

Make the next label additions optional. Do not reset the schema:

- target tone estimate / confidence
- multiple-signal flag
- copy-confidence flag
- negative / no-copy labels
- short notes for “weaker target under stronger adjacent station” style cases

So the answer is:

- **keep labeling**
- **keep exact-window truth**
- **do not throw away the current corpus**
- **expand metadata only where hard signals need more context**

## Experiments to date

## Phase 1: custom streaming decoder and real-audio harvest

Initial work established:

- real-file decode
- live audio capture
- an early custom streaming path
- harvest from offline/stream agreement
- pause-bounded region snapping

This gave the project real off-air candidate regions instead of synthetic clean-CW toy cases.

## Phase 2: exact-window human labeling

The GUI labeling workflow added:

- slowed preview audio
- signal-profile editing
- exact-window saved truth
- clipped-edge flags

This is the key change that turned the experiment into a measurable loop.

## Phase 3: simplified causal baseline

Whole-window `ditdah` succeeded on the strong W1AW sample where the earlier streaming path missed copy. That led to the simplified causal baseline:

- repeated whole-window `ditdah`
- prefix stabilization
- streaming-style transcript commit

This baseline became the reference scorer target.

## Phase 4: scorer and parameter sweep

`eval` now supports:

- label discovery via `--labels-dir` / `--all-labels`
- repeated `--labels <file>` arguments so scoring/sweeping can target an arbitrary subset of label files
- exact-window scoring
- full-stream scoring
- wide and interactive sweeps
- sweep ranking by exact matches, total edit distance, average CER, and worst-case CER
- failure classification such as:
  - `exact`
  - `leading_edge_error`
  - `near_match`
  - `spacing_only_error`
  - `garbage_decode`
  - `empty_output`

## Phase 5: GUI integration

The experiment no longer depends on shell-only tuning:

- Tuning tab exposes score/sweep
- Apply Top Result connects sweeps to baseline decode
- harvest results are cached per file and persisted across app restarts
- Decode tab can record live audio and replay it offline for CER comparison
- Decode and Labeling now share inline audio playback instead of launching an external media player
- CW SCOPE now shows a moving signal profile/playhead during playback

### Integration with the QsoRipper GUI (Round 1, PR #324)

The QsoRipper desktop GUI (`src\dotnet\QsoRipper.Gui`) hosts `cw-decoder.exe`
as a subprocess (`stream-live --json`) and consumes the NDJSON event stream
to drive two operator-facing surfaces:

- **CW WPM auto-fill**: when a CW QSO is logged, the time-weighted mean WPM
  over the QSO start→end window is written to `QsoRecord.cw_decode_rx_wpm`.
- **F9 CW Stats pane**: This live overlay shows confidence, lock state, signal pitch, WPM, recent characters, and the last garbled symbol.
  The CW Scope tools and this pane use the same NDJSON stream.

**Operator activity, not decoder lock, defines the episode boundary.**
The QSO clock starts when the operator first types a callsign.
It ends when the operator saves or clears the QSO.
The decoder runs continuously while Radio Monitor is enabled.

### Advanced diagnostics mode

Enable **Settings → Advanced CW diagnostics** to capture an offline-debug
bundle for every QSO. Each radio-monitor session writes to:

```text
%LOCALAPPDATA%\QsoRipper\diagnostics\session-<UTC>\
  session.json                 startup metadata (binary, device, loopback)
  session.wav                  continuous mirror of decoder input audio
  session-events.ndjson        every raw NDJSON line emitted by the decoder
  episodes\episode-NNN\
    events.ndjson              decoder events between callsign-typed and save/clear
    ux-snapshot.json           comparison: aggregator mean vs displayed UI
                               WPM vs in-window samples + the QsoRecord +
                               a copy/paste repro command
```

The repro command in each `ux-snapshot.json` re-runs the decoder against the
captured WAV over the same time window. Sample form:

```text
cw-decoder decode-and-play --json --start 12.4 --end 47.9 "session.wav"
```

Use this command to compare the status-bar value with a deterministic offline replay.
This method finds round 1 WPM regressions without a reproduction of live propagation.

WAV size is roughly 330 MB/hour (48 kHz mono, 16-bit) and is not rotated in
round 1. Disable diagnostics or prune `%LOCALAPPDATA%\QsoRipper\diagnostics`
manually between debug sessions.

### WPM emission smoothing (#326)

The first diagnostics capture found a failure in `current_wpm()`.
During a sustained signal degradation, raw WPM decreased from 11.3 to 6.75 in approximately six seconds.
The pitch lock continued to report a good condition.
The pitch-quality watchdog operated approximately six seconds after the WPM decrease.
Thus, the displayed speed decreased from approximately 13 WPM to 6 WPM during the QSO.

`StreamingDecoder` now emits a smoothed value instead of the raw
`current_wpm()` in `StreamEvent::WpmUpdate`:

1. **Median over the last `WPM_SMOOTH_WINDOW` (=7) raw samples.** Rejects
   single degenerate calibration windows where one mis-classified
   character produces a wild dot-length estimate.
2. **Rate cap of `WPM_MAX_REL_DELTA_PER_EMIT` (=3%) per emit.** Real
   operators cannot change keying speed by more than this between adjacent
   characters. A larger change shows that dit-cluster calibration follows
   noise instead of the operator. Genuine WPM changes converge in
   approximately three seconds. A calibration failure becomes slow enough
   for the watchdog to drop the lock first.

The internal `current_wpm()` is unchanged.
End-of-run summaries and harvest output continue to use it.
In a replay of the captured session, displayed WPM stays above 9.5.
Before the fix, it decreased to 6.75 in the same interval.

## Current labeled corpus

Label files are under `data\cw-samples\` at the repository root.
They are not under `experiments\cw-decoder\`.

`--all-labels` resolves `data\cw-samples\` from the current working directory.
It finds the labels when you run `eval` from the repository root.
It can find no labels when you run the command from a different directory.

The examples use `--labels-dir data\cw-samples` to make the path clear.
From the repository root, `--all-labels` gives the same result.

Current corpus files and label counts:

| File | Labels |
|---|---|
| `data\cw-samples\W1AW_de_W5WZ_DX_CW_20180623_000422Z_14MHz.labels.jsonl` | 6 |
| `data\cw-samples\k5zd-zs4tx-80m-qso.labels.jsonl` | 2 |
| `data\cw-samples\K1ZZ_de_DH8BQA_CQWWCW_CW_20151129_174710Z_14MHz.labels.jsonl` | 1 |

Current size: **9 labels**

Companion `.mp3` recordings (including `K1ZZ_de_LA8OM_*`, `k5zd-ey8mm-40m-qso`, and ad-hoc `radio-*` captures) are present but not yet labeled.

## Current results

### Label corpus reference scores

The corpus currently has **9 labels**. The safest command form is explicit about the repo-root label directory:

```powershell
cargo run --release --manifest-path experiments\cw-decoder\Cargo.toml --bin eval -- --labels-dir data\cw-samples
```

From the repository root, `--all-labels` gives the same result.
It resolves `data\cw-samples\` from the current working directory.
Do not run it from `experiments\cw-decoder`.

Current exact-window score:

- **6 / 9 exact**
- **avg CER = 0.10**
- **total edit distance = 12**

Current full-stream score:

- **1 / 9 exact**
- **avg CER = 0.85**

Interpretation:

- exact-window scoring still tells us what the classifier can do when the target audio is bounded correctly
- full-stream scoring tells us that acquisition, segmentation, gap maturity, and finalization are the hard live problems
- the append-only event-stream foundation is the best current live path
- it removes rolling-window replacement and stitching from the critical path
- labels and replay transcripts can measure its results directly

### Append-foundation smoke evidence

The foundation path now runs through the same Rust core in every surface:

- `stream-live-v3` emits the append transcript as primary `text` / `transcript`
- `cursor_transcript` keeps the older event-cursor transcript for diagnostics
- `raw_morse` exposes the raw event stream for bar-level debugging
- `eval --strategy-sweep --strategies foundation` compares the foundation against other strategies
- `bench-latency --foundation --json` emits transcript-quality rows for the append foundation
- the GUI defaults Decode/Labeling file and live runs to **Append event stream (foundation)**
- the Visualizer still has an **APPEND DECODE** view/debug path, but it is now aligned with the same underlying append contract

On synthetic PARIS bench scenarios the foundation clean/noise transcripts are recognizable immediately (`PARIS PARIS ...`) while voice-lead-in scenarios still show garbage before the target appears. That is useful: it confirms the foundation is simple and honest rather than hiding acquisition/target-isolation failures behind post-hoc stitching.

## Current thinking

## What we know with reasonable confidence

1. **The simple append-event stream is working much better than the rolling-window transcript machinery.**
   The new method classifies bars and appends each mature event once.
   The old method decoded a rolling window and then joined its text.
   The new method removes ghost characters, repeated prefixes, text replacement, and overlapping-window artifacts.
2. **Visualizer truth is event truth, not redraw truth.**
   The colored bars are a rolling display. Logging every redraw records repeated partial windows (`..`, then `..-`, then `..- ...`) and creates fake Morse. The useful debug layer is the audio-time event stream beneath the redraw.
3. **Spacing is now visible and testable.**
   The raw stream (`.` / `-` / `/` / `//`) shows incorrect word gaps.
   For example, it shows `...//.-` (`S A`) instead of `.../.-` (`SA`).
   Future spacing work can target this failure directly.
4. **Audio/playback synchronization matters.**
The old Visualizer file path used separate decode and audio processes. Thus, the visual bars did not always agree with the audio. `stream-live-v3 --file --play` uses one process. It sends the output playback cursor to the decoder.
5. **Hard contest audio is still the frontier.**
   The foundation does not magically solve target isolation, voice lead-ins, same-band QRM, or weak/noisy spacing. It gives us a stable place to measure those failures without rolling-window artifacts obscuring them.

## What this implies for next steps

### Keep the append foundation as the non-regression line

The append-only path is now the default contract:

```text
audio -> envelope/viz events -> append event decoder -> transcript
```

Use several test levels to detect regressions:

1. **Unit level:** `AppendEventDecoder` tests must cover repeated `viz` frames, character gaps, word gaps, and the final pending-character flush.
2. **CLI level:** `stream-live-v3 --json` transcript events must keep `transcript` / `text` as the append-foundation text, with `cursor_transcript` only as diagnostics.
3. **GUI level:** Decode, Labeling, Bench, Tuning, and Visualizer must use or include `foundation`. Identify each future mode as experimental.
4. **Corpus level:** Each future algorithm must report against `--labels-dir data\cw-samples`. Each strategy sweep must include `foundation`.
5. **Bench level:** Keep `bench-latency --foundation --json` as a quick test. It must produce recognizable transcript rows before detailed latency tests.

Improvements can change event detection, filtering, or classification.
They must not restore rolling text stitching as the primary live transcript path.

### Keep pursuing labeling, but evolve it carefully

Do **not** stop investing in labels.

Instead:

1. keep the current exact-window label corpus active
2. add richer metadata only for hard cases
3. add some negative / no-copy examples
4. add target-tone hints where multiple CW signals are present

### Use foundation-first evaluation

For corpus work, keep the current order:

1. append-foundation score / replay transcript
2. exact-window label score as the upper-bound classifier check
3. full-stream score as the live segmentation/finalization check
4. experimental strategy sweeps that always include `foundation`

### Use experiments as layers above the stable base

The promising future work is no longer "replace the foundation." It is "make better events for the foundation to append":

- **spacing classifier:** tune char-vs-word gap thresholds, make gap maturity explicit, and score raw Morse gaps against labels where possible
- **target isolation:** track multiple tone ridges and choose or present candidates instead of winner-takes-all pitch lock
- **preprocessing:** bandpass-around-pitch and dynamic compression helped real radio clips, but must be gated so clean synthetic CW does not regress
- **matched element scoring:** replace hard threshold chatter with soft scores over candidate 1-dot, 3-dot, and 7-dot windows
- **lock/acquisition policy:** speed up acquisition after voice/noise lead-ins without allowing noise-locked dit spam
- **region segmentation:** detect active CW spans and compare region-local decode against the append live transcript
- **multi-surface diagnostics:** keep raw Morse, transcript, bars, WPM, pitch, SNR, and label CER tied to the same audio-time cursor

But every improvement must be measured back against:

- the label corpus
- replay CER
- raw Morse gap fidelity
- foundation-vs-experiment deltas

## Recommended next steps

1. **Promote foundation regression checks.**
   Keep tests for `src\append_decode.rs`.
   Require `foundation` in strategy sweeps.
   Preserve `raw_morse` and `cursor_transcript` diagnostics.
   These diagnostics must explain changes, not only show final text.
2. **Quantify spacing failures.**
   The current foundation clearly showed word-gap errors.
   The next scorer must classify character substitution, character-gap, and word-gap errors.
3. **Close the acquisition gap after voice/noise lead-ins.**
   Synthetic test results show that the append foundation gives an accurate result.
   It recognizes clean and noisy PARIS, but voice lead-ins still cause incorrect initial text.
   Improve target detection and lock admission. Do not change transcript stitching.
4. **Layer preprocessing carefully.**
   Real-radio bandpass+compander preprocessing can help dramatically, but it previously broke clean synthetic CW in some paths. Treat preprocessing as an optional layer above the foundation with explicit A/B coverage.
5. **Add richer label metadata for hard cases.**
   Add target tone, a multi-signal flag, negative regions, no-copy regions, and gap annotations.
   These values will help the review of future experiments.
6. **Top-K candidate tracker.**
   Replace single-pitch lock with a CFAR-scored ridge tracker over 350-1500 Hz.
   Show separate track candidates for contest audio that contains multiple signals.

Current evidence suggests:

- **clean/noisy single-target misses** -> spacing maturity and final pending-character flush
- **voice lead-in misses** -> acquisition / lock admission
- **contest/multi-signal misses** -> target isolation + segmentation

## Practical workflow today

If the goal is the fastest useful loop on another PC with live radio audio:

1. pull this branch
2. build `experiments\cw-decoder`
3. run the GUI
4. use **Append event stream (foundation)** for Decode / Labeling / Visualizer
5. record live audio and use replay/label scoring to compare against ground truth
6. keep `foundation` in every strategy sweep so improvements are real and regressions are obvious

If the goal is custom-streaming research:

- keep the GUI default at **Append event stream (foundation)**
- use live recording + replay CER for quick iteration
- keep the label corpus as the harder regression gate
- treat other modes as experiments layered above the stable base

## Build and run

The Avalonia GUI launches whichever Rust binaries it finds under `experiments\cw-decoder\target\{release,debug}\` (release preferred) but does **not** rebuild them. A debug build is enough to make the GUI run. Release is recommended for realistic decode latency. Build the engine first, then the GUI:

```powershell
cargo build --release --manifest-path experiments\cw-decoder\Cargo.toml
dotnet build experiments\cw-decoder\gui\CwDecoderGui.csproj
```

Run the GUI:

```powershell
dotnet run --project experiments\cw-decoder\gui\CwDecoderGui.csproj
```

If you want to smoke-test inline playback directly from the CLI:

```powershell
cargo run --release --manifest-path experiments\cw-decoder\Cargo.toml -- play-file data\cw-samples\W1AW_de_W5WZ_DX_CW_20180623_000422Z_14MHz.mp3 --json
```

Run the scorer on the full corpus (from the repo root, since labels live under `data\cw-samples\`):

```powershell
cargo run --release --manifest-path experiments\cw-decoder\Cargo.toml --bin eval -- --labels-dir data\cw-samples --window 20 --min-window 0.5 --decode-every-ms 1000 --confirmations 3
```

Run the full-stream scorer:

```powershell
cargo run --release --manifest-path experiments\cw-decoder\Cargo.toml --bin eval -- --labels-dir data\cw-samples --mode full-stream --window 20 --min-window 0.5 --decode-every-ms 1000 --confirmations 3 --post-roll-ms 1500
```

Run the foundation strategy in the same sweep harness used by the Tuning tab:

```powershell
cargo run --release --manifest-path experiments\cw-decoder\Cargo.toml --bin eval -- --labels-dir data\cw-samples --strategy-sweep --strategies foundation
```

Run the experimental range-lock scorer against a focused label subset:

```powershell
cargo run --release --manifest-path experiments\cw-decoder\Cargo.toml --bin eval -- --labels data\cw-samples\W1AW_de_W5WZ_DX_CW_20180623_000422Z_14MHz.labels.jsonl --experimental-range-lock --range-lock-min-hz 550 --range-lock-max-hz 850
```

Run a baseline `ditdah` parameter sweep against the corpus:

```powershell
cargo run --release --manifest-path experiments\cw-decoder\Cargo.toml --bin eval -- --labels-dir data\cw-samples --sweep-ditdah --wide-sweep --top 10
```

Run scoring or sweeping against a hand-picked subset of labels:

```powershell
cargo run --release --manifest-path experiments\cw-decoder\Cargo.toml --bin eval -- --labels data\cw-samples\W1AW_de_W5WZ_DX_CW_20180623_000422Z_14MHz.labels.jsonl --labels data\cw-samples\k5zd-zs4tx-80m-qso.labels.jsonl --sweep-ditdah --top 5
```

Without any `--labels` / `--labels-dir` / `--all-labels` flag, `eval` falls back to its built-in synthetic suite (silence, noise, clean/noisy synthesized CW) instead of label scoring.

On the debug binary, label sweeps can still take a few minutes because each config replays the selected corpus. For practical tuning loops, prefer `cargo build --release --bins` so CW SCOPE launches the faster release `eval.exe` / `cw-decoder.exe`.

Probe likely target tones by Fisher score:

```powershell
cargo run --release --manifest-path experiments\cw-decoder\Cargo.toml -- probe-fisher data\cw-samples\k5zd-zs4tx-80m-qso.mp3 --min-hz 350 --max-hz 1500 --step-hz 10 --top 8
```

Run the cold-start and lock-stability benchmark on the synthetic scenario matrix.
The matrix includes silence, noise, voice, and long-clean-CW lead-ins.
It reports `lat_ms = t_stable_N - cw_onset_ms`.
It also reports post-lock uptime, drops, relock cycles, and the longest non-Locked gap.

```powershell
.\experiments\cw-decoder\target\release\cw-decoder.exe bench-latency
```

Same benchmark on a real recording (operator supplies CW onset + truth):

```powershell
.\experiments\cw-decoder\target\release\cw-decoder.exe bench-latency `
    --from-file data\cw-recordings\live-20260422-220247.wav `
    --cw-onset-ms 0 `
    --truth "W7LXN DE WA?FBSA K" `
    --stable-n 3
```

Compare two configurations by tagging each run with `--label`. Combine with `--json` to capture machine-readable rows for offline comparison:

```powershell
.\experiments\cw-decoder\target\release\cw-decoder.exe bench-latency --label baseline    --json > bench-baseline.ndjson
.\experiments\cw-decoder\target\release\cw-decoder.exe bench-latency --label no-purity   --purity 0 --json > bench-no-purity.ndjson
```

Run the append-foundation bench smoke. This records transcript quality for the GUI foundation. Latency and lock fields are intentionally empty in this mode:

```powershell
.\experiments\cw-decoder\target\release\cw-decoder.exe bench-latency --foundation --json > bench-foundation.ndjson
```

List live audio devices and run the legacy TUI:

```powershell
cargo run --release --manifest-path experiments\cw-decoder\Cargo.toml -- devices
cargo run --release --manifest-path experiments\cw-decoder\Cargo.toml -- live --device "USB Audio CODEC"
```

Run the 30 WPM abbreviation bench across the full real-audio variant matrix (12 scenarios from clean to chaos):

```powershell
# One-time: regenerate the variant WAVs (gitignored, ~50 MB total).
.\experiments\cw-decoder\scripts\gen-30wpm-variants.ps1

# Then bench:
.\experiments\cw-decoder\scripts\bench-30wpm.ps1 -Label default
```

The variant matrix is intentionally tiered:

| Tier | Variants | What stresses the decoder |
|---|---|---|
| baseline | `clean`, `weak`, `qrn`, `qsb`, `weak_qsb` | mild SNR or fade. The decoder must pass. |
| extreme | `extreme_qrn`, `crushed`, `deep_qsb`, `buried` | heavy brown noise (mostly killed by the 300 Hz HP) + deep slow QSB. `buried` combines all three and is where the decoder first cracks |
| harsh   | `harsh_white`, `inband_qrm`, `chaos` | white / CW-band-bandpassed noise the front end *cannot* filter away. Locks acquire instantly on the right pitch but symbol classification fails - this is where the decoder's downstream gating becomes the bottleneck rather than acquisition |

The `harsh_white`, `inband_qrm`, and `chaos` variants show a decoder weakness.
The keying envelope chatters in dense in-band noise, although lock and Fisher confidence are good.
The ditdah classifier then emits long groups of incorrect characters.

The next bench target is a better `false_chars_before_stable` value for these variants.
Issue [#320](https://github.com/rtreit/qsoripper/issues/320) tracks this work.

### Downstream classifier hardening for #320 (chatter-merge + duration sanity + rescue suppression)

The first hysteresis-only patch stopped the long ghost-character stream on `harsh_white`.
It did not stop envelope chatter in dense noise.
The dot/dah classifier interpreted this chatter as groups of `E` and `T`.

A second change added four related fixes.
All fixes are optional and available through the CLI, JSON configuration, and bench script.

1. **Hard ON-duration sanity gate** (always on). After the dot length is known, ON intervals shorter than `0.4 · dot` or longer than `4.8 · dot` are dropped *and* the in-progress letter is cleared. This stops both threshold chatter from being classified as a dit and giant QRM blobs from being classified as a dah. Tracked by the `invalid_on_duration_dropped` counter.
2. **Single-element rescue suppression**. Previously, the code rescued all `valid_morse` letters when the rhythm gate was closed.
   Thus, short pulses produced false `E` (`.`) and `T` (`-`) characters.
   Rescue now applies only to multi-element patterns and uses `RhythmGate::was_recently_mature`.
   The `single_element_rescue_suppressed` counter tracks this action.
3. **Real merge for `min_gap_dot_fraction`**. Previously, the code dropped a short OFF interval.
   It then classified the adjacent ON intervals separately.
   Thus, a noise gap in a dah produced `. .` instead of `-`.
   The `sanitize_interval` function in `streaming.rs` now combines adjacent ON intervals before classification.
   The `short_gaps_bridged` and `on_runs_merged` counters track this action.
4. **Hysteresis wired through every streaming path**. `--hysteresis-fraction` is now accepted by `cw-decoder stream-file`, `stream-live`, `decode-and-play`, `bench-latency`, and `eval` (previously only `bench-latency` and JSON-stdin took it). The `bench-30wpm.ps1` script gained `-Hysteresis`, `-MinGap`, and `-MinPulse` parameters so the full sweep is one command.

Bench JSON now includes a `decoder_counters` block.
It includes `raw_edges_total`, `short_pulses_dropped`, `short_gaps_bridged`, and `on_runs_merged`.
It also includes `invalid_on_duration_dropped`, `single_element_rescue_suppressed`, and `chars_emitted`.
These values show whether a configuration improves CER or only suppresses output.

Best-known bench config on the 12-variant matrix is `-Hysteresis 0.3 -MinGap 0.2 -MinPulse 0.3`:

| variant       | baseline ghost | best-config ghost | baseline lat_ms | best-config lat_ms |
|---------------|---------------:|------------------:|----------------:|-------------------:|
| clean         | 0              | 0                 | 15700           | 15700              |
| weak          | 0              | 0                 | 14000           | 14000              |
| qsb           | 2              | 0                 | 14700           | 14700              |
| weak_qsb      | 2              | 1                 | 14000           | 14000              |
| crushed       | 6              | 0                 | 17800           | 17800              |
| deep_qsb      | 3              | 1                 | 14000           | 14000              |
| harsh_white   | **343**        | **0**             | 110500          | (never stable)     |
| inband_qrm    | 0              | 0                 | (never stable)  | (never stable)     |
| chaos         | 0              | 0                 | (never stable)  | (never stable)     |

The four fixes reduce ghost output across the complete baseline tier without a latency regression.
Most variants have zero ghost characters. The two QSB-heavy variants have one.
The fixes also stop the harsh-tier ghost flood.

The remaining problem is stable acquisition for `harsh_white`, `inband_qrm`, and `chaos`.
Continuous in-band carriers cause this problem, not the downstream classifier.
The next test must examine CFAR local-contrast detection or an envelope against a slow carrier floor.

The changes do not meet all acceptance criteria in #320.
They do resolve the initial ghost-character failure.

### Opt-in CFAR keying (#322)

The first CFAR keying implementation is available through an optional flag.
Use `--cfar-keying`, or use `-CfarKeying` in `bench-30wpm.ps1`.

This mode sends the dimensionless ratio `smoothed / noise` to the on/off threshold state machine.
It does not use raw `smoothed` Goertzel power.
It bypasses the global `snr_ok` gate.
The rolling-quantile threshold for the ratio supplies its own discrimination.

This was the only per-frame variant in the #322 matrix that produced a stable `harsh_white` transcript.
It became stable at 72.9 seconds after 38 ghost characters.
However, it causes regressions in the `clean` and `qsb` scenarios.
Therefore, the flag is off by default.
Production behavior is unchanged for all 12 baseline scenarios.

| Mode | PASS | WARN | FAIL | Total ghost | `harsh_white` |
| --- | --- | --- | --- | --- | --- |
| default (no flag) | 6 | 3 | 3 | 2 | no stable |
| `-CfarKeying` | 5 | 3 | 4 | 42 | **stable @72.9 s** |

Issue #322 records the result.
Per-frame normalization alone cannot decode the harsh tier without a regression in the clean tier.

The next iteration will use `--cfar-keying` as its base.
It will apply soft matched scoring to candidate one-dot, three-dot, and seven-dot windows after the initial dot estimate.

## Repo-local artifacts

- `gui-screenshot*.png` - historical GUI screenshots tracking visual iteration on the Decode tab
- `screenshots\sensitivity-panel.png` - close-up of the sensitivity / threshold panel
- `target\` - local Cargo build output (debug + release) for `cw-decoder` and `eval`
- `gui\bin\`, `gui\obj\` - local .NET build output for the Avalonia GUI
- `bench-runs\` - per-label JSON results from `bench-30wpm.ps1`
- `artifacts\run\cw-debug-bars-*.txt` - Visualizer append-debug raw Morse streams (`.` / `-` / `/` / `//`) flushed when a clip stops

These are not committed-meaningful build artifacts. They exist to make the GUI runnable without an extra build step on the developer machine.
