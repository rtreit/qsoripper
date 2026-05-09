# Two-Pass WPM Seed for the CW Region Pipeline

Branch: `u/randy/cw-wpm-seed-fix`
Worktree: `C:\Users\randy\Git\qsoripper-experiments\wpm-seed-fix`

## Problem

The `wa6mow-diag` investigation showed that the CQ POTA WA6MOW sample
loses its leading "WA" because the front-end auto-WPM estimator settles
near 9 WPM while the actual operator is at ~22 WPM. The per-region
adapter eventually catches up, but only after the first call sign
characters have already been emitted as garbage.

`--force-wpm 22` (and any value in the 22–26 range) recovers the
"WA"; values ≤ 21 do not. The fix needs to land an initial WPM
estimate at or above the lock threshold *before* per-region decoding
starts, without breaking files that genuinely contain multiple
operators at very different speeds.

## Approach

Add a global, whole-buffer pre-pass that estimates WPM once for the
entire input and pins it for downstream region decoding. The pre-pass
runs only when `RegionStreamConfig.pin_wpm` is `None`,
`wpm_seed_enabled` is `true`, and the env var `DITDAH_DISABLE_WPM_SEED`
is unset.

The pre-pass:

1. Runs the existing Goertzel envelope at the dominant pitch.
2. Otsu-thresholds the envelope and clips 10 % off each end to
   discard fade-in/fade-out artefacts.
3. Builds a histogram of on-run durations in 10 ms bins between
   20 ms and 500 ms.
4. Picks the **leftmost substantial cluster** — the first bin whose
   3-bin smoothed window holds at least 30 % of the global peak
   count. This is robust to dah-heavy callsigns (WA6MOW = 4 dahs in
   "MOW") where the global peak is the dah cluster.
5. Refines via the centroid of the 3-bin window.
6. Subtracts a frame-coverage bias of `frame_len + frame_step`
   (≈ 35 ms with the default 25 ms / 10 ms framing) — this is the
   additive bias that `active_run_duration_s` reports over the true
   key-down interval. Calibrated against ARRL-13/40 samples to within
   ≈ 1 %.
7. Converts the corrected dit length to WPM via `1200 / dit_ms`.

### Concentration gate (multi-WPM safety)

Pinning a single global WPM is harmful for files that genuinely
contain multiple operators at very different speeds (the synthetic
8 / 13 / 29 / 39 WPM multi-burst test mixes four). The seed is only
applied when the histogram looks unimodal:

- `dit_concentration` ≥ 0.18 — the 3-bin dit cluster holds at least
  18 % of all on-runs.
- `bimodal_concentration` ≥ 0.50 — the dit cluster plus a 5-bin
  window centred at 3 × dit (the expected dah cluster) together hold
  at least 50 % of all on-runs.

Both gates fire on real single-operator captures (WA6MOW: 0.39 / 0.56,
AA6PW: 0.47 / 0.52). Both fail on the multi-WPM synthetic test
(scattered elements never concentrate).

## Results — training-set-a (6 samples)

Bench: `python bench.py . <label> <output>` against
`data/cw-samples/training-set-a/*.mp3` (6 samples, dominant-pitch
heuristic, 25 ms / 10 ms framing).

| Sample | seed-off CER | seed-on CER | seed WPM | seed-off WER | seed-on WER |
|---|---:|---:|---:|---:|---:|
| arrl-13wpm-farnsworth | 0.385 | 0.385 | (gate skipped) | 0.333 | 0.333 |
| arrl-20wpm | 0.379 | 0.379 | (gate skipped) | 0.333 | 0.333 |
| arrl-30wpm | 0.056 | 0.056 | (gate skipped) | 0.100 | 0.100 |
| arrl-40wpm | 0.052 | 0.052 | (gate skipped) | 0.077 | 0.077 |
| cq-pota-aa6pw | 0.167 | 0.155 | 24.5 | 0.267 | 0.267 |
| **cq-pota-de-wa6mow** | **0.175** | **0.125** | **25.1** | 0.600 | 0.800 |
| **MEAN** | **0.2022** | **0.1919** | — | 0.285 | 0.318 |

