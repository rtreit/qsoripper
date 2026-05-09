# Kaggle Morse Learning Machine Challenge v2 — external benchmark

This pipeline ingests the Kaggle [Morse Learning Machine Challenge v2][kaggle]
dataset and uses it as a **third external benchmark** alongside `training-set-a`
(real OTA) and the adversarial synthetic suite (`bench_adversarial.py`).

[kaggle]: https://www.kaggle.com/competitions/morse-learning-machine-challenge-v2

Tracks GitHub issue **#424**.

## Why

1. **External metric.** Public, third-party CER number scored on data we did not
   pick. Defends against silently overfitting `training-set-a`.
2. **Distribution coverage.** Kaggle randomizes per-file SNR (−14 to +20 dB),
   pitch (600–1200 Hz), and speed (12–80 WPM). Our current OTA bench tops out
   around 40 WPM, so the high-WPM tail (50–80) stresses regions we never see.
3. **Augmenter cross-check.** The `augment_arrl` synthesizer aims at the same
   randomization envelope. If augmented-corpus statistics don't bracket
   Kaggle's, the augmenter is mis-tuned.

## Dataset facts

- 200 WAV files, mono, 32-bit float, 8 kHz
- File naming: `cw001.wav` … `cw200.wav`
- ~100 labeled (training); ~100 held out (validation, scored via leaderboard)
- Per-file randomization: SNR ∈ [−14, +20] dB, pitch ∈ [600, 1200] Hz,
  speed ∈ [12, 80] WPM
- Scoring metric: Levenshtein distance == our CER

## What this pipeline does

1. **Download** — `download.py` invokes `kaggle competitions download` and
   extracts under `data/cw-samples/kaggle-morse-v2/` (gitignored).
2. **Manifest** — `build_manifest.py` reads `SampleSubmission.csv` and the
   training labels, producing `manifest.jsonl` rows of `{id, wav, truth, split}`.
3. **Bench** — `bench.py` runs `cw-decoder.exe stream-region --json --no-realtime`
   on each WAV, computes per-file CER, buckets by *estimated* WPM and pitch
   (extracted from the decoder's region trace), and writes a JSON report.
4. **Submission** — `submit.py` generates a leaderboard-shaped CSV from the
   held-out half and prints the path you upload to Kaggle.

The harness pattern matches `bench_adversarial.py` (PR #417) for consistency.

## Authentication

Two paths are supported.

### Recommended: KAGGLE_API_TOKEN in .env (no kaggle.json needed)

1. Visit <https://www.kaggle.com/settings/account> and click **Create New
   Token** (the new "API Token" type that begins with `KGAT...`).
2. Add to the repo's `.env` file:

   ```text
   KAGGLE_API_TOKEN=KGAT...your-token...
   ```

3. **Accept the competition rules** (one-time, browser-only):
   <https://www.kaggle.com/competitions/morse-learning-machine-challenge-v2/rules> →
   click "I Understand and Accept". Without this step, every download endpoint
   returns 403 Forbidden — Kaggle does not expose a programmatic
   rules-acceptance API.

`download.py` reads `.env` automatically (no `python-dotenv` dependency) and
calls the Kaggle REST API directly. No `kaggle` CLI required.

### Fallback: classic kaggle.json

1. Same Create New Token page (download `kaggle.json`).
2. Place at `%USERPROFILE%\.kaggle\kaggle.json` (Windows) or
   `~/.kaggle/kaggle.json` (Linux/macOS).
3. `chmod 600 ~/.kaggle/kaggle.json` on Linux/macOS.
4. Accept the competition rules as above.
5. `pip install kaggle`.

`download.py` falls back to invoking `kaggle competitions download` when
`KAGGLE_API_TOKEN` is not set.

## Usage

```powershell
# Once: download + extract (~150 MB)
python experiments\cw-decoder\scripts\kaggle_morse_v2\download.py

# Build manifest from extracted files + SampleSubmission.csv
python experiments\cw-decoder\scripts\kaggle_morse_v2\build_manifest.py

# Build the decoder once (release)
cargo build --manifest-path experiments\cw-decoder\Cargo.toml --release --bin cw-decoder

# Run the benchmark (labeled split only, scored vs. ground truth)
python experiments\cw-decoder\scripts\kaggle_morse_v2\bench.py --label baseline-2026-05

# Generate Kaggle submission CSV for the held-out split
python experiments\cw-decoder\scripts\kaggle_morse_v2\submit.py
```

## Recovering low-SNR / off-band empties

The default region detector requires a tonal-prominence ratio of 8.0 to mark a
burst as CW (rejects white-noise / static). On the Kaggle test set this rejects
~16% of files outright and they submit as empty strings.

Set `DITDAH_MIN_TONAL_PROMINENCE_RATIO=3.0` before invoking the decoder to
recover most of those without regressing the noise-rejection synthetic suites
or training-set-a:

```powershell
$env:DITDAH_MIN_TONAL_PROMINENCE_RATIO = "3.0"
python experiments\cw-decoder\scripts\kaggle_morse_v2\submit.py
Remove-Item env:\DITDAH_MIN_TONAL_PROMINENCE_RATIO
```

Measured impact on the 200-file held-out split:

| Setting | Empties | Public LB | Private LB |
| --- | ---: | ---: | ---: |
| baseline (8.0) | 32 / 200 | 34.99 | 34.18 |
| `=3.0`         | 15 / 200 | 31.56 | 30.63 |

## Smoke test (no Kaggle account needed)

If you want to verify the harness without registering for Kaggle, the bench
script accepts `--manifest <path>` so you can point at a synthetic mini-suite
that uses the same WAV format + SNR/WPM/pitch envelope:

```powershell
python experiments\cw-decoder\scripts\kaggle_morse_v2\generate_synthetic_minisuite.py
python experiments\cw-decoder\scripts\kaggle_morse_v2\bench.py `
  --manifest experiments\cw-decoder\scripts\kaggle_morse_v2\synthetic_minisuite_manifest.jsonl `
  --out experiments\cw-decoder\kaggle_minisuite_report.json `
  --label minisuite-smoke
```

## Output

- `manifest.jsonl` — `{id, wav, truth, split, ...}` rows
- `<out>.json` — per-file CER plus aggregate stats and SNR/WPM/pitch buckets
- `<out>_submission.csv` — Kaggle-format predictions for the held-out split

## Limitations

- This is **not** a training corpus. ~100 labeled files is too small.
- This is **not** real-world. Synthetic CW + AWGN, no Watterson, no QRM, no fist
  variation. Beating Kaggle is necessary but not sufficient for OTA performance.
- SNR/WPM/pitch labels are per-synthesis-parameter and are not in the published
  truth file; the bench buckets by *post-hoc decoder estimates*. Expect noise.
