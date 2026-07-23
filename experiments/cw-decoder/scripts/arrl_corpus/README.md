# ARRL CW Corpus - Index-Driven Parallel Harvester

Fetch the ARRL Code Practice archive.
Then trim, align, and divide it into a labeled CW training corpus.
The archive contains W1AW bulletins.
The process is **fast** and **resumable**.
One HTTP request for each speed builds the complete session index.
Then download, trim, and align operations use all available cores.

## Pilot result

The pilot used four speeds and the 50 newest sessions for each speed.
It processed 200 sessions in **6.2 minutes** on a Windows workstation.
The process used eight download workers and eight alignment workers.
It produced 1,576 chunks and approximately 17.8 hours of clean labeled CW.
See `quality_report.md` and `../../../EXPERIMENT_REPORT.md`.

## Prerequisites

- Python 3.11+ with `aiohttp`, `numpy` (and optionally `tqdm`).
  Install: `py -3 -m pip install aiohttp numpy`
- `ffmpeg` on PATH (for MP3 → 8 kHz mono PCM decode).
- `cw-decoder` Rust binary built in release:
  `cargo build --release --bin cw-decoder` from `experiments/cw-decoder/`.

## Run

From the worktree root (paths are anchored relative to the repo so any cwd
works):

```
py -3 experiments/cw-decoder/scripts/arrl_corpus/run.py \
    --speeds 15,20,25,30 --limit-per-speed 50
```

Common flags:

| Flag | Default | Meaning |
|:-----|:-------:|:--------|
| `--speeds`            | `15,20,25,30`        | Comma-separated WPM list |
| `--limit-per-speed`   | `50`                 | Newest N sessions per speed |
| `--workers`           | `8`                  | Concurrent HTTP downloads (per-host cap) |
| `--align-workers`     | `os.cpu_count()`     | Process pool size for trim+align |
| `--skip-stages`       | _empty_              | for example `0,1` to skip rebuild & redownload |

Stage indices: `0=build_index 1=download 2=trim 3=align 4=manifest 5=report`.

## Pipeline stages

| Stage | Module | What it does | Output |
|:-----:|:-------|:-------------|:-------|
| 0 | `build_index.py`     | One GET per speed → parses anchor tags → emits `index.jsonl` of every (wpm,date,mp3,txt) tuple. **No date-probing.** | `data/cw-samples/arrl-archive/index.jsonl` |
| 1 | `download_parallel.py` | aiohttp + `Semaphore(workers)`. Validates content-type (rejects the ARRL CDN GIF error page). Per-file `*.dl.json` sidecar. Polite. | `data/cw-samples/arrl-archive/{wpm}wpm/raw/{YYMMDD}.{mp3,txt}` |
| 2 | `trim_parallel.py`   | ProcessPool. Ffmpeg → 8 kHz mono → Goertzel @ 700 Hz → strip intro/outro silence. | `…/trimmed/{YYMMDD}.wav` + `.trim.json` |
| 3 | `align_parallel.py`  | ProcessPool spawning N `cw-decoder.exe stream-region` subprocesses. Levenshtein-anchored alignment vs truth → sentence-bounded chunks. Drops chunks with align CER > 0.05 and whole files with CER > 0.50. | `…/chunks/{YYMMDD}_{seq:04d}.wav` + `.chunks.jsonl` |
| 4 | `manifest.py`        | Aggregates all `*.chunks.jsonl` → master manifest + 20-row committed sample. | `…/manifest.jsonl`, `sample_manifest.jsonl` |
| 5 | `report.py`          | Per-speed coverage, duration histogram, alignment score distribution, spot checks, pipeline timings. | `quality_report.md` |

All stages are **idempotent** - re-running with the same args is a no-op for
already-completed work (downloads check size, trim/align check sidecar / chunks
JSONL).

## Manifest schema (downstream consumers)

`data/cw-samples/arrl-archive/manifest.jsonl` is one JSON object per chunk:

```json
{
  "wav_path": "data/cw-samples/arrl-archive/20wpm/chunks/250624_0003.wav",
  "text": "WHEN THE DECODER LOSES LOCK DUE TO A FREQUENCY CHANGE, ...",
  "wpm": 20.0,
  "date": "2025-06-24",
  "alignment_score": 0.0068,
  "duration_s": 76.316,
  "sample_rate": 8000,
  "char_count": 147,
  "source_file": "250624.mp3"
}
```

- `wav_path` is forward-slash relative to the repo root.
- `alignment_score` is per-chunk Levenshtein-anchored CER between the truth
  text and the decoder's transcript at the same proportional position. Lower
  is better. Chunks > 0.05 are dropped before manifest writeout.
- `duration_s` is the actual sliced WAV duration (mono int16, 8 kHz).

## Scaling guidance

- **Wider speed coverage:** add `5,7.5,10,13,18,35,40` to `--speeds`. The
  current `cw-decoder` model has trouble locking at 15 WPM (whole-file CER
  ~0.30 → 0 chunks retained). `20+ WPM` works cleanly. The ARRL archive index
  parser handles all 11 official speeds. The URL slug for fractional speeds
  is `7pt5` (for example `https://www.arrl.org/7pt5-wpm-code-archive`).
- **Full archive:** drop `--limit-per-speed`. Each speed has ~285 sessions
  going back to 2013-2014 (bi-weekly cadence). Total at 8 speeds with full
  alignment ≈ ~3 000 sessions, ~470 audio hours, ~30 000+ chunks.
- **Throughput:** download stage is bandwidth-bound at ~1.1 files/s/host
  (deliberately polite). Alignment stage is CPU-bound, ~1.2 sessions/s on 8
  cores. To go faster on a beefier box bump `--align-workers`. Do **not**
  bump `--workers` past 8. ARRL is a non-profit hosting public files.
- **Resumability:** kill at any point and re-run. Finished work is skipped.
  To force a refetch, delete the per-file `*.dl.json` sidecar (or the file
  itself).

## Why the index-driven approach

The prior pipeline (`u/randy/cw-exp-arrl-corpus`) blindly probed every
Mon/Wed/Fri date in 2024-2026 with a HEAD request, then a GET on hits. That
took 80+ minutes for **2 sessions**. ARRL bulletins are actually **bi-weekly**
and every available file is listed in plain HTML on the per-speed archive
page. One HTTP request per speed (≤11 total) replaces ~9 000 blind probes.

## Files

```
arrl_corpus/
├── README.md             ← you are here
├── common.py             ← paths, normalization, helpers
├── build_index.py        ← Stage 0
├── download_parallel.py  ← Stage 1
├── trim_parallel.py      ← Stage 2
├── align_parallel.py     ← Stage 3
├── manifest.py           ← Stage 4
├── report.py             ← Stage 5
├── run.py                ← orchestrator (recommended entry point)
├── quality_report.md     ← committed report from latest pilot run
└── sample_manifest.jsonl ← 20 representative chunks (committed)
```