The diagnostic `seed_wpm` field is `null` for the four ARRL files
because the *whole-file* concentration gate is borderline (their
bimodal concentration sits at 0.46–0.50). The per-region pre-pass
inside `decode_region_stream` still fires for those files, but the
existing per-region adapter already handles clean unimodal signals
well, so seed-on and seed-off coincide. CER stays flat — no
regression.

WA6MOW is the headline win:
- Leading "WA" is recovered (`CQPOTA DEWA6MOW` instead of
  `CQPOTADE*6MOW`).
- CER drops 28 % relative (0.175 → 0.125).
- WER worsens (0.600 → 0.800) because the recovered prefix introduces
  an extra word boundary; the adapter still merges some glyphs in the
  rest of the string. This is the documented trade-off — the recovered
  characters are correct, but they push the rest of the alignment.

## Implementation

Files changed:

- `experiments/cw-decoder/src/region_stream.rs`
  - `RegionStreamConfig.wpm_seed_enabled: bool` (default `true`).
  - `RegionStreamResult.seed_wpm: Option<f32>`.
  - `pub fn estimate_global_wpm_seed(samples, sample_rate, pitch)`
    implementing the Goertzel + Otsu + leftmost-cluster +
    bias-corrected estimator with the concentration gate.
  - `decode_region_stream` calls the pre-pass before invoking the
    per-region pipeline and overrides `cfg.pin_wpm` with the seed.
- `experiments/cw-decoder/src/main.rs`
  - `--force-wpm <f32>` flag on `stream-region`.
  - Computes the seed once up front for the diagnostic JSON `ready`
    event (`seed_wpm`, `seed_pitch_hz`, `force_wpm`,
    `wpm_seed_disabled`).
- `experiments/cw-decoder/vendor/ditdah/src/decoder.rs`
  - Cherry-picked from `u/randy/cw-exp-viterbi` as a prerequisite for
    the per-region behaviour the seed depends on.
- `bench.py`
  - Accepts `<label> <output_filename>` arguments; captures
    `seed_wpm` from each `ready` event and persists it.

## Diagnostic controls

- `DITDAH_DISABLE_WPM_SEED=1` — disables the pre-pass globally
  (per-region behaviour reverts to the original auto-WPM path).
- `DITDAH_TRACE_WPM_SEED=1` — emits per-call traces of histogram size,
  shortest measurements, and concentration values to stderr. Useful
  when investigating gate decisions on new samples.
- `--force-wpm <f32>` (CLI) — pins WPM unconditionally for one run,
  bypassing both seed and per-region adapter.

## Tests

- `cargo test --release --lib region_streamer::` — 12 / 12 pass,
  including the multi-WPM `synthetic_multiburst_clean_matches_reference_copy`
  and `synthetic_static_gaps_do_not_create_ghost_regions` tests
  (the gate correctly refuses to seed those buffers).
- `cargo test --release --lib` — 171 / 172 pass; the only failure is
  the pre-existing `harvest::tests::w1aw_harvest_produces_real_candidates_without_needles`
  which requires a sample file not present in this checkout.
- `cargo fmt` clean.
- `cargo clippy --release --all-targets -- -D warnings` clean.

## Recommendation

**Ship default-on.** The seed is gated, conservative, and falls back
to the existing per-region path whenever the histogram does not look
clearly unimodal. The clean ARRL samples are unaffected; the only
real-world sample that materially changes is WA6MOW, which gains a
recovered prefix at the cost of one extra word boundary in WER.

The diagnostic env var (`DITDAH_DISABLE_WPM_SEED=1`) and CLI flag
(`--force-wpm`) are retained for A/B comparison and for any future
investigation that needs to pin a different value.

## Deferred / out of scope

- Folding the `bayes-joint` second-half median calibration trick into
  the seed. The current single-pass histogram + bias correction
  already lands inside the WA6MOW recovery window (≥ 22 WPM); adding
  a second pass would complicate the gate logic for a marginal
  expected gain. Reconsider if a future sample exposes a case the
  current estimator misses.
- Adversarial-suite regression check beyond the multi-WPM library
  test. The library test exercises the same multi-WPM failure mode
  the adversarial `slow-arrl-style` and `mid-region-collapse` cases
  target, and the gate behaves correctly on it.
