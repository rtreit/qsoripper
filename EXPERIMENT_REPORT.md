# Round 4 — Bigram-Viterbi rescoring on ARRL corpus

**Branch:** `u/randy/cw-exp-bigram-corpus`
**Base:** `bc4580e`
**Cherry-pick provenance:** `vendor/ditdah/src/decoder.rs` taken from
`u/randy/cw-exp-viterbi` commit `f16c651` (Round 1 winner — soft per-element
posterior + ghost suppression). Source-only port; the viterbi branch's
`bench.py`, `experiment_report.json`, and `EXPERIMENT_REPORT.md` were
deliberately NOT brought across.

## TL;DR — honest negative result

A character bigram trained on the 154k-char ARRL corpus, composed (in
log-space) with the soft acoustic posterior and decoded by a per-letter
Viterbi over `(prev_char, curr_char)`, **does not improve** mean CER over the
viterbi-only baseline. At small lambda it is neutral-to-slightly-worse; at
large lambda it actively damages rare-callsign samples (`aa6pw`, `wa6mow`)
exactly the way the prior `charlm` experiment did.

| config              | mean CER | mean WER | aa6pw CER | wa6mow CER | wa6mow WER |
| ------------------- | -------: | -------: | --------: | ---------: | ---------: |
| baseline (Round 0)  |   0.234  |    —     |    0.321  |     —      |    0.600   |
| viterbi-only (port) |  0.2022  |   0.285  |    0.167  |    0.175   |    0.600   |
| **bigram λ=0.0**    | **0.2064** | 0.302  |    0.167  |    0.200   |    0.700   |
| bigram λ=0.25       |  0.2064  |   0.302  |    0.167  |    0.200   |    0.700   |
| bigram λ=0.5        |  0.2064  |   0.302  |    0.167  |    0.200   |    0.700   |
| bigram λ=1.0        |  0.2365  |   0.418  |    0.298  |    0.250   |    0.800   |
| bigram λ=2.0        |  0.2928  |   0.491  |    0.417  |    0.450   |    1.000   |

**Best LM config: λ ∈ {0.0, 0.25, 0.5}** (all identical — see analysis below).
**Mean-CER winner overall: viterbi-only (no LM)** at 0.2022.
**Target (≤ 0.190) was NOT met.**

## Bigram training setup

- **Corpus:** ARRL Code Practice manifest at
  `C:\Users\randy\Git\qsoripper-experiments\arrl-corpus-fast\data\cw-samples\arrl-archive\manifest.jsonl`,
  1,576 chunks, 153,736 truth chars (155,311 after joining with single
  spaces).
- **Vocabulary:** `A–Z 0–9 ' ' / = . , ?` (42 symbols).
- **Smoothing:** add-one Laplace. `P(c2|c1) = (count(c1,c2)+1) / (count(c1)+V)`.
- **Output:** `experiments/cw-decoder/data/bigram_arrl.json` (14 KB,
  `include_str!`-embedded into the binary).
- **Trainer:** `experiments/cw-decoder/scripts/train_bigram.py`.

A quick sanity check is in the unit tests: `log P(H|T) > log P(Q|T)` as
expected for English text (also dominant in ARRL bulletins).

## Composition with soft Viterbi

The bigram layer **does not replace** the soft per-element posterior; it
sits on top of it.

1. `build_lattice(intervals, dot_len)` mirrors the segmentation logic of
   `soft_decode_intervals` from the viterbi branch — same noise floor,
   same single-element-letter penalty — but emits a `Vec<Slot>` where each
   `Letter` slot carries the per-element `(log_p_dot, log_p_dash)` pair
   for every observed on-interval.
2. `bigram_lm::decode_lattice(slots, lambda)` runs a standard bigram
   Viterbi:
   - per-slot acoustic candidate set = every Morse code with the same
     element count, scored as
     `acoustic = Σ (log_p_dot[i] if c=='.' else log_p_dash[i])`,
     normalized by per-slot max so λ is comparable across slots;
   - transition = `λ · log P(curr | prev)` from the LM;
   - word-break slots force SPACE and reset LM context to SPACE.
3. Backtrack and trim. Word-breaks are merged consecutively to avoid
   stuttering blanks.

Activated at runtime by `DITDAH_BIGRAM_LM=1`; weight via
`DITDAH_BIGRAM_LAMBDA` (default `0.5`).

## Per-sample numbers

### viterbi-only (baseline for this branch)

| sample                  |  CER   |  WER   |
| ----------------------- | -----: | -----: |
| arrl-13wpm-farnsworth   | 0.385  | 0.333  |
| arrl-20wpm              | 0.379  | 0.333  |
| arrl-30wpm              | 0.056  | 0.100  |
| arrl-40wpm              | 0.052  | 0.077  |
| cq-pota-aa6pw           | 0.167  | 0.267  |
| cq-pota-de-wa6mow       | 0.175  | 0.600  |
| **mean**                | **0.2022** | **0.285** |

### viterbi + bigram LM, λ = 0.5

| sample                  |  CER   |  WER   | Δ-CER vs viterbi |
| ----------------------- | -----: | -----: | ---------------: |
| arrl-13wpm-farnsworth   | 0.385  | 0.333  |       0.000      |
| arrl-20wpm              | 0.379  | 0.333  |       0.000      |
| arrl-30wpm              | 0.056  | 0.100  |       0.000      |
| arrl-40wpm              | 0.052  | 0.077  |       0.000      |
| cq-pota-aa6pw           | 0.167  | 0.267  |       0.000      |
| cq-pota-de-wa6mow       | 0.200  | 0.700  |     **+0.025**   |
| **mean**                | **0.2064** | **0.302** | **+0.004** |

