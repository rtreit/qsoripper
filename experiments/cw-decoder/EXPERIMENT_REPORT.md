# CW Decoder - Adversarial Synthetic Test Suites

**Branch:** `u/randy/cw-adversarial-suite`
**Base SHA:** `bc4580e`
**Date:** 2026-05

This experiment replaces the 6-sample smoke bench with **8 synthetic
adversarial suites × 20 examples = 160 deterministic examples**, each
targeting a specific decoder failure mode.
We evaluate the current baseline `cw-decoder.exe` on every suite.
The binary was built from `bc4580e` without a source change.
We also compare it with the locally available `viterbi` branch binary.

All audio is regeneratable bit-exactly from a fixed seed
(`SEED_BASE = 0xC0DEC0DE`). WAVs are not committed. Only the
manifest + scripts.

## Files

| Path | Purpose |
|---|---|
| `experiments/cw-decoder/scripts/generate_adversarial_suites.py` | WAV / truth / manifest generator (numpy + soundfile) |
| `experiments/cw-decoder/scripts/adversarial_manifest.jsonl` | 160-row manifest (committed) |
| `experiments/cw-decoder/scripts/bench_adversarial.py` | Bench harness (CER/WER + element-level + suite-specific metrics) |
| `experiments/cw-decoder/adversarial_report.json` | Baseline run (this branch) |
| `experiments/cw-decoder/adversarial_report_viterbi.json` | Cross-comparison vs viterbi worktree binary |
| `data/cw-samples/synthetic-adversarial/<suite>/*.wav` | Regenerated locally. Gitignored |

## Suite spec

| Suite | n | Stresses | Generator notes |
|---|---|---|---|
| `weak-prefix` | 20 | First key-down events at -10 dB SNR, ramped back to clean over 600 ms | Linear amplitude ramp on the leading 600 ms of CW activity. Rest is clean + 0.05 white noise floor |
| `mid-region-collapse` | 20 | Middle of long region drops 12 dB for 2 s, then recovers | 50-ms raised-cosine edges on the gain notch. 0.04 white noise floor |
| `qrm-same-pitch` | 20 | Two stations at same pitch, secondary starts at 50% of primary, lower amplitude | Sum of two synth tracks at the same `pitch_hz`, primary 0.5 / secondary 0.35 |
| `off-pitch-start` | 20 | ±100 Hz drift over first 1 s, stable after | True linear chirp via cumsum on instantaneous frequency |
| `farnsworth-extremes` | 20 | Element WPM 30, character WPM 10 (slow Farnsworth) | Element-level dot duration uses 30 WPM. Letter/word gaps stretched to 10 WPM |
| `noise-only` | 20 | White or pink noise, no CW. Truth is empty. | 8-14 s of Voss-McCartney pink or Gaussian white at 0.08-0.15 amplitude |
| `slow-arrl-style` | 20 | Clean signals at 5 / 10 / 13 / 15 WPM (round-robin by index) | Standard timing, 0.025 white noise floor |
| `fast-contest` | 20 | 35 / 40 / 45 WPM short contest exchanges | Standard timing, 0.025 white noise floor |

All suites use the decoder-native 12 kHz sample rate and 16-bit PCM WAV. Pitch
is sampled per-example from a small set near 600 Hz. Example content draws from
20 callsigns plus CQ / exchange / abbreviation templates.

## Baseline results (this branch, `bc4580e`)

| Suite | mean CER | mean WER | el-recall | el-precision | suite-specific |
|---|---:|---:|---:|---:|---|
| `weak-prefix`         | 0.051 | 0.205 | 0.983 | 0.998 | first-1 survive 70%, first-2 30%, first-3 25%, **first-5 5%** |
| `mid-region-collapse` | 0.040 | 0.077 | 0.963 | 1.000 | 0.0 ghost chars / s of -12 dB silence |
| `qrm-same-pitch`      | 0.927 | 1.088 | 0.918 | 0.729 | - |
| `off-pitch-start`     | 0.051 | 0.164 | 0.944 | 0.998 | - |
| `farnsworth-extremes` | 0.790 | 1.352 | 0.362 | 0.650 | - |
| `noise-only`          | 0.000 | 0.000 | 1.000 | 1.000 | **fully empty 100%**, 0 ghost chars / s |
| `slow-arrl-style`     | 0.213 | 0.438 | 0.888 | 0.951 | - |
| `fast-contest`        | 0.000 | 0.000 | 1.000 | 1.000 | - |
| **Overall**           | **0.259** | **0.416** | **0.882** | **0.916** | |

Element-level alignment uses Needleman-Wunsch on the dit/dah string of each
text (gaps stripped). Element recall = matches / |truth elements|. Element
precision = matches / |hypothesis elements|.

