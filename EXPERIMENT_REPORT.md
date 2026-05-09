# CW Decoder — Round 4: HMM with Baum-Welch on ARRL Corpus

**Branch:** `u/randy/cw-exp-hmm-bw`
**Base:** `bc4580e`
**Trained params:** `experiments/cw-decoder/data/hmm_params.json` (548 bytes)

## Hypothesis

Replacing the hand-tuned ratio thresholds in `ditdah` (`DIT_DAH_BOUNDARY=2.0`,
`LETTER_SPACE_BOUNDARY=2.0`, `WORD_SPACE_BOUNDARY=5.0` in dot-length units)
with a 5-state HMM whose emission and transition parameters are estimated
from real-world CW (the 1,576-chunk / 154 k-character ARRL corpus) via
Baum-Welch should give us a data-driven decision boundary that handles
fading, drift, and varied operator timing better than fixed ratios — and
should at minimum beat the existing `viterbi` baseline (mean CER 0.202).

## Implementation

Files added / changed:

| Path | Purpose |
| --- | --- |
| `vendor/ditdah/src/gap_hmm.rs` | 5-state HMM, Gaussian emissions on `ln(len/dot_len)`, parity-restricted transitions, JSON I/O, Baum-Welch helpers, Viterbi. ~600 LOC including tests. |
| `vendor/ditdah/src/decoder.rs` | Refactored `decode_with_params_inner` to first collect debounced runs, then either run HMM Viterbi (when `DITDAH_HMM` env var points to a params file) or fall back to the original ratio classifier. New public `extract_runs_for_training(samples, sr, pin_wpm)` exposes the front-end output for the trainer. |
| `vendor/ditdah/src/lib.rs` | Re-exports `gap_hmm`, `MorseDecoder`, `extract_runs_for_training`, `RunsResult`. |
| `src/bin/train_hmm.rs` | New `train-hmm` binary. Reads ARRL `manifest.jsonl`, runs the front-end per chunk in parallel (rayon), accumulates EM sufficient stats over all sequences, runs M-step until `|Δ mean-LL| < tol`. Writes JSON params + history CSV. |
| `experiments/cw-decoder/data/hmm_params.json` | Trained parameters (committed). |
| `experiments/cw-decoder/data/hmm_params.history.csv` | EM convergence trace. |

Activation: `DITDAH_HMM=<path-to-json>` (also accepts `seed` to use the
hand-seeded pre-EM model, or empty/`0` to disable). Default behavior is
unchanged — no env var, no HMM, no behavioral diff.

### Model

* States `S = {Dit, Dah, Intra, Char, Word}`.
* Observation per run: `x = ln(len_samples / dot_len_samples)` where
  `dot_len_samples` is set by the WPM hint in training and by ditdah's
  existing self-calibration / pinned-WPM at inference.
* Emission: `N(μ_s, σ_s²)` per state, with `σ_s ≥ 0.20` floor (raised from
  the initial 0.05 floor — see §"Sigma collapse" below).
* Transitions: parity-restricted at run-time (an off→off transition is
  always disallowed because the runs alternate by construction). Counts
  are accumulated only over legal transitions.

### Training

* Corpus: 1,576 ARRL chunks across 20/25/30 WPM (15 WPM has no chunks).
* For each chunk: load WAV → resample to 12 kHz → bandpass → Goertzel @ pitch → smooth → IQR-based threshold using the pinned WPM → debounced run-length extraction. Yields **674,947 alternating intervals**.
* Initialization: hand-seeded `μ = (0, ln 3, 0, ln 3, ln 7)`, `σ = (0.30, 0.30, 0.30, 0.30, 0.40)`, plausible morse transitions.
* E-step: forward-backward in log-space (sequences average ~430 intervals; numerically stable). Accumulates `γ_t(s)` (state posteriors) and `Σ ξ_t(i,j)` (transition pair posteriors) in `f64`.
* M-step: re-estimate `μ`, `σ`, `log_a`, `log_π`. Standard MLE updates.

### EM convergence

```
iter  0: seqs=1576 obs=674947 mean-LL=-0.54420 delta=inf
iter  1: seqs=1576 obs=674947 mean-LL=-0.11019 delta=0.434
iter  2: seqs=1576 obs=674947 mean-LL=-0.10777 delta=2.4e-3
iter  3: seqs=1576 obs=674947 mean-LL=-0.10777 delta=5.2e-7  ← converged
```

Wall-time end-to-end: 5.9 s for run extraction (parallel over chunks) + 4
EM iters × ~3 s each ≈ 18 s on a Windows desktop. Well within "fast iter".

### Learned parameters

```
state    mu     sigma   log_pi
Dit    -0.090   0.200   -1.331
Dah    +1.069   0.200   -1.831
Intra  +0.084   0.200   -1.258
Char   +1.129   0.200   -1.379
Word   +1.961   0.200   -3.230

log_a (rows = from, cols = to):
       Dit     Dah     Intra   Char    Word
Dit    -inf    -inf    -0.492  -1.197  -2.446
Dah    -inf    -inf    -0.431  -1.299  -2.560
Intra  -0.579  -0.822  -inf    -inf    -inf
Char   -0.521  -0.901  -inf    -inf    -inf
Word   -0.599  -0.797  -inf    -inf    -inf
```

## Per-sample numbers vs baselines

| sample                          | baseline CER | viterbi CER | **HMM CER** | Δ vs baseline |
| ------------------------------- | -----------: | ----------: | ----------: | ------------: |
| arrl-13wpm-farnsworth           |        0.385 |         —   |   **0.615** |        +0.230 |
| arrl-20wpm                      |        0.414 |         —   |   **0.414** |         0.000 |
| arrl-30wpm                      |        0.056 |         —   |   **0.056** |         0.000 |
| arrl-40wpm                      |        0.052 |         —   |   **0.052** |         0.000 |
| cq-pota-aa6pw                   |        0.321 |       0.167 |   **0.536** |        +0.215 |
| cq-pota-de-wa6mow               |        0.175 |       —     |   **0.175** |         0.000 |
| **mean**                        |    **0.234** |   **0.202** |   **0.308** |        +0.074 |

