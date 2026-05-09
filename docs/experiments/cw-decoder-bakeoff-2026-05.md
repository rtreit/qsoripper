# CW Decoder Bake-Off — May 2026

This document captures a multi-round parallel experiment to fix two specific
failure modes in the CW decoder observed on real over-the-air captures:

1. **Ghost character cascades** — when SNR drops within an otherwise-decodable
   region, the decoder emits a stream of phantom `E`/`T`/`I`/`M` characters
   instead of dropping output.
2. **Word-segmentation errors** — leading characters of repeated callsigns
   (e.g. `WA` of `WA6MOW`) silently disappear before reaching the decoder's
   gap-classification stage.

Eight algorithmic approaches were implemented in parallel git worktrees and
benchmarked against the same six real OTA samples. A ninth track built an
auto-labeled training corpus from ARRL Code Practice broadcasts to unblock
future neural / statistical-LM work.

This file is the canonical write-up so the data and conclusions are not lost.

---

## Test harness

All experiments were measured by the same `bench.py` script invoking
`cw-decoder.exe stream-region --file <mp3> --json --no-realtime` and parsing
the final `transcript`/`end` event. Each transcript was normalized via
`re.sub(r"[^A-Z0-9]+", " ", s.upper())` before comparison to per-sample truth.

Metrics:

- **CER** — character error rate (Levenshtein distance / max-length)
- **WER** — word error rate
- Token recall / precision

Six samples from `data/cw-samples/training-set-a/`:

| Sample | Truth (excerpt) | Notes |
|---|---|---|
| `cq-pota-aa6pw` | `NQ POTA AA6PW AA6PW CQPOTA AA6PW...` | Headline ghost target — heavy E/T/I cascades |
| `cq-sota-wa7ben` | CQ SOTA call from WA7BEN | Standard pileup CQ |
| `wa6mow` | WA6MOW callsign repeats | Word-segmentation target — `WA` drops |
| 3 additional CQ/exchange samples | mixed | Baseline coverage |

Mean CER is the unweighted average across the six samples.

---

## Round 1 — initial algorithmic diversity

Four orthogonal approaches launched in parallel.

### viterbi — `u/randy/cw-exp-viterbi` @ `f16c651` 🥇 WINNER

**Hypothesis:** Hard length thresholding (dit < X ms, dah ≥ X ms) is too
brittle when SNR varies inside a region.

**Implementation** (`vendor/ditdah/src/decoder.rs`):

1. `merge_short_blips` — coalesce sub-threshold blips before classification.
2. `soft_decode_intervals` — Gaussian per-class log-probability over (dit, dah)
   lengths given current WPM estimate.
3. Argmax over the soft scores instead of hard threshold.

**Results:**

| Metric | Baseline | Viterbi | Δ |
|---|---:|---:|---:|
| Mean CER | 0.234 | **0.202** | **−13.7%** |
| `cq-pota-aa6pw` CER | 0.321 | **0.167** | **−48%** |
| `wa6mow` WER | 0.600 | 0.600 | 0 |

**Why it won:** The soft Gaussian gives the decoder graceful degradation when
an element is borderline length, instead of flipping a coin at the threshold.

**Why `wa6mow` did not move:** the `WA` problem is upstream of length
classification — elements are not detected at all, so no classifier can fix
it. See gap-gmm / next-round notes.

### charlm — `u/randy/cw-exp-charlm` @ `b15ed0c` — mixed, partially salvageable

**Hypothesis:** A character-level n-gram language model can rescore ambiguous
decodes using English / callsign-prefix frequency.

**Implementation:** 4-gram + callsign-prefix LM scored against decoder output.
A mid-edit syntax error (orphan `];` debris from an incomplete edit) caused 13
cascading "unknown prefix" diagnostics; cleaned up by the orchestrator.

**Results:** Mean CER worse on most samples — the English LM over-confidently
"corrects" rare callsigns toward English text.

**Salvageable win:** **`wa6mow` recall 0.67 → 1.00**. The callsign-prefix
component genuinely helps when the substring contains a known prefix pattern.

**Action:** cherry-pick the callsign-join logic only; discard English LM
rescoring.

### snr-gate — `u/randy/cw-exp-snr-gate` @ `9b07ad5` — honest negative

**Hypothesis:** Suppress decoding entirely in low-SNR regions.

**Implementation:** SNR estimator + threshold gate on the region path.
Default-off plumbing landed correctly.

**Result:** No CER improvement at any tested threshold (kills weak-but-correct
signals). Plumbing kept because it is reusable for elem-gate and future
experiments.

### ctc-neural — `u/randy/cw-exp-ctc-neural` @ `04d7456` — research artifact

**Hypothesis:** End-to-end CNN + CTC trained on synthetic CW.

**Implementation (Python / PyTorch):** 1D-CNN encoder, CTC loss, beam search
decode. ~50 hours of synthetic training data.

**Results:**

| Metric | Value |
|---|---:|
| Mean CER | **0.917** (4× worse than baseline) |
| `cq-pota-aa6pw` CER | **0.202** ← validates noise-region hypothesis |

**What went wrong:**

