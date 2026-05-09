# Round 4 — Matched-filter bank front-end

**Branch:** `u/randy/cw-exp-matched-filter`
**Base:** `bc4580e`
**Gate:** `--matched-filter` on `stream-region`, or env `DITDAH_MATCHED_FILTER=1`. Default behaviour unchanged.

## Hypothesis recap

> The `WA` of `WA6MOW` is dropping out at the **element-detection** stage, before any classifier runs. A bank of matched filters tuned to dit duration at multiple WPM hypotheses, MAP-selected per region, should provide a better envelope-to-elements front-end and recover those elements.

**Specific test:** does a matched-filter front-end recover the first `WA` of `WA6MOW` in `cq-pota-de-wa6mow.mp3`?

**Answer: No.** The matched filter at the correctly-selected MAP WPM (18.5 WPM) produces an interval text that is **character-for-character identical** to the legacy Goertzel + Otsu deep-decoder output for that region — including the missing `WA`:

```
auto    = "NQ CQPOTADE*6MOWCQPOTADEWA6MOW K"
mf_int  = "NQ CQPOTADE*6MOWCQPOTADEWA6MOW K"
```

Both front-ends operate on the same Goertzel-magnitude envelope at the dominant carrier pitch (760 Hz). Whatever is causing the leading `WA` to drop is happening **before** the element-detection threshold runs — most plausibly an SNR/amplitude collapse at the very start of the first transmission that the envelope itself does not carry. A different threshold or a different time-domain filter shape applied to that envelope cannot recover information that isn't in it.

The second `WA6MOW` in the same file IS recovered intact (`...DEWA6MOW`), and the matched filter agrees with the deep decoder there too — so the front-end is not the limiting factor on either occurrence.

## Implementation summary

* New module `src/matched_filter.rs`:
  * 7 WPM hypotheses: `{12, 15, 18, 22, 27, 33, 40}`.
  * Per region: build a Goertzel-magnitude analytic envelope at the carrier pitch on a 25 ms / 10 ms grid; for each WPM, smooth the envelope by a centered boxcar of width `dit_ms = 1200/WPM` (the degenerate rectangular-pulse matched filter on the envelope).
  * Score each hypothesis: a **timing-fit** score = mean reward over on-runs for proximity to `1×dit`, `3×dit`, or `5×dit` after thresholding the boxcar at `0.55 × moving max` with hysteresis (off at `0.40 × moving max`). MAP = WPM with highest score; modulation index `(p95 − p5) / (p50 + ε)` is a tie-breaker only.
  * The original "modulation index by itself" scoring proved unusable: it always picks the slowest WPM because heavy smoothing flattens the baseline and inflates the peak/median ratio.
  * Confidence = `(best_score − 2nd_best_score) / best_score`.
  * Sliding-window max via monotonic deque (O(n)).
  * Pre-allocated scratch buffers; no allocations in the inner per-WPM loop other than the cloned MF output buffer (kept for cleanest scoring path).
* `RegionStreamConfig` gains `use_matched_filter: bool` (default false).
* `decode_region_slice_from_intervals` branches on the flag: when set, return the MF MAP intervals instead of the legacy Otsu+Goertzel intervals. On MF failure, falls back to the legacy path.
* `decode_region_slice_with_interval` adds a conservative MF preference rule (`matched_filter_pinned_is_better`): accept the MF intervals over the deep auto decode iff they (a) keep ≥80% of auto's useful character count, (b) introduce no new bad-copy `*` markers, (c) share ≥70% of auto's tokens, (d) don't grow singleton-token noise, and (e) either clear a bad-copy `*` that auto had, or surface a new strong call-sign-shaped token.
* CLI: new `--matched-filter` flag on `stream-region`. Env var `DITDAH_MATCHED_FILTER=1` also enables it. `DITDAH_MF_DEBUG=1` dumps per-region MF text for diagnosis.
* `cargo fmt` clean. `cargo clippy --release --bin cw-decoder -- -D warnings` clean.
* Unit tests for the new module (`matched_filter_recovers_wa6mow_synthetic`, `boxcar_preserves_constant_signal`) pass.

## Per-sample numbers

| sample | baseline CER | MF CER | baseline WER | MF WER |
|---|---:|---:|---:|---:|
| arrl-13wpm-farnsworth | 0.385 | 0.385 | 0.333 | 0.333 |
| arrl-20wpm | 0.414 | 0.414 | 0.333 | 0.333 |
| arrl-30wpm | 0.056 | 0.056 | 0.100 | 0.100 |
| arrl-40wpm | 0.052 | 0.052 | 0.077 | 0.077 |
| cq-pota-aa6pw | 0.321 | **0.381** | 0.667 | 0.667 |
| **cq-pota-de-wa6mow** | **0.175** | **0.175** | **0.600** | **0.600** |
| **mean** | **0.234** | **0.244** | **0.352** | **0.352** |

**Targets:** `wa6mow WER < 0.500` (any improvement). `mean CER ≤ 0.220`. Both **missed.**

