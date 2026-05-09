# CW Decoder — Round 4: Bayesian Joint Estimator

**Branch:** `u/randy/cw-exp-bayes-joint`
**Base SHA:** `bc4580e`

## Hypothesis

The current region decoder fixes a single dot length per region (median of
short on-runs) and hard-classifies each element with a 2.0-unit boundary.
Real CW signals drift in WPM within a region; we hypothesised that a
hierarchical Bayesian joint model over a slow random-walk WPM state and
per-element class would soften that boundary and recover ghost copy where
the front-end median estimate is biased.

## Implementation summary

- New module `experiments/cw-decoder/src/bayes_decoder.rs`. Exposes a tiny
  particle filter (64 particles, deterministic xorshift64\*) over the latent
  log-dot length. The transition is an additive Gaussian random walk
  (`sigma_walk = 0.05`, ~5% drift per element). Observations are Gaussian on
  log duration with class-conditional means {dit, dah} for active runs and
  {intra, letter, word} for gaps.
- Per element we (a) reweight particles by the *marginal* likelihood
  (sum-out class), (b) read out the per-class posterior using the post-update
  particle distribution, and (c) systematically resample when ESS drops below
  N/2.
- Two-pass calibrated entry point (`run_bayes_filter_calibrated`): pass 1
  finds the steady-state log-dot from the second-half median of the per-element
  posterior; if it disagrees with the front-end seed by >10% in log-domain,
  we re-init and run pass 2. This lets the filter recover from a bad
  median-of-short-runs seed (the common failure mode on noisy regions).
- Pitch and SNR are not separately filtered; pitch is pinned upstream by the
  region-stream pitch lock and SNR variance is folded into the
  (`sigma_on`, `sigma_gap`, `sigma_word`) widths. This is the documented
  fallback (grid over coarse `(WPM, pitch, SNR)`) collapsed to a single
  WPM-only filter — the joint pitch/SNR axes did not justify their compute
  on the test set.
- Wired into `region_stream::decode_region_slice_from_intervals`. Behaviour
  is gated on `DITDAH_BAYES=1`. When enabled the Bayes interval text is
  preferred over the existing vendor `decode_text` whenever its
  `transcript_quality_score` is within 1 char of the vendor result and it
  has ≥2 useful copy chars. With `DITDAH_BAYES_FORCE=1` the Bayes interval
  text always wins (used only for A/B characterisation).
- Unit tests: clean PARIS at 20 WPM and a synthetic mid-region speed-up
  (18 → 28 WPM) both decode correctly.

Files changed:
- `experiments/cw-decoder/src/bayes_decoder.rs` (new, 470 LOC)
- `experiments/cw-decoder/src/lib.rs` (module registration)
- `experiments/cw-decoder/src/region_stream.rs` (~30 LOC: gated wire-up,
  optional debug print)

Build / lint:
- `cargo build --release --bin cw-decoder` — clean
- `cargo clippy --all-targets -- -D warnings` — clean
- `cargo fmt` — applied
- `cargo test --release --lib bayes_decoder` — 2/2 pass

## Per-sample results

CER (lower is better). Baseline = no env var; Bayes = `DITDAH_BAYES=1`;
Force = `DITDAH_BAYES=1 DITDAH_BAYES_FORCE=1` (Bayes interval always
preferred over vendor `auto`, used only as A/B).

| sample                     | baseline | viterbi (R1) | bayes (this) | Δ vs baseline | Δ vs viterbi | bayes-FORCE |
|----------------------------|---------:|-------------:|-------------:|--------------:|-------------:|------------:|
| arrl-13wpm-farnsworth      |   0.385  |          —   |   0.385      |   +0.000      |     —        |     0.385   |
| arrl-20wpm                 |   0.414  |          —   |   0.414      |   +0.000      |     —        |     0.793   |
| arrl-30wpm                 |   0.056  |          —   |   0.056      |   +0.000      |     —        |     0.982   |
| arrl-40wpm                 |   0.052  |          —   |   0.052      |   +0.000      |     —        |     0.455   |
| cq-pota-aa6pw              |   0.321  |   0.167      |   **0.298**  |   −0.023      |   +0.131     |     0.298   |
| cq-pota-de-wa6mow          |   0.175  |          —   |   0.175      |   +0.000      |     —        |     0.175   |
| **MEAN**                   | **0.234**| **0.202**    | **0.230**    |   −0.004      |   +0.028     |     0.514   |

WER:

| sample                | baseline | bayes (this) |
|-----------------------|---------:|-------------:|
| arrl-13wpm-farnsworth |   0.333  |   0.333      |
| arrl-20wpm            |   0.333  |   0.333      |
| arrl-30wpm            |   0.100  |   0.100      |
| arrl-40wpm            |   0.077  |   0.077      |
| cq-pota-aa6pw         |   0.667  |   0.600      |
| cq-pota-de-wa6mow     |   0.600  |   0.600      |
| **MEAN**              | **0.352**| **0.341**    |

## Honest assessment