1. **Blank-collapse trap** required four fixes (head bias init
   `head.bias[BLANK] = -3.0`, pitch-invariant pooling, high-SNR curriculum
   warmup, `GroupNorm` not `BatchNorm`) just to train.
2. Synthetic training data did not generalize to real OTA — synthetic too
   clean.
3. Tiny model + short clips + no LM thrashes on calls it has never seen.

**Verdict:** Not shippable as a decoder. It did, however, beat the baseline on
`cq-pota-aa6pw` (0.202) without any morse-domain heuristic, proving "noise
regions are decodable if you stop forcing them through hard classifiers." That
result motivated the ARRL corpus track.

---

## Round 2 — targeted mechanism stacking

### elem-gate — `u/randy/cw-exp-elem-gate` @ `2b36af5` 🥈 ties Viterbi, orthogonal mechanism

**Hypothesis:** Per-element SNR gate (different from region-level snr-gate).

**Implementation:** Compute SNR for each detected element; drop elements below
a configurable dB floor. Env var `DITDAH_ELEM_GATE_DB` (default 12 dB).

**Results:**

| Metric | Baseline | Elem-gate |
|---|---:|---:|
| Mean CER | 0.234 | 0.208 |
| `cq-pota-aa6pw` CER | 0.321 | **0.167** |
| `wa6mow` WER | 0.600 | 0.600 |

**Key insight:** reaches the same `cq-pota-aa6pw` CER as Viterbi via a
completely different mechanism (gate vs. soft classification). They should
**stack** — Viterbi handles borderline lengths, elem-gate kills phantom
elements before they reach the classifier.

### bigram-viterbi — `u/randy/cw-exp-bigram-viterbi` @ `1eea2ed` — no-op pending corpus

**Hypothesis:** Replace argmax with full Viterbi over character bigrams.

**Result:** No-op at safe weights — bigrams trained on a small synthetic
corpus contained no real signal. **Now unblocked** by the 154k-character ARRL
corpus from `arrl-corpus-fast`.

### gap-gmm — `u/randy/cw-exp-gap-gmm` @ `2bd6e19` — falsified hypothesis

**Hypothesis:** Inter-element gaps follow a 3-component GMM (intra-char,
inter-char, inter-word); EM fit gives better word boundaries than fixed
thresholds.

**Result:** No improvement on `wa6mow`. Diagnostic insight: the `WA` of
`WA6MOW` drops out at the element-detection stage, **before** any gap
classifier runs. The bug is upstream in onset detection / Goertzel /
smoothed-power.

**Value:** killed a wrong hypothesis cheaply. Future word-segmentation work
must target onset detection, not gap classification.

### ensemble — `u/randy/cw-exp-ensemble` @ `050991d` — infrastructure win, voting inconclusive

**Hypothesis:** Vote across multiple decoder variants per region.

**Implementation:** Build-time vendoring of three backends (`vendor/ditdah` +
`vendor/ditdah-viterbi` + `charlm`) with per-thread `BackendGuard` RAII
isolation. New `stream-region-ensemble --variant {baseline,viterbi,charlm,majority,anchor}`
subcommand.

**Results:**

| Variant | Mean CER | Notable |
|---|---:|---|
| baseline | 0.234 | reference |
| viterbi | 0.202 | matches Round 1 |
| charlm | mixed | matches Round 1 |
| majority | 0.224 | naive vote loses to Viterbi alone |
| **anchor** | 0.219 | best `wa6mow` WER **0.400** (vs 0.600 baseline) |

**Verdict:** voting alone does not beat the strongest single decoder. The
multi-backend infrastructure is, however, the only pre-built path to add a
retrained ctc-neural decoder as a fourth voter.

---

## Round 3 — ARRL auto-labeled corpus

### arrl-corpus-fast — `u/randy/cw-exp-arrl-corpus-fast` @ `4dd5183` — pipeline shipped

**Goal:** Build an auto-labeled training corpus from ARRL Code Practice MP3s,
which ship with ground-truth text files at predictable URLs.

**Architecture (5 stages + orchestrator at `experiments/cw-decoder/scripts/arrl_corpus/`):**

```text
build_index   parse 4 HTML archive pages → 1,146 sessions discovered
download      8-worker asyncio aiohttp, content-type validation
trim          ProcessPool + ffmpeg → mono 8 kHz + Goertzel @ 700 Hz
align         ProcessPool × cw-decoder.exe stream-region + SequenceMatcher
manifest      → jsonl (1,576 chunks)
report        per-speed quality stats
```

**Pilot run results:**

| Metric | Value |
|---|---:|
| Speeds attempted | 15 / 20 / 25 / 30 WPM |
| Sessions attempted | 200 (50 per speed, newest-first) |
| Sessions yielding chunks | 148 / 200 (15 WPM all dropped) |
| **Chunks produced** | **1,576** |
| **Total labeled audio** | **17.84 h** |
| **Total characters** | **154,014** |
| Median CER (chunk) | ~0.016 |
| p95 CER (chunk) | ~0.031 |
| **Wall time** | **6 min 13 s** (~40× faster than serial baseline) |