## Cross-comparison: viterbi worktree binary

Same manifest, run against `…/viterbi/experiments/cw-decoder/target/release/cw-decoder.exe`.
Mostly identical to baseline, with **one notable regression**:

| Suite | baseline CER | viterbi CER | Δ |
|---|---:|---:|---:|
| `mid-region-collapse` | 0.040 | **0.228** | +0.19 |
| `mid-region-collapse` extra-chars-per-silence-s (mean / max) | 0.0 / 0.0 | **2.88 / 14.5** | viterbi cascades phantom chars during the drop |
| `qrm-same-pitch` | 0.927 | 0.970 | +0.04 |
| All other suites | - | - | within ±0.001 |

This is the synthetic analog of the **aa6pw cascade** failure mode (phantom
characters fanning out from a low-SNR region). The current branch's baseline
does **not** exhibit it on synthetic input. Viterbi does.

## Specific findings the brief asked for

1. **`weak-prefix` (WA6MOW analog)**: The SNR increases from -10 dB during 600 ms.
   The first character survives in **70%** of the samples.
   The first two characters survive in **30%** of the samples.
   The first three survive in **25%** of the samples.
   Only **5%** of the first five characters survive without a change.
   The decoder correctly emits the remainder of the message.

   The total CER is only 5.1 percent.
   However, the decoder consistently removes the prefix.
   This behavior matches the WA6MOW pattern.

2. **`mid-region-collapse` (aa6pw cascade analog)**: The current baseline
   produces **0.0 phantom characters per second** of -12 dB silence. The
   region-isolated decoder is well-behaved here. Cascades do not appear in
   synthetic input at -12 dB.
   The viterbi binary averages **2.88 phantom characters each second**.
   Its peak is **14.5**.
   Thus, this suite identifies the cascade regression when silence detection is not strict.

3. **`noise-only` (hallucination check)** - the decoder is clean: **100% of
   noise-only inputs produce empty output** (0 ghost chars / s, mean and
   max). Any future decoder that loses this property will be caught on the
   first run.

4. **`farnsworth-extremes`** - the worst suite by raw character recall:
   element recall drops to **0.362** (precision 0.650). The long word /
   letter gaps cause the region streamer to cut messages into fragments
   that the decoder either misreads or drops entirely. CER 0.79, WER 1.35.

5. **`qrm-same-pitch`** - CER 0.93. Element recall is high (0.92) because
   the truth's elements are physically present in the audio. Element
   precision drops (0.73) because the interferer adds extra elements that
   confuse character segmentation.

6. **`fast-contest`** - perfect (CER 0).

## Regression-gate guidance

Suites suitable as **hard regression gates** (current baseline already
passes. Any regression here is a real bug):

| Suite | Suggested gate |
|---|---|
| `noise-only` | `mean_ghost_chars_per_s == 0.0` AND `fully_empty_rate == 1.0` |
| `fast-contest` | `mean_cer == 0.0` |
| `mid-region-collapse` | `max_extra_chars_per_silence_s ≤ 0.5` (detects the viterbi regression) |
| `weak-prefix` overall | `mean_cer ≤ 0.10` |
| `off-pitch-start` | `mean_cer ≤ 0.10` |

Suites suitable as **soft regression gates** (track over time, expect
improvement, large absolute changes must block a merge):

| Suite | Current baseline | Notes |
|---|---:|---|
| `weak-prefix` first-2 survival | 0.30 | Clear improvement target |
| `weak-prefix` first-5 survival | 0.05 | The hardest WA6MOW-style target |
| `farnsworth-extremes` el-recall | 0.362 | Decoder is dropping the bulk of slow-Farnsworth letters |
| `qrm-same-pitch` el-precision | 0.729 | Decoder needs to pick a track when two collide |
| `slow-arrl-style` mean CER | 0.213 | 5 WPM examples dominate failures |

## How to reproduce

```powershell
# 1. Build the decoder once (release).
cd experiments\cw-decoder
cargo build --release

# 2. Generate the suites locally (deterministic; safe to re-run).
python experiments\cw-decoder\scripts\generate_adversarial_suites.py

# 3. Bench against the local release binary.
python experiments\cw-decoder\scripts\bench_adversarial.py

# 4. Or bench an alternate decoder (e.g. another worktree) for comparison.
python experiments\cw-decoder\scripts\bench_adversarial.py `
    --exe C:\path\to\other\cw-decoder.exe `
    --label other-branch `
    --out experiments\cw-decoder\adversarial_report_other.json
```

The 6-sample bench at the worktree root remains as a smoke test. This
adversarial suite becomes the success criterion for Round 4+ work.
