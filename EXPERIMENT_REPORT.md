# CW decoder — Viterbi + elem-gate three-variant stack

Branch: `u/randy/cw-vit-elem-stack`
Base SHA: `bc4580e`
Bench: 6 samples in `data/cw-samples/training-set-a/` via `bench.py`.

## Cherry-pick provenance

The viterbi soft decoder and the elem-gate hard mask were ported in a
single commit by checking out the matching files from
`u/randy/cw-exp-elem-gate` (which itself merges the round-1 viterbi work
with the round-2 elem-gate). Source commits:

- `952ea4e exp(cw-decoder/viterbi): soft per-element confidence + Viterbi decode (#411)` — round-1 viterbi work, originally on `u/randy/cw-exp-viterbi` as `f16c651`.
- `2b36af5 exp(elem-gate): per-element confidence gating in ditdah` — round-2 elem-gate, originally on `u/randy/cw-exp-elem-gate`.

Files ported into this branch from `u/randy/cw-exp-elem-gate`:

- `experiments/cw-decoder/vendor/ditdah/src/decoder.rs` (soft viterbi + hard elem-gate mask + `compute_element_snrs_db`).
- `experiments/cw-decoder/src/region_stream.rs` (downstream wiring updates from the elem-gate branch).

`EXPERIMENT_REPORT.md` and `bench.py` from upstream branches were intentionally not pulled in; this branch carries its own.

## Variants

A new env var `DITDAH_STACK` selects the stack variant; unset preserves the
pre-experiment default (= hard elem-gate at 12 dB on top of soft viterbi).

| Mode | `DITDAH_STACK` | Mechanism |
| --- | --- | --- |
| viterbi-only | `viterbi` | Soft viterbi decoder only — no elem-gate, no SNR likelihood |
| hard-stack | `hard` | viterbi + per-on-interval SNR mask at `DITDAH_ELEM_GATE_DB` (default 12 dB) |
| soft-stack | `soft` | viterbi + per-element score `+= λ * log_LR_real_vs_noise(snr)`, SNR Gaussian in dB |
| abstaining-stack | `abstain` | soft + suppress single-element E/T whose summed score < `DITDAH_STACK_ABSTAIN` |

Soft / abstain tunables (env vars):

- `DITDAH_STACK_LAMBDA` — λ weight for the SNR term (default 1.0)
- `DITDAH_STACK_SNR_MEAN_DB` — Gaussian mean for "real element" prior (default 12.0)
- `DITDAH_STACK_SNR_SIGMA_DB` — Gaussian sigma (default 4.0)
- `DITDAH_STACK_ABSTAIN` — abstain threshold for single-element E/T summed log-prob (default -5.0)

The SNR log-likelihood ratio is closed-form linear in dB:

```
LLR(snr_db) = (mean / sigma^2) * (snr_db - mean / 2)
```

so it's positive above `mean/2`, negative below — a smooth ramp around `mean/2` rather than the hard step at `mean` that the elem-gate uses.

## Headline numbers

| Variant | Mean CER | Mean WER | aa6pw CER | arrl-20wpm CER |
| --- | ---: | ---: | ---: | ---: |
| **viterbi-only** (baseline)  | **0.2022** | 0.2850 | 0.167 | 0.379 |
| hard-stack                   | 0.2022 | 0.2850 | 0.167 | 0.379 |
| soft-stack λ=1.0             | 0.2059 | 0.2850 | **0.155** ⭐ | 0.414 |
| abstaining-stack λ=1.0 a=-5  | 0.2059 | 0.2850 | **0.155** ⭐ | 0.414 |

⭐ = beats the prior 0.167 ceiling on `cq-pota-aa6pw` (stretch goal).

### Per-sample breakdown

| Sample | viterbi | hard | soft λ=1.0 | abstain a=-5 |
| --- | ---: | ---: | ---: | ---: |
| arrl-13wpm-farnsworth | 0.385 | 0.385 | 0.385 | 0.385 |
| arrl-20wpm            | 0.379 | 0.379 | **0.414** | **0.414** |
| arrl-30wpm            | 0.056 | 0.056 | 0.056 | 0.056 |
| arrl-40wpm            | 0.052 | 0.052 | 0.052 | 0.052 |
| cq-pota-aa6pw         | 0.167 | 0.167 | **0.155** | **0.155** |
| cq-pota-de-wa6mow     | 0.175 | 0.175 | 0.175 | 0.175 |

## Findings

### 1. Hard-stack is a no-op against viterbi-only on this bench

`hard-stack` produced character-for-character identical output to
`viterbi-only` on all six samples. The viterbi soft decoder's
`SINGLE_ELEMENT_EMIT_FLOOR` already suppresses every E/T that the
elem-gate's 12 dB hard mask would have killed. The two ghost-suppression
mechanisms operate on disjoint failure modes in principle, but on this
6-sample corpus they overlap completely.

This is a meaningful negative: the prior elem-gate-only result (0.208)
was scored against a non-viterbi baseline, so when stacked on top of
viterbi the elem-gate stops paying its way.

### 2. Soft-stack smooths the elem-gate cliff but shifts the trade

The round-2 elem-gate branch reported a sharp threshold cliff at
`DITDAH_ELEM_GATE_DB`: 11 dB no-op, 12 dB sweet spot, 13 dB drops legitimate
dits. The soft variant replaces this step with a smooth ramp.

Lambda sweep against the two driving samples:

