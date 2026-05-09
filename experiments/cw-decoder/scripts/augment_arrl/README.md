# ARRL CW augmentation pipeline

Generate a deterministic, multi-impairment augmentation of the pristine ARRL
Code Practice corpus (1,576 chunks, 17.84 h, median CER 1.6 %) so downstream
training experiments can target real-world HF channel conditions instead of
the unrealistically clean ARRL recordings.

## Layout

```
augment_arrl/
    __init__.py
    config.py                  # paths + sampling probabilities
    impairments.py             # Watterson channel, QSB, jitter, AGC, ...
    render.py                  # per-variant orchestrator
    run.py                     # batch pipeline (ProcessPool)
    render_on_demand.py        # CLI: render one variant deterministically
    eval_decoder.py            # CER vs SNR via the viterbi-branch decoder
    distribution_check.py      # PNG: real-OTA vs augmented histograms
    report.py                  # markdown summary
```

## Determinism

Every variant is a pure function of `(chunk_id, augment_seed)`:

```python
seed_u32 = zlib.crc32(f"{chunk_id}|{augment_seed}".encode()) & 0xFFFFFFFF
rng = numpy.random.default_rng(seed_u32)
```

`chunk_id` is `"{wpm_dir}_{wav_stem}"` (e.g. `20wpm_230905_0000`) so the
same stem in two different speed buckets yields independent variants.

## Quickstart

```powershell
# Render a single variant (writes WAV to disk + JSON metadata to stderr).
py -m augment_arrl.render_on_demand `
    --chunk-id 20wpm_230905_0000 --augment-seed 7 `
    --out variant.wav

# Batch render: first 50 chunks × 30 variants.
py -m augment_arrl.run --limit 50 --variants 30

# Decode 100 variants with the viterbi-branch decoder + bucket CER vs SNR.
py -m augment_arrl.eval_decoder --n 100

# Distribution-check plot vs real OTA bench samples.
py -m augment_arrl.distribution_check `
    --real-dir C:\Users\randy\Git\qsoripper\data\cw-samples\training-set-a

# Markdown report.
py -m augment_arrl.report
```

## Impairment summary

Group A — timing / rate

- WPM scale (always, [0.7, 1.5])
- Farnsworth ratio (20 %, [1.2, 3.0]) — gap-only stretch
- WPM drift in chunk (always, ε∈[0, 0.10], f∈[0.01, 0.1] Hz)
- Per-element LogNormal jitter (always; paddle 0.05, amateur 0.10, rough 0.20)

Group B — frequency / pitch

- Pitch shift (always, ±50 Hz from 700 Hz carrier)
- Pitch drift (30 %, ±20 Hz/min)
- VFO chirp on key-down (30 %, ±5–10 Hz)

Group C — HF channel

- Watterson 2-tap (65 %, profiles `good` / `moderate` / `poor` per ITU-R F.1487)
- QSB (55 %, f∈[0.1, 2.0] Hz, depth∈[0.2, 0.8])

Group D — noise / interference

- AWGN (always, SNR ∈ {0, 5, 10, 15, 20, 30} dB)
- Pink noise (45 %, configurable level)
- Atmospheric impulses (1 %, Poisson 0.5–3 Hz)
- QRM (30 %, partner chunk at Δpitch ∈ [-100, 100] Hz, S/QRM ∈ [-6, 6] dB)
- Receiver birdies (5 %, 1–2 tones at ±50–200 Hz)
- AGC pumping (10 %, attack 10 ms / release 200 ms)

## Output layout

```
data/cw-samples/arrl-augmented/                # gitignored
    15wpm/<chunk_id>_aug<NNN>.wav
    20wpm/<chunk_id>_aug<NNN>.wav
    25wpm/<chunk_id>_aug<NNN>.wav
    30wpm/<chunk_id>_aug<NNN>.wav

experiments/cw-decoder/scripts/
    arrl_augmented_manifest.jsonl              # full (gitignored)
    arrl_augmented_sample_manifest.jsonl       # 100-row sample (committed)
    augment_distribution_check.png             # committed
```

## Filtering downstream

```python
import json
rows = [json.loads(l) for l in open("arrl_augmented_manifest.jsonl")]

# Watterson "poor"-only training set
poor = [r for r in rows if r["watterson_profile"] == "poor"]

# Hard variants (low SNR + QRM)
hard = [r for r in rows if r["snr_db"] <= 5.0 and "qrm" in r["applied"]]

# Cleanest 30 dB AWGN-only baseline (closest to source ARRL)
clean = [r for r in rows
         if r["snr_db"] == 30.0
         and r["watterson_profile"] == "off"
         and not any(t in r["applied"] for t in ("qrm", "pink_noise", "impulse"))]
```
