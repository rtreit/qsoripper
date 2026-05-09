# ARRL CW Corpus — Index-Driven Parallel Harvester

**Branch:** `u/randy/cw-exp-arrl-corpus-fast`
**Predecessor:** `u/randy/cw-exp-arrl-corpus` (serial pipeline, replaced by this work)

## TL;DR

Replaced the prior agent's blind date-probing serial pipeline with an
**index-driven parallel harvester**. Result: **6.2 minutes wall time** for
**200 sessions × 4 speeds = 1576 labeled chunks / 17.8 hours of clean CW
audio**, vs the prior pipeline's 80+ minutes for 2 sessions.

Speedup: roughly **600× chunks/minute throughput**.

## Pipeline overview

Five stages, each idempotent and resumable. See
`experiments/cw-decoder/scripts/arrl_corpus/README.md` for full per-stage
documentation and CLI flags.

```
Stage 0  build_index           1 HTTP GET per speed → index.jsonl  (~8s for 4 speeds)
Stage 1  download_parallel     aiohttp + Semaphore(8)              (3 min for 200 files × 2)
Stage 2  trim_parallel         ProcessPool, ffmpeg + Goertzel      (11 s for 200 files)
Stage 3  align_parallel        ProcessPool, N × cw-decoder.exe     (2.85 min for 200)
Stage 4  manifest              concat per-session JSONL            (1 s)
Stage 5  report                quality_report.md w/ perf section   (1 s)
```

The big architectural win is Stage 0. ARRL bulletins are posted bi-weekly
and every available MP3 + truth file is listed in plain HTML on the per-speed
archive page (`https://www.arrl.org/{N}-wpm-code-archive`). Parsing that HTML
once gives the full ground-truth session index per speed, eliminating ~9000
blind HEAD requests the prior agent made.

## Comparison vs prior `arrl-corpus` serial pipeline

| Metric | Prior (serial, blind probe) | This (index-driven, parallel) |
|:------|:---------------------------:|:-----------------------------:|
| Sessions completed in pilot run | 2 | 200 |
| Wall time | 80+ min | **6.2 min** |
| Chunks produced | 17 | **1576** |
| Audio hours labeled | 0.25 h | **17.84 h** |
| Per-session avg time | ~40 min | **~1.9 s** |
| HTTP requests for discovery | ~9000 HEADs | 4 GETs |
| Discovery method | guess every Mon/Wed/Fri date | parse archive HTML |
| Concurrency | none (single requests session) | 8 download / 8 align |

## Pilot run output

```
================ PIPELINE SUMMARY ================
  build_index       8.4s
  download        180.0s   workers=8, limit=50/speed
  trim             11.3s   workers=8
  align           170.9s   workers=8
  manifest          1.0s
  report            1.2s
  TOTAL           372.8s
  Sessions:      200
  Chunks:        1576
==================================================
```

Per-speed breakdown (from `quality_report.md`):

| WPM | Sessions | Trimmed (h) | Chunks | Audio kept (h) | Median align CER |
|---:|---:|---:|---:|---:|---:|
| 15 | 50 | ~7.5 | **0** | 0.00 | n/a — see "limitations" |
| 20 | 50 | ~8.0 | ~450 | ~5.4 | ~0.0023 |
| 25 | 50 | ~8.4 | ~530 | ~6.3 | ~0.0024 |
| 30 | 50 | ~8.6 | ~596 | ~6.1 | ~0.0017 |

(Exact numbers in the committed `quality_report.md`.)

## Honest assessment

**What worked well**

- Index parsing is robust: 858–1146 sessions discovered across the four pilot
  speeds (the ARRL site uses both server-relative `/files/...` hrefs and
  fully-qualified `http://www.arrl.org/files/...` hrefs depending on the
  speed page; the regex tolerates both).
- Parallel download saturates the polite 8-connection cap at ~1.1 files/s.
- Alignment scales near-linearly with `align-workers`. On 8 cores, 200
  sessions align in ~3 minutes.
- Resumability is real: re-running `run.py` after a kill skipped all 200
  cached entries in <2 seconds total.

**What didn't work / known issues**

- **15 WPM yields zero chunks.** All 50 sessions hit the
  whole-file-CER drop threshold (CER ~0.30). The current `cw-decoder` Rust
  binary doesn't lock onto the slower keying — the decoded transcript is
  garbled and alignment fails. Audio + truth are fine; this is purely a
  decoder-side capability gap. The pipeline correctly classifies these as
  `whole-file-poor` / `no-chunks` and continues. **Recommendation:** tune the
  decoder's WPM detection range or feed 15 WPM through a future model
  pretrained on the 20/25/30 corpus.
- 5/7.5/10/13 WPM not exercised in this pilot. The prior agent reported the
  archive has many error-page entries at those speeds; opt in via
  `--speeds 5,7.5,10,13,15,18,20,25,30,35,40` once a slower-tolerant decoder
  is available.
- ARRL CDN occasionally serves an image/* response for missing files. The
  downloader detects this (`error_page` status in `*.dl.json`) and continues
  rather than writing a corrupt file. None observed in this pilot.

## Recommendations

1. **Use this corpus for pretraining only** — studio-clean source, single
   pitch (~700 Hz), bulletin vocabulary. Always combine with augmentation
   (additive noise, pitch jitter, QSB) for any model intended for on-air use.
2. **Hold out by date for validation** — entire sessions are highly
   correlated within themselves; never split a single session across
   train/eval.
3. **Scale to full archive** by dropping `--limit-per-speed`. With the index
   parser, a full ~3000-session run is bandwidth-bound (bytes from arrl.org)
   and would take ~45–60 minutes wall time on the same hardware.
4. **Fix 15 WPM (decoder side).** The 50 downloaded 15 WPM sessions are now
   cached on disk; once the decoder lock range is widened, just re-run
   `run.py --skip-stages 0,1,2 --speeds 15` and the alignment stage will pick
   them up automatically.
5. **Add 18 WPM** — it's between two known-good speeds and will likely
   produce another ~600 chunks for cheap.

## File layout

- Pipeline code: `experiments/cw-decoder/scripts/arrl_corpus/`
- Generated raw + trimmed + chunked WAVs: `data/cw-samples/arrl-archive/`
  (gitignored — kept locally; ~5 GB after pilot run)
- Committed artifacts:
  - `experiments/cw-decoder/scripts/arrl_corpus/sample_manifest.jsonl` (20 chunks)
  - `experiments/cw-decoder/scripts/arrl_corpus/quality_report.md`
  - this file (`EXPERIMENT_REPORT.md`)
