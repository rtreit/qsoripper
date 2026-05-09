# Experiment: WA6MOW four-toggle layer-localization diagnostic

- Branch: `u/randy/cw-wa6mow-diag`
- Base SHA: `bc4580e`
- Sample: `data/cw-samples/training-set-a/cq-pota-de-wa6mow.mp3`
- Truth:  `NQ CQ POTA DE WA6MOW CQ POTA DE WA6MOW K`
- Needle: leading `WA` of the **first** `WA6MOW` transmission.

## TL;DR — Verdict

**The bug is WPM auto-lock at startup.**

The Goertzel/onset/region-detection front end is *not* the failure layer.
Padding, pitch lock, and manual region boundaries cannot recover WA. Only
forcing WPM ≈ 22 (the truth speed; auto-WPM is locking at ~9–11 WPM
on the first burst) recovers the leading WA of the first `WA6MOW`.

This corroborates the `bayes-joint` agent's earlier observation that the
arrl-20wpm front end seeds at 11–13 WPM but truth is 20 WPM. The
diagnostic now confirms that the same low-WPM seed bias is what
destroys the first transmission of WA6MOW.

## Per-toggle results

| # | Toggle | 1st WA6MOW | 2nd WA6MOW | WA6MOW count | Decoded |
|---|--------|:---------:|:---------:|:---:|---------|
| 1 | full-file `file` (auto-WPM)            | ❌ | ✅ | 1 | `NQ CQPO TA DE 5RHTI OW CQ PO TA DEWA6M OW K` |
| 2 | `stream-region` (default)              | ❌ | ✅ | 1 | `NQ CQPOTA DE*6MOW CQ PO TADEWA6M OW K` |
| 3 | `stream-region --pad-s 0.30`           | ❌ | ✅ | 1 | `NQ CQPOTA DE*6MOW CQ PO TADEWA6M OW K` |
| 4 | `stream-region --pad-s 0.50`           | ❌ | ✅ | 1 | `NQ CQPOTA DE*6MOW CQ PO TADEWA6M OW K` |
| 5 | `stream-region --pad-s 1.00`           | ❌ | ✅ | 1 | `NQ CQ P O T A D E SIRSI TI OW C Q P O T A D E WA 6 M OW K` |
| 6 | `stream-region --force-wpm 22`         | ✅ | ✅ | **2** | `NQ CQPOTADEWA6MOW CQPOTA DEWA6MOW K` |
| 7 | `stream-region --force-pitch 760`      | ❌ | ✅ | 1 | `NQ CQ P O T A D E SIRSI TI OW C Q P O T A D E WA 6 M OW K` |
| 8 | `stream-region --region-start-s 0 --region-end-s 12` | ❌ | n/a | 0 | `NQ CQPOTA DE*6MOW CQ E` |
| 9 | toggle 8 + `--force-wpm 22` (window contains only 1st burst) | ✅<sup>†</sup> | n/a | 1 | `NQ CQPOTADEWA6MOW CQE` |

<sup>†</sup> Window only spans `[0, 12 s]`, which contains only the
first transmission, so a single `WA6MOW` in the output is full
recovery.

The full per-toggle JSON is in
`experiments/cw-decoder/data/wa6mow_diagnostic.json`; rendered markdown
in `experiments/cw-decoder/data/wa6mow_diagnostic.md`.

## Layer attribution

| Layer                              | Hypothesis                                         | Falsified by  |
|------------------------------------|----------------------------------------------------|---------------|
| Region cropping (onset clipping)   | Larger pad recovers WA                             | Toggles 3–5: pad up to 1.0 s does **not** recover WA |
| Pitch lock                         | Forced pitch recovers WA                           | Toggle 7: pinning Goertzel @ 760 Hz (Fisher-best) does **not** recover WA |
| Region detection boundaries        | Hand-picked window recovers WA                     | Toggle 8: hand-picked `[0, 12 s]` window does **not** recover WA on its own |
| Streaming/warmup                   | Whole-file decode recovers WA                      | Toggle 1: whole-file `file` decode (different code path) **also** loses WA |
| **WPM auto-lock**                  | **Forced WPM recovers WA**                         | Toggles 6 & 9: pinning WPM = 22 is the **only** thing that recovers WA |
| Envelope/onset detection           | Nothing recovers WA → low-level detector failure   | Toggle 6 recovers WA → envelope detection works |

The per-toggle data uniquely points at WPM lock. Toggle 5 (`pad_s = 1.0`)
is informative on its own: with a long pad the decoder over-segments
the first burst into single letters (`P O T A D E SIRSI TI OW`) — the
classic "WPM way too low → every dah looks like a separate dit-letter"
signature. Toggle 6 fixes the same burst into `POTADEWA6MOW` by simply
informing the decoder that the rate is 22 WPM.

## Why prior work missed this

`gap-gmm` and `bayes-joint` both operate **after** the per-region decode
has already picked a wrong WPM and emitted the wrong elements. They can
re-segment the gaps but cannot resurrect dits/dahs that were never
emitted because the rate was wrong. WPM lock is the layer below them.

## Recommended next experiment (highest signal)

Replace ditdah's short-burst auto-WPM seed with a **whole-file (or
last-N-seconds) WPM estimate** before per-region decoding starts. Two
concrete options, in order of expected value:

1. **Two-pass WPM seed** — run a cheap whole-file pass to estimate the
   dominant on-time / off-time histogram peaks, derive a global WPM
   estimate, and pass it as `pin_wpm` to per-region decoding for the
   first ~3 seconds (until per-burst auto-WPM has enough samples).
   Expected to fully recover WA6MOW with no other changes.
2. **WPM bootstrap from later burst** — when the streamer has accumulated
   ≥ N committed regions, re-decode the first region with the stable
   median WPM from later regions. Effectively the production version of
   what toggle 6 / 9 prove works.

A defensive third experiment, only if 1+2 underperform: Bayesian
multi-WPM trial decode (try 12/16/20/24/28 WPM in parallel for the
first burst, pick the one with the best CW-rhythm score). Toggle 6
demonstrates the upper bound on what this can recover.

## Deliverables in this branch

- `experiments/cw-decoder/src/region_stream.rs` — added `pin_pitch_hz`
  field to `RegionStreamConfig` (gated, opt-in; default `None`
  preserves prior behaviour).
- `experiments/cw-decoder/src/main.rs` — added `--force-wpm`,
  `--force-pitch`, `--region-start-s`, `--region-end-s` flags to
  `stream-region`. All gated; defaults preserve prior behaviour.
- `experiments/cw-decoder/scripts/wa6mow_diagnostic.py` — runs the
  9-toggle harness and emits both JSON and markdown reports.
- `experiments/cw-decoder/data/wa6mow_diagnostic.{md,json}` — diagnostic
  output for this base SHA.
- `EXPERIMENT_REPORT.md` — this file.

## Hygiene

- `cargo fmt` clean.
- `cargo clippy --all-targets -- -D warnings` clean.
- Pre-existing failing test
  `harvest::tests::w1aw_harvest_produces_real_candidates_without_needles`
  is unrelated and not touched by this branch.
- No decoder math changed — flags are pure plumbing.