| metric    | baseline | viterbi (target) | **HMM**   |
| --------- | -------: | ---------------: | --------: |
| Mean CER  |    0.234 |            0.202 | **0.308** |
| Mean WER  |    0.352 |              —   | **0.385** |

**The HMM did not beat the baselines.** It matches baseline exactly on 4 of
6 samples (arrl-20wpm, arrl-30wpm, arrl-40wpm, cq-pota-de-wa6mow) and
regresses on 2 (arrl-13wpm-farnsworth and cq-pota-aa6pw).

## Honest assessment

The trained model is *correct in the sense that EM finds canonical morse*:
the learned means are within 5% of the textbook 1:3:7 dit/dah/word ratios.
But that's exactly the problem.

1. **The HMM's decision boundaries are *more* aggressive than baseline.**
   * Intra–Char midpoint in log-space: `(0.084 + 1.129)/2 = 0.605` → ratio **1.83**. Baseline: **2.0**.
   * Char–Word midpoint: `(1.129 + 1.961)/2 = 1.545` → ratio **4.69**. Baseline: **5.0**.
   * So the corpus-trained HMM splits letters and inserts word breaks slightly more eagerly than baseline. That cost us on Farnsworth (where character gaps are stretched and the HMM mis-classifies them as word breaks) and on noisy / fading audio (`cq-pota-aa6pw`), where short-burst noise gets parsed as multiple `E` letters with intra-gaps instead of one BAD_COPY `*`.

2. **The corpus is too clean.** ARRL practice transmissions are produced by a paddle keyer with very tight timing. The fitted `σ = 0.20` is almost entirely the noise floor of our DSP front-end, not real operator timing variance. With such low variance, the model is effectively a pair of sharp threshold cuts at slightly tighter ratios than baseline. Real-world CW (POTA, contests) has *much* wider gap distributions, which a corpus-trained model has no way to anticipate.

3. **Sigma collapse.** With the original `MIN_SIGMA = 0.05` floor, all five state σ-values immediately collapsed onto the floor, producing very peaked emissions that overrode the transition prior. Raising the floor to 0.20 produced essentially identical bench numbers (0.300 → 0.308) — the issue isn't smoothness, it's where the means land.

4. **Compared to `viterbi` (target 0.202).** Without that decoder's source I can only speculate, but my read is the viterbi baseline likely either (a) trains on a corpus that includes Farnsworth/QSB audio so its means/sigmas span a wider range, or (b) operates over symbol-level lattices (constrained by a morse-table bigram prior) rather than per-interval state classification. Either change would help here; both are larger than what was time-boxed for this round.

5. **Where this *would* pay off.** Two concrete improvements likely help if pursued:
   * **Train per-WPM** rather than pooling: the HMM means are the same in dot-length-units across WPM, but the *sigmas* would diverge (slow CW has more drift). Per-WPM σ would soften decisions where it counts.
   * **Add a "noise" state.** The aa6pw regression is dominated by short noise bursts being parsed as Dit-Intra-Dit-Intra-… A 6th state with high σ and low transition probability would absorb these and prevent the `EEEEE` pathology.

## Notes on learned parameters

* **Dit μ = -0.09** vs **Intra μ = +0.08** — about 0.17 log-units (~18%) apart. The corpus consistently has slightly *longer* intra-element gaps than dit on-times. This is plausibly an envelope-asymmetry artifact of the smoothing low-pass in the front-end (rise-time shorter than fall-time → on intervals slightly truncated, off intervals slightly lengthened). Worth a deeper look if anyone touches the DSP.
* **Dit→Word = exp(-2.45) ≈ 0.087** — about 1 in 12 element-end transitions in the corpus is a word boundary. This matches reasonable English text statistics. The hand-seeded prior of 0.05 was too low; the hand-seeded inter-letter `Dit→Char ≈ 0.40` was a touch high vs the learned 0.30.
* **`log_pi[Word] = -3.23`** — barely any chunks start in a word gap (because the chunker trims leading silence). Reassuring that EM picks this up.
* **Off-state→Dit > Off-state→Dah** for all three off-states (≈0.55:0.45 ratio). English/CW prosign mix biases morse symbols dot-heavy (E=., I=.., S=…, T=- only T is dah-only). Matches general morse statistics.

## Reproduction

```powershell
cd experiments\cw-decoder

# 1. Build
cargo build --release --bin cw-decoder --bin train-hmm

# 2. Train (≈18 s on a desktop)
.\target\release\train-hmm.exe `
    --manifest    'C:\...\arrl-archive\manifest.jsonl' `
    --corpus-root 'C:\...\arrl-corpus-fast' `
    --out         'data\hmm_params.json'

# 3. Bench (HMM on)
$env:DITDAH_HMM = (Resolve-Path .\data\hmm_params.json)
python ..\..\bench.py (Resolve-Path ..\..)
Remove-Item env:DITDAH_HMM   # back to baseline
```

## Files of interest

* `experiments/cw-decoder/vendor/ditdah/src/gap_hmm.rs` — the HMM
* `experiments/cw-decoder/src/bin/train_hmm.rs` — the trainer
* `experiments/cw-decoder/data/hmm_params.json` — trained params
* `experiment_report.json` — bench output with HMM enabled (this run)
* `experiment_report.baseline.json` — bench output with HMM disabled
* `experiment_report.hmm.json` — duplicate of the HMM run, kept for diff