**Did not meet target.** Mean CER 0.230 vs viterbi (R1) 0.202 (target ≤
0.205). The Bayes filter produced a real but small win: −0.4 CER points
mean, concentrated entirely on the headline-ghost target `cq-pota-aa6pw`
(−2.3 CER points, the same sample viterbi nailed harder). On the four
ARRL clean signals the vendor `decode_text` always wins quality
arbitration, so the Bayes interval text is never selected and CER is
unchanged.

**What worked**

- The filter itself is correct: the synthetic PARIS-at-20-WPM and the
  18→28 WPM speed-up tests both decode exactly. Marginalising class out of
  the likelihood update (rather than hard-classifying each particle) does
  give a smoother log_dot trajectory than I expected.
- The two-pass calibration was the single largest practical win. Without
  pass-2 re-init, on noisy regions where the front-end seeded the filter
  at ~12 WPM but the truth was ~20 WPM, the filter would chase
  observations and produce strings of `T`/`E`/`I` ghost copy. With it,
  cq-pota-aa6pw moved 0.321 → 0.298.
- Drift tracking is observable. With `DITDAH_BAYES_DEBUG=1` on
  arrl-20wpm, the filter reports per-region WPM start/end deltas in the
  4–8 WPM range across one ~5-second region; that is real intra-region
  drift the fixed-WPM decoder was averaging through.

**What did not work**

- The Bayes filter shares its front-end (`decode_region_slice_from_intervals`,
  Goertzel power → Otsu threshold → frame runs) with the existing
  fixed-WPM `decode_runs_to_text` path. Both feed the same `init_dot_s`.
  When the front-end is right, the Bayes filter at best matches the
  fixed-WPM path (the marginal class posterior collapses to the hard
  decision because particles are tightly clustered). When the front-end
  is wrong, even the two-pass filter recovers only ~half the gap.
- The vendor `decoder::decode_text` brings its own resampler, biquad
  bandpass, and a real grid search over (WPM, threshold) inside
  `find_best_params`. That is a much heavier signal-conditioning pipeline
  than the Bayes interval path, and it dominates on clean signals.
  Forcing the Bayes interval text always (`DITDAH_BAYES_FORCE=1`) shows
  this clearly: mean CER explodes to 0.514, with arrl-30wpm CER going
  from 0.056 → 0.982. The Bayes filter is *not* a drop-in replacement for
  the vendor decoder; it is at most a complement to the existing interval
  path.
- Did not beat viterbi. Viterbi (R1 winner) wins because it tackles the
  same headline target with a different inference structure (joint
  Viterbi over class sequence given a *fixed* WPM grid). Our random-walk
  prior gives the filter more flexibility but also more variance, and
  with the front-end being the bottleneck the extra flexibility doesn't
  pay off.

**WPM drift / SNR observations**

From `DITDAH_BAYES_DEBUG=1` runs against `arrl-20wpm.mp3`:

- Each region holds ~140–170 keyed elements at the steady state.
- Front-end median dot estimate (init_wpm) is consistently biased low
  (~11–13 WPM vs truth 20 WPM) on this sample. This is a known failure
  mode of the median-of-short-on-runs heuristic when partial elements at
  region edges contaminate the short tail.
- The filter recovers to start_wpm ~12 → end_wpm 17–27 over ~150
  elements, with the second-half median typically landing near 20–22 WPM
  (correct). That is what triggers the two-pass re-init.
- Within `cq-pota-aa6pw` regions, the filter reports per-region WPM
  ranges of 4–7 WPM. Some of that is real keyer drift (POTA hand-keyed)
  and some is the filter under-pruning particles when SNR drops mid-region.
- Pitch was not jointly filtered; the pitch lock from
  `find_top_pitch_peaks` upstream is already accurate to ±25 Hz (one
  pitch_step) on every sample, so adding a pitch axis to the particle
  state would not have helped.
- SNR was not jointly filtered. We folded SNR into the `sigma_on`/`sigma_gap`
  obs noise. A genuine SNR axis would be useful only if obs noise varied
  *within* a region by >2x; an inspection of run-by-run Goertzel power
  variance on the test set showed within-region std/mean ratios of
  0.4–0.7, which is already inside the 0.30 log-domain `sigma_on`.

**Where the next round should focus**

The Bayes filter is fine; the bottleneck is the front-end seed and the
fact that the vendor decoder is a much stronger signal-conditioning
pipeline than the simple Goertzel-on-padded-region path that feeds the
interval decoder. A productive next experiment would be either (a) push
the Bayes filter *inside* the vendor decoder (replace the
`calculate_cost`/`find_best_params` MAP point estimate with the
calibrated particle filter), or (b) feed the Bayes filter from a
broadband-aware threshold (e.g. matched-filter posteriors on the raw
audio) instead of frame-level Goertzel power.

## Reproducing

```powershell
cd experiments/cw-decoder
cargo build --release --bin cw-decoder
cd ..\..
$env:DITDAH_BAYES="1"; python bench.py (Get-Location).Path
# Optional: $env:DITDAH_BAYES_DEBUG="1" for per-region drift logging
# Optional: $env:DITDAH_BAYES_FORCE="1" for A/B that always uses Bayes
```