| λ | mean CER | aa6pw CER | arrl-20wpm CER |
| ---: | ---: | ---: | ---: |
| 0.5 | 0.2059 | 0.155 | 0.414 |
| 1.0 | 0.2059 | 0.155 | 0.414 |
| 1.5 | 0.2059 | 0.155 | 0.414 |
| 2.0 | 0.2022 | 0.167 | 0.379 |

A wide λ ∈ [0.5, 1.5] plateau hits the aa6pw improvement; at λ ≥ 2 the
SNR term dominates enough that borderline elements are *also* killed
by `ELEMENT_NOISE_FLOOR` and the result reverts to the viterbi baseline
exactly. So yes, the cliff is smoothed — but only in the sense that
several λ values reach the same operating point. The trade between
"keep helpful weak E in aa6pw" and "drop ghost trailing T in arrl-20"
is irreducible at any single λ value because their SNR-conditioned
scores collide in the same neighborhood.

### 3. Abstain-stack is dominated by soft-stack on this bench

Abstain sweep at λ=1.0:

| abstain | mean CER | aa6pw CER | arrl-20wpm CER | notes |
| ---: | ---: | ---: | ---: | --- |
| -3 | 0.2022 | 0.167 | 0.379 | Drops both ghost T *and* helpful E (matches viterbi exactly) |
| -4 | 0.2059 | 0.155 | 0.414 | Same as no abstain |
| -5 | 0.2059 | 0.155 | 0.414 | Same as no abstain |
| -8 | 0.2022 | 0.167 | 0.379 | Suppresses different elements; coincidentally matches viterbi |

The abstain knob is bimodal on this corpus: either it's tight enough to
catch the trailing T ghost in arrl-20wpm (and also kill the helpful E in
aa6pw), or it's loose enough that nothing is suppressed and behavior
matches plain soft-stack. There is no abstain threshold that splits the
two.

This means abstain adds zero useful capacity on top of soft on this
bench: every operating point the abstain knob can reach is already
reachable by another (variant, λ) combination.

### 4. Clean-sample regression check (4 ARRL samples)

| Sample | viterbi | soft λ=1.0 | Δ |
| --- | ---: | ---: | ---: |
| arrl-13wpm-farnsworth | 0.385 | 0.385 | 0.000 |
| arrl-20wpm            | 0.379 | **0.414** | **+0.035** |
| arrl-30wpm            | 0.056 | 0.056 | 0.000 |
| arrl-40wpm            | 0.052 | 0.052 | 0.000 |

Soft-stack regresses arrl-20wpm by exactly one trailing character (a
ghost "T" the SNR term keeps that the viterbi-only flush correctly
drops). The other three ARRL samples are byte-identical. The 13/30/40
WPM signals are clean enough that no ghost suppression is exercised at
all.

## Recommendation: **ship viterbi-only, do not merge stack variants**

- No variant gives a clean win on the success criterion (mean CER < 0.202 without ARRL regression).
- Hard-stack is byte-identical to viterbi-only on this corpus. Keep it as a `DITDAH_ELEM_GATE_DB` diagnostic env var, but it shouldn't change defaults.
- Soft-stack hits the stretch goal on aa6pw (0.155 < 0.167) but pays for it with a one-character regression on arrl-20wpm. Net mean CER: -0.0037 (worse).
- Abstain-stack is dominated by soft-stack on this bench. The abstain threshold is bimodal: it either does nothing or also kills helpful single elements.

The user's framed worry — "the elem-gate cliff is a sign the gate should
be a likelihood term, not a hard mask" — is at least partially confirmed:
the soft term *does* smooth the cliff and *does* extract a gain on
aa6pw. But on the rest of the corpus the elem-gate isn't doing any work
the viterbi flush isn't already doing, so smoothing it doesn't move the
needle. The aa6pw gain is real but isolated.

A larger / messier corpus would be needed to tell whether soft-stack's
aa6pw gain generalizes; on these six samples it does not.

The implementation is committed and gated; flipping `DITDAH_STACK=soft`
in production is a one-env-var change if a future corpus or operator
decides the aa6pw-style gain is worth the arrl-20wpm regression.

## Honest negative on threshold-cliff smoothing

The soft variant *is* mathematically smoother than the elem-gate hard
mask: the linear-in-dB LLR replaces the step. But the *behavior* is
not as smooth as that suggests, because:

- At low λ the SNR term is a small additive bias that rarely flips a
  decision (everything matches viterbi-only or matches soft-1.0 plateau).
- At high λ the SNR term dominates and pushes any borderline element
  below `ELEMENT_NOISE_FLOOR = -3`, which is itself a hard step that
  drops the element entirely.

So the cliff isn't eliminated; it's relocated downstream to the
`ELEMENT_NOISE_FLOOR` check. A future iteration could replace the
floor with a soft Bayesian "is this element real" decision, but that's
out of scope for this branch.

## Reproduce

```powershell
cd C:\Users\randy\Git\qsoripper-experiments\vit-elem-stack\experiments\cw-decoder
cargo build --release --bin cw-decoder

cd ..\..
$env:DITDAH_STACK="viterbi"; python bench.py .
$env:DITDAH_STACK="hard"; python bench.py .
$env:DITDAH_STACK="soft"; $env:DITDAH_STACK_LAMBDA="1.0"; python bench.py .
$env:DITDAH_STACK="abstain"; $env:DITDAH_STACK_LAMBDA="1.0"; $env:DITDAH_STACK_ABSTAIN="-5"; python bench.py .
```
