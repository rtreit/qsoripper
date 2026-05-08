# ARRL CW Corpus — Quality Report (Index-Driven Pipeline)

Auto-generated from `data/cw-samples/arrl-archive/manifest.jsonl` and the per-session `*.trim.json` sidecars by the index-driven parallel harvester.

## Pipeline Performance

- **Total wall time:** 371.6 s (6.2 min)

| Stage | Duration (s) | Notes |
|:------|------------:|:------|
| build_index | 8.4 |  |
| download | 180.0 | workers=8, limit=50/speed |
| trim | 11.3 | workers=8 |
| align | 170.9 | workers=8 |
| manifest | 1.0 |  |

- Sessions in pilot: 200
- Chunks produced: 1576
- Download throughput: 66.7 files/min
- Alignment throughput: 70.2 sessions/min

## Summary

- Speeds covered: [20.0, 25.0, 30.0]
- Sessions trimmed: 200
- Chunks retained: **1576**
- Total labeled audio: **17.84 h** (64218 s)
- Total labeled characters: 154,014

## Per-Speed Breakdown

| WPM | Sessions trimmed | Trimmed audio (h) | Chunks | Audio kept (h) | Median align CER | p95 align CER |
|----:|-----------------:|------------------:|-------:|---------------:|-----------------:|--------------:|
| 15.0 | 50 | 5.66 | 0 | 0.00 | nan | nan |
| 20.0 | 50 | 6.42 | 426 | 6.15 | 0.0159 | 0.0315 |
| 25.0 | 50 | 5.95 | 515 | 5.76 | 0.0137 | 0.0319 |
| 30.0 | 50 | 6.01 | 635 | 5.93 | 0.0143 | 0.0312 |

## Chunk Duration Distribution

- min=7.85s, median=39.05s, max=85.89s

| Bucket | Count |
|:-------|------:|
| <=5s | 0 |
| 5-10s | 1 |
| 10-15s | 8 |
| 15-30s | 356 |
| 30-60s | 1065 |
| 60-90s | 146 |
| >90s | 0 |

## Per-Chunk Alignment Score Distribution

- median=0.0146, p95=0.0312, max retained=0.0500
- (drop threshold: 0.05; chunks above were filtered out by `align_parallel.py`)

## Spot Checks (8 random chunks)

| WPM | Date | Duration | Align CER | First 80 chars of truth |
|----:|:-----|---------:|----------:|:------------------------|
| 30.0 | 2024-10-15 | 27.7s | 0.0250 | `WORSE, THIS UNIT DID NOT MEET THE FCC LIMITS FOR SPURIOUS EMISSIONS ON 6 METERS.` |
| 20.0 | 2024-10-01 | 48.1s | 0.0217 | `INSTALLATION TAKES ONLY A FEW MINUTES THE BACK OF THE ENCLOSURE HAS FOUR JACKS S` |
| 20.0 | 2023-11-14 | 58.5s | 0.0175 | `UNFORTUNATELY, THE SPONGE IS HELD IN PLACE ONLY BY FRICTION, SO IT TENDS TO FALL` |
| 30.0 | 2025-08-19 | 34.9s | 0.0000 | `A BRACKET FOR THE FACEPLATE CONTROLLER, A 10 FOOT CONTROL CABLE, A USB CABLE, AN` |
| 25.0 | 2024-03-19 | 42.6s | 0.0098 | `THINGIVERSE. COM. FIGURES 1, 2, 3, AND 4 ARE EXAMPLES OF OBJECTS IVE FOUND ON TH` |
| 25.0 | 2023-12-26 | 31.3s | 0.0130 | `RECEIVER SENSITIVITY WAS SPECIFIED AT 0R35 V FOR 20 DB SIGNAL TO NOISE RATIO.` |
| 25.0 | 2023-10-31 | 51.9s | 0.0079 | `CONCLUSION THE OPERATION OF THE DVMEGA CAST IS QUITE SIMPLE THANKS TO THE TOUCHS` |
| 20.0 | 2025-02-18 | 42.8s | 0.0238 | `IF YOU RUN THE OUTPUT THROUGH AN ADDITIONAL DSP = END OF 20 WPM TEXT = QST DE W1` |

## Trim / Intro Detection

- Intro stripped on 0/200 files (avg 0.0 s removed)
- Outro stripped on 16/200 files (avg 0.8 s removed)

## Known Limitations

- **Studio-clean source.** ARRL bulletins are recorded directly from a code generator; no QSB, no QRM, no fading, single oscillator pitch (~700 Hz). A model trained only on this corpus will be brittle on real on-air audio. Use it as a pretraining seed and add on-the-fly augmentation (additive noise, SNR randomization, pitch shifts ±300 Hz, time-warp ±10 %, fading, QSB, key-clicks).
- **Bulletin vocabulary.** Texts are pulled from QST articles, ARRL announcements and callsign drills. Ham vocabulary is well represented; conversational English / contest exchanges are not.
- **One WPM per file.** Mixed-speed bursts (typical of real QSOs) are not represented.
- **Uniform char-rate alignment.** We assume constant CW rate within a session; small tempo wobble or operator pauses can shift chunk boundaries by 0.3–0.8 s. The drop threshold (alignment CER > 0.05) catches the worst cases.
- **Bi-weekly cadence.** The ARRL archive has been bi-weekly (every 2 weeks) since at least 2014. The index parser pulls every available date in one HTTP request per speed, eliminating the prior pipeline's blind date probing.

## Recommended Use

- **Pretraining only.** Use this corpus to bootstrap a CTC / transducer encoder.
- **Always augment.** Suggested chain: random gain → additive band-limited noise to reach SNR ∈ [-3, +20] dB → ±300 Hz pitch shift → random 5–10 % time stretch → occasional QSB envelope (0.3–1.5 Hz). Keep the truth label unchanged.
- **Validation set.** Reserve ≥ 1 full session per speed (hold out by date) so you never evaluate on a chunk whose neighbours were trained on.
- **Mix with augmented synthetic.** Combine 70 % ARRL chunks with 30 % synthetic CW from `gen-rough-fist` / `gen-qso-suite` to add fist variability and prosigns.

## Scaling Estimate

ARRL archive coverage observed during pilot index build:

- ~285 sessions per speed × 11 speeds available = ~3 100 candidate sessions.
- Median trimmed length ~9 min ≈ ~470 hours of clean labeled CW at full coverage.
- At pilot's chunks-per-session density, that's ~30 000–40 000 labeled chunks.
- Storage at 8 kHz int16: ~17 GB raw WAV. MP3 source ~35 GB.
