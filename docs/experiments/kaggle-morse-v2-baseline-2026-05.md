# Kaggle Morse Learning Machine Challenge v2 - Baseline (2026-05-09)

First QsoRipper CW decoder submission to the public Kaggle leaderboard for the
[Morse Learning Machine Challenge v2](https://www.kaggle.com/competitions/morse-learning-machine-challenge-v2).

## Headline numbers

| Metric | Score |
|---|---|
| Public leaderboard (mean Levenshtein) | **34.99** |
| Private leaderboard (mean Levenshtein) | **34.18** |
| Files predicted | 200 / 200 |
| Files with non-empty prediction | 159 / 200 (79.5%) |
| Files where decoder produced nothing | 41 / 200 (20.5%) |
| Mean prediction length (chars) | 38.0 |
| Submission ID | 52489975 |

Lower mean Levenshtein distance is better. There is no published golden
baseline for the v2 dataset yet. This number anchors all future improvements.

## What was decoded

The competition ships 200 unlabeled WAVs (`cw001.wav` … `cw200.wav`) and asks
for one transcript per file. There is **no training set** with truth - scoring
is leaderboard-only against the hidden ground truth.

Decoder used: `experiments/cw-decoder/target/release/cw-decoder.exe stream-region --file <wav> --json --no-realtime`.
Pipeline: download → manifest → decode-all → upload via Kaggle CLI.

## Failure-mode shape (qualitative)

Spot-checking the decoded outputs already shows the same failure classes the
synthetic adversarial suite + bake-off identified:

- **Empty transcripts (20.5%)**: decoder produced nothing. Either the file is
  below the region detector's tonal-prominence floor, off the pitch search
  band, or sub-region durations were rejected by `min_region_s`. Examples:
  cw187, cw189, cw190.
- **Ghost/noise expansions**: short bursts decoded as long noisy `E/T/I/M`
  cascades (for example cw197 `'O1T 0MD9 94WWWO B6 Z'`).
- **Leading-character drops** (the WA6MOW class from training-set-a): clean
  callsigns recovered with one or two leading letters chopped, for example cw185
  `'EUXILIARN'` (probably "AUXILIARY").
- **Strong long-form CW recovered well**: cw194, cw195, cw196, and cw198 each
  produce more than 50 readable characters. The steady-state engine is healthy.
  Most loss occurs at signal edges and in weak files.

The 20.5% "no transcript at all" rate is the single biggest concrete win
available - every empty row is currently scoring `len(truth)` in Levenshtein.
A naive "always emit something" lower limit on those 41 files can improve the
public score without an engine change.

## How to reproduce

```powershell
# 1. KAGGLE_API_TOKEN in .env, plus competition rules accepted in browser
python experiments\cw-decoder\scripts\kaggle_morse_v2\download.py
python experiments\cw-decoder\scripts\kaggle_morse_v2\build_manifest.py
python experiments\cw-decoder\scripts\kaggle_morse_v2\submit.py
$env:KAGGLE_API_TOKEN = (Get-Content .env | Select-String '^KAGGLE_API_TOKEN=').ToString().Split('=',2)[1]
& "$env:LOCALAPPDATA\Programs\Python\Python313\Scripts\kaggle.exe" `
  competitions submit -c morse-learning-machine-challenge-v2 `
  -f experiments\cw-decoder\scripts\kaggle_morse_v2\submission.csv `
  -m "qsoripper baseline (region-stream, 200 files)"
```

## Next experiments to move this number

In priority order (matching the bake-off direction in
`docs/experiments/cw-decoder-bakeoff-2026-05.md`):

1. **Reduce the empty-transcript rate.**
   Lower `RegionStreamConfig::min_tonal_prominence_ratio` and `min_region_s` for the Kaggle test.
   Alternatively, add a raw-envelope estimate when the region pipeline produces no output.
   The target empty rate is less than 5 percent.
2. **Hard-stack Viterbi + elem-gate** (the recommended next ensemble move from
   PR #410's discussion). This change must correct the ghost-cascade class directly.
3. **Backward re-decode for warmup losses.** WA6MOW-class leading-character
   drops dominate the partial-callsign failures here too.
4. **Lower SNR / off-pitch sweep**. Several empty files appear to have
   off-band pitch or weak signal. Widening the pitch search range or using a
   coarser threshold on a second pass can correct them.

## Files in this baseline run

- `experiments/cw-decoder/scripts/kaggle_morse_v2/submission.csv` - the actual
  uploaded submission (200 rows, regenerable).
- `experiments/cw-decoder/scripts/kaggle_morse_v2/manifest.jsonl` - file index.
- Submission ID 52489975 on Kaggle ledger.