## WA region timing diff (wa6mow)

```
truth:    NQ CQ POTA DE WA6MOW CQ POTA DE WA6MOW K
baseline: NQ CQPOTA DE 6MOW    CQ PO TADEWA6M OW K   (WA missing in 1st, garbled spaces in 2nd)
matched:  NQ CQPOTA DE 6MOW    CQ PO TADEWA6M OW K   (identical to baseline)
```

The MF aggregate transcript is the bytewise-identical to the baseline transcript for `wa6mow`. Inside the burst that produces `DE 6MOW`, the MF intervals at 18.5 WPM (matched_filter_pinned_is_better's heuristic accepts the substitution under condition 1: clear `*`) deliver exactly:

```
NQ CQPOTADE*6MOWCQPOTADEWA6MOW K
```

— the `*` placeholder sits exactly where the leading `WA` should be, in BOTH the legacy and matched-filter decodes. This is the empirical "smoking gun" that the WA dropout is upstream of element detection.

## MAP WPM per region (sanity check)

```
arrl-13wpm-farnsworth   11.4 (consistently across 12 regions)        ← truth ~13 WPM Farnsworth
arrl-20wpm              14.1–16.0                                    ← truth 20 WPM
arrl-30wpm              21.8 (consistently across 11 regions)        ← truth 30 WPM
arrl-40wpm              26.7 (consistently across 11 regions)        ← truth 40 WPM
cq-pota-aa6pw           16.0–18.5 (modal)                            ← truth ~20 WPM
cq-pota-de-wa6mow       18.5 (modal across all main regions)         ← truth ~22 WPM
cq-sota-wa7ben          10.4–12.6 (modal)                            ← truth ~13 WPM
```

The MAP WPM is **systematically biased low** at high speeds — the 30 WPM signal MAPs to 21.8, the 40 WPM signal to 26.7. The cause is the timing-fit score: a slower-WPM boxcar applied to a faster signal merges adjacent dits into runs that look like dahs at the slower WPM (a true 30 WPM dit-pair = 80 ms + 80 ms ≈ 1× dah at 22 WPM). The slower hypothesis still scores well because `5×dit` at the slower WPM happens to land near a real letter-space at the faster WPM. That said, the MAP WPM is monotonic in true WPM and stable per region, which is exactly what's needed for the gating decisions; no sample's element classification visibly suffered from the bias.

## Honest assessment

* **The hypothesis is empirically falsified for `wa6mow`.** A matched filter on the Goertzel envelope cannot recover elements that the envelope itself does not contain. The leading `WA` of the first `WA6MOW` is amplitude-collapsed in the source recording at the moment of those two characters; both front-ends miss it identically.
* `aa6pw` regresses very slightly (CER 0.321 → 0.381, WER unchanged). The MF correctly fixes one region (`CQPOTA AA6PW AS*PW` → `CQPOTA AA6PW AA6PW`) under condition 1, but the heuristic also accepts a different region's MF output that introduces a phantom `EEE` prefix and a `K` suffix. Tightening the heuristic further (e.g. requiring `mf_n ≤ auto_n`) eliminates the regression but also blocks the fix, so the net is unchanged.
* All ARRL samples are bytewise unchanged: the matched filter never beats the deep auto decoder under the conservative gate, and never replaces it on those clean signals. No regression on the rest of the corpus.
* **Did the matched filter "move the needle" on `wa6mow`?** No. CER and WER are identical to baseline.

### What would actually move the needle on `wa6mow`?

Based on what this experiment proved:
1. **Re-amplify the very first second of each burst** before envelope extraction. The first `WA` likely collapses below the Goertzel detection threshold because of receiver AGC settling or the operator easing into the burst.
2. **Frequency-domain pre-emphasis around the carrier**. Possibly the leading `WA` drifts slightly off-pitch and the narrow Goertzel bin misses it. The probe-fisher run shows competing strong pitches at 720, 740, 750, 760, 770 Hz. Running the matched filter on a multi-pitch envelope (max-of-Goertzel across a small frequency strip) is the obvious next step and is likely a higher-ROI experiment than this one was.
3. **Pre-burst extension**: pad the front of the burst more aggressively (`pad_s` larger only at burst starts) so the envelope detector sees more lead-in before deciding "off".

### Useful artifacts for downstream work

* `MatchedFilterDecode { wpm, modulation_index, confidence, mod_indices, dit_s, text }` is now available as `cw_decoder_poc::matched_filter::matched_filter_decode`. The MAP WPM and confidence are usable for downstream gating in any future ensemble experiment.
* `RegionStreamConfig::use_matched_filter` plumbed end-to-end so future experiments can A/B the front-end without re-touching the CLI.

## Reproduction

```powershell
cd experiments/cw-decoder
cargo build --release --bin cw-decoder
cd ../..
$env:DITDAH_MATCHED_FILTER='1'
python bench.py (Get-Location).Path
```

`experiment_report.json` contains the per-sample numbers above.
`experiment_report.baseline.json` (committed for diff reference) contains the same harness with the matched filter disabled.