### viterbi + bigram LM, λ = 1.0 (the LM dominates)

| sample                  |  CER   |  WER   | Δ-CER vs viterbi |
| ----------------------- | -----: | -----: | ---------------: |
| arrl-13wpm-farnsworth   | 0.385  | 0.333  |       0.000      |
| arrl-20wpm              | 0.379  | 0.333  |       0.000      |
| arrl-30wpm              | 0.056  | 0.100  |       0.000      |
| arrl-40wpm              | 0.052  | 0.077  |       0.000      |
| cq-pota-aa6pw           | 0.298  | 0.867  |     **+0.131**   |
| cq-pota-de-wa6mow       | 0.250  | 0.800  |     **+0.075**   |
| **mean**                | **0.2365** | **0.418** | **+0.034** |

## Honest assessment — what happened, and why

1. **The LM is neutral on the four ARRL samples.** They're literally drawn
   from the same corpus the LM was trained on, so the bigram should help
   most there if it helped anywhere. It didn't — the per-slot acoustic
   posterior is already so peaked (per-element Gaussian σ = 0.55·dot_len,
   so log-prob differences are O(10) within a slot) that even a strongly
   informed bigram log-prob (~−2 to −5 nats) can't flip a candidate at
   λ ≤ 0.5. The LM is essentially being out-voted on every slot.

2. **At λ ≥ 1.0 the LM does flip slots — and it flips them wrong on
   callsigns**, exactly the failure mode the user warned about and that
   killed the prior `charlm` experiment. The corpus has zero examples of
   `AA6PW` or `WA6MOW`, so callsign bigrams like `A6` and `6P` are
   downweighted vs. common English bigrams (`TH`, `IN`, `ER`). Even at
   character-level granularity the LM is biased toward English text and
   away from callsigns. The user's hypothesis that "character-level should
   be more robust" turned out to be partially true (it's less catastrophic
   than word-level charlm at small λ) but the failure mode is the same in
   kind.

3. **λ = 0 vs viterbi-only (0.2064 vs 0.2022) — why the small gap?**
   When the LM path is enabled even at λ = 0, the candidate enumeration
   only considers Morse codes whose length matches the observed element
   count. The `MORSE_BY_LEN` table covers A–Z, 0–9, `/`, `=`, `.`, `,`,
   `?` (matching the LM vocab) but omits some long punctuation that
   `morse_to_char` recognizes (`'`, `"`, `:`, `;`, `(`, `)`, `&`, `@`,
   `_`, `!`, `$`, `+`, `-`, SK). When such a slot appears in `wa6mow`
   it falls to `*` (BAD_COPY_MARKER), which `bench.py.normalize` collapses
   to a space, slightly inflating its CER from 0.175 → 0.200. This is a
   structural cost of the LM path, not LM rescoring.

4. **Mixing in a callsign-frequency bigram (per the gotcha note) was not
   attempted.** Even with one I'd expect the same dynamic: per-slot
   acoustic peakedness dominates wherever the audio is clean, and on the
   noisy `aa6pw`/`wa6mow` samples a generic English+callsign LM still
   doesn't have any direct information about the specific callsigns that
   would let it correct true acoustic ambiguity. A *length-aware* prior
   (e.g., callsign-shape regex matched against the candidate stream) is
   the more promising direction, and is fundamentally a different
   architecture from a character bigram.

## Comments on language-model risk

- Character bigrams trained on bulk text (English, even ham-flavored ARRL
  bulletins) carry a structural bias **away from callsigns** — they'll
  always prefer "THE" over "AA6" given any acoustic ambiguity. Without
  per-token-class conditioning (callsign-mode vs. text-mode), this is
  unavoidable.
- The acoustic posterior in the soft Viterbi is already very confident on
  clean audio. To get LM rescoring to **do anything useful**, the
  acoustic side would need to expose a calibrated, less-peaked posterior
  (lower σ-tightness, or multiple candidate segmentations rather than
  just multiple candidates per fixed segmentation). Right now the LM
  either does nothing (small λ) or steamrolls the audio (large λ).
- The most promising place a bigram **might** help — borderline ARRL
  speed/Farnsworth samples — is exactly where its 38–39% CER is dominated
  by acoustic / segmentation issues that no LM can fix without alternate
  segmentation hypotheses.

**Recommendation:** do not ship the bigram path. Keep the viterbi soft
decoder. The next promising experiment is alternate-segmentation lattices
(give the LM real choices) or a callsign-shape prior gated on contest /
DX context.

## Reproduction

```powershell
# Train (already committed; re-run only if corpus changes):
python experiments/cw-decoder/scripts/train_bigram.py `
    C:\Users\randy\Git\qsoripper-experiments\arrl-corpus-fast\data\cw-samples\arrl-archive\manifest.jsonl `
    experiments/cw-decoder/data/bigram_arrl.json

# Build:
cd experiments/cw-decoder
cargo build --release --bin cw-decoder

# Bench viterbi-only:
$env:DITDAH_BIGRAM_LM = $null
python ../../bench.py (Resolve-Path ../..)

# Bench with LM at λ = 0.5:
$env:DITDAH_BIGRAM_LM = "1"
$env:DITDAH_BIGRAM_LAMBDA = "0.5"
python ../../bench.py (Resolve-Path ../..)
```