Stage timings:

```text
build_index     8.4s    4 GETs, 1146 sessions discovered
download      180.0s    8 workers, polite
trim           11.3s    8 workers, ProcessPool
align         170.9s    8 workers, ProcessPool × cw-decoder.exe
manifest        1.0s
report          1.2s
```

**Known gap:** 15 WPM yields zero chunks — `cw-decoder` does not lock at slow
speeds in its current configuration. MP3 + truth files are intact and cached
on disk; once decoder lock-range is widened,
`run.py --skip-stages 0,1,2 --speeds 15` will harvest in seconds.

**Other speeds (5 / 7.5 / 10 / 13 / 35 / 40 WPM)** are supported by the index
parser and can be enabled with a CLI flag.

A serial pilot (`u/randy/cw-exp-arrl-corpus`) was also completed (449 chunks /
4.74 h in ~80 min) but pruned in favour of the fast version.

---

## Final scoreboard

| # | Approach | Mean CER | aa6pw CER | wa6mow WER | Status | Recommendation |
|---|---|---:|---:|---:|---|---|
| — | baseline | 0.234 | 0.321 | 0.600 | reference | — |
| 🥇 | **viterbi** | **0.202** | **0.167** | 0.600 | ready | **merge to main** |
| 🥈 | **elem-gate** | 0.208 | **0.167** | 0.600 | ready | **stack on viterbi** |
| 3 | ensemble (anchor) | 0.219 | 0.167 | **0.400** | infra | hold for ctc retrain |
| 4 | charlm | mixed | — | recall 1.00 | partial | cherry-pick callsign-join only |
| 5 | snr-gate | 0.234 | 0.321 | 0.600 | plumbing | keep, default-off |
| 6 | bigram-viterbi | no-op | — | — | unblocked | retrain on ARRL corpus |
| 7 | gap-gmm | no-op | — | — | falsified | discard, fix upstream first |
| 8 | ctc-neural | 0.917 | **0.202** | — | research | retrain on ARRL → 4th voter |

---

## Key takeaways

1. **Soft beats hard.** The single biggest CER win came from replacing one
   hard threshold with a soft Gaussian (Viterbi).
2. **Two orthogonal mechanisms reach the same ceiling.** Viterbi and elem-gate
   both hit `cq-pota-aa6pw` CER 0.167 from completely different angles —
   strong evidence they will stack.
3. **Word segmentation is upstream.** `gap-gmm` falsified the
   gap-classification hypothesis. Don't waste effort on classifier-stage
   word-gap work; fix element detection first.
4. **Synthetic neural did not generalize** but proved the hypothesis. Real
   path requires real corpus — which now exists (ARRL pipeline).
5. **Corpus throughput matters.** ~40× speedup just from index-driven
   discovery + parallelism. Same lesson likely applies to future training
   pipelines.

---

## Reproducibility

- `bench.py` is preserved at `experiments/cw-decoder/scripts/bench.py` (or in
  each experiment branch). Six fixed samples in
  `data/cw-samples/training-set-a/`.
- Each branch above has an `experiment_report.json` with raw per-sample
  numbers and an `EXPERIMENT_REPORT.md` with the implementer's narrative.
- Worktree pattern:
  `git worktree add -b u/randy/cw-exp-<name> C:\Users\randy\Git\qsoripper-experiments\<name> <base-sha>`.
  Each worktree has its own `target/` dir so parallel cargo builds do not
  collide.
- ARRL corpus pipeline is idempotent — re-running with cached state skips
  ~200 cached sessions in <2 s.

## Operational notes

- `cw-decoder` binary is `cargo build --release --bin cw-decoder` (the crate
  package name is `cw-decoder-poc`).
- `bench.py` invokes the default `stream-region` subcommand. For ensemble
  variants use `stream-region-ensemble --variant <v>`.
- Always rebuild before benchmarking — bench.py uses the pre-built
  `target/release/cw-decoder.exe` and a stale binary will mask source-level
  failures.
- `RegionStreamConfig::default()` does not match CLI defaults
  (`merge_gap_s` 3.0 vs 0.5, `min_region_s` 0.6 vs 0.3, `pad_s` 0.10 vs 0.15).
  When using region-stream APIs from a binary, mirror CLI defaults explicitly.

---

## Pointers to next round

Concrete experiments unlocked by this work:

- **bigram-viterbi (re-run)** — train transition matrix on the 154k-char ARRL
  corpus instead of synthetic data. Probably the cheapest next win.
- **ctc-neural (retrain)** — retrain on the 17.84 h ARRL corpus with a
  beam-search decoder + n-gram language model trained on the same corpus.
  Issue #409 has the full Phase 1-7 plan.
- **viterbi + elem-gate stack** — combine the two mechanisms; both hit
  `cq-pota-aa6pw` 0.167 independently, so a stacked decoder may break that
  ceiling.
- **Upstream element detection** — re-investigate onset detection / Goertzel /
  smoothed-power so `WA` of `WA6MOW` survives. Required for any further
  word-segmentation work.

See also: GitHub issue #409 (production neural decoder roadmap).
