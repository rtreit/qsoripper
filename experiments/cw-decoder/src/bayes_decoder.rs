//! Hierarchical Bayesian joint estimator over (WPM drift, dit/dah class) for
//! a single keyed region.
//!
//! The prior model is a slow random walk in log-domain dit length:
//!
//!     log_dot_t = log_dot_{t-1} + N(0, sigma_walk)
//!
//! Pitch and SNR are technically part of the joint state too, but pitch is
//! pinned upstream by the region-stream pitch lock and SNR is folded into the
//! observation noise sigma, so this filter focuses inference compute on the
//! WPM dimension where the *intra-region* drift matters most.
//!
//! Per element, the observation model is Gaussian on log-duration. For an
//! on-run with duration d_n the dit/dah likelihoods are
//! `N(log d_n; log_dot_t, sigma_on)` and `N(log d_n; log_dot_t + ln 3, sigma_on)`
//! respectively. For an off-run with duration g_n the intra-element / letter /
//! word gap likelihoods are `N(log g_n; log_dot_t, sigma_gap)`,
//! `N(log g_n; log_dot_t + ln 3, sigma_gap)`, and
//! `N(log g_n; log_dot_t + ln 7, sigma_word)`.
//!
//! Inference is done with a tiny particle filter (default 64 particles) in
//! log_dot space. After the latent-state weight update we read out the marginal
//! posterior over class for that element by integrating the per-class
//! likelihood against the (post-update) particle distribution.
//!
//! The output of the filter is a class label per run; we then apply the
//! standard ditdah morse map and word-break heuristics. The whole thing
//! is gated behind `DITDAH_BAYES=1` so the default decode path is unchanged.

use std::sync::OnceLock;

const PARTICLE_COUNT: usize = 64;

const SIGMA_WALK: f32 = 0.05;
const SIGMA_ON: f32 = 0.30;
const SIGMA_GAP: f32 = 0.40;
const SIGMA_WORD: f32 = 0.60;
const INIT_LOG_SPREAD: f32 = 0.25;

const LN_3: f32 = 1.098_612_3;
const LN_7: f32 = 1.945_910_2;

/// One observed element from the front-end. Duration is always strictly
/// positive seconds; `active` separates dit/dah candidates from gap candidates.
#[derive(Debug, Clone, Copy)]
pub struct ElementObservation {
    pub active: bool,
    pub duration_s: f32,
}

/// Per-element marginal posterior, returned by [`run_bayes_filter`].
///
/// For `active=true` runs the (`p_intra_gap`, `p_letter_gap`, `p_word_gap`)
/// fields are zero. For `active=false` runs the dit/dah probabilities are
/// zero.
#[derive(Debug, Clone, Copy)]
pub struct ElementPosterior {
    pub active: bool,
    pub p_dit: f32,
    pub p_dah: f32,
    pub p_intra_gap: f32,
    pub p_letter_gap: f32,
    pub p_word_gap: f32,
    /// Particle-mean log_dot in seconds *after* this element. Useful for
    /// diagnostics on WPM drift across a region.
    pub log_dot_mean: f32,
}

/// Returns true when Bayes joint inference has been enabled via env var.
pub fn enabled() -> bool {
    static FLAG: OnceLock<bool> = OnceLock::new();
    *FLAG.get_or_init(|| {
        std::env::var("DITDAH_BAYES")
            .map(|v| {
                let v = v.trim();
                !v.is_empty() && v != "0" && !v.eq_ignore_ascii_case("false")
            })
            .unwrap_or(false)
    })
}

/// Returns true when verbose Bayes diagnostics should be emitted to stderr.
pub fn debug_enabled() -> bool {
    static FLAG: OnceLock<bool> = OnceLock::new();
    *FLAG.get_or_init(|| {
        std::env::var("DITDAH_BAYES_DEBUG")
            .map(|v| {
                let v = v.trim();
                !v.is_empty() && v != "0" && !v.eq_ignore_ascii_case("false")
            })
            .unwrap_or(false)
    })
}

/// Run the particle filter across `obs` and return the per-element posterior.
/// This is the single-pass primitive; most callers will prefer
/// [`run_bayes_filter_calibrated`], which auto-recovers from a bad initial
/// dot length by running the filter twice.
pub fn run_bayes_filter(obs: &[ElementObservation], init_dot_s: f32) -> Vec<ElementPosterior> {
    if obs.is_empty() {
        return Vec::new();
    }
    let init_log = init_dot_s.max(1e-4).ln();

    let mut rng = XorShift::new(0xCAFE_BABE_DEAD_BEEF);
    let mut log_dots: [f32; PARTICLE_COUNT] = [0.0; PARTICLE_COUNT];
    for ld in &mut log_dots {
        *ld = init_log + rng.normal() * INIT_LOG_SPREAD;
    }
    let mut weights: [f32; PARTICLE_COUNT] = [1.0 / PARTICLE_COUNT as f32; PARTICLE_COUNT];

    let mut out = Vec::with_capacity(obs.len());

    for o in obs {
        // Random-walk diffusion of latent log_dot.
        for ld in &mut log_dots {
            *ld += rng.normal() * SIGMA_WALK;
        }

        let log_d = o.duration_s.max(1e-4).ln();

        if o.active {
            // Marginal likelihood per particle and reweight.
            let mut new_w = [0.0_f32; PARTICLE_COUNT];
            for i in 0..PARTICLE_COUNT {
                let l_dit = gauss_log_pdf(log_d, log_dots[i], SIGMA_ON);
                let l_dah = gauss_log_pdf(log_d, log_dots[i] + LN_3, SIGMA_ON);
                let lik = log_sum_exp2(l_dit, l_dah);
                new_w[i] = weights[i] * lik.exp();
            }
            normalize_weights(&mut new_w);
            weights = new_w;

            // Marginal class posterior using post-update weights.
            let mut p_dit_acc = 0.0_f32;
            let mut p_dah_acc = 0.0_f32;
            for i in 0..PARTICLE_COUNT {
                let l_dit = gauss_log_pdf(log_d, log_dots[i], SIGMA_ON);
                let l_dah = gauss_log_pdf(log_d, log_dots[i] + LN_3, SIGMA_ON);
                let m = l_dit.max(l_dah);
                let pd = (l_dit - m).exp();
                let pa = (l_dah - m).exp();
                let s = (pd + pa).max(1e-30);
                p_dit_acc += weights[i] * pd / s;
                p_dah_acc += weights[i] * pa / s;
            }

            out.push(ElementPosterior {
                active: true,
                p_dit: p_dit_acc,
                p_dah: p_dah_acc,
                p_intra_gap: 0.0,
                p_letter_gap: 0.0,
                p_word_gap: 0.0,
                log_dot_mean: weighted_mean(&log_dots, &weights),
            });
        } else {
            let mut new_w = [0.0_f32; PARTICLE_COUNT];
            for i in 0..PARTICLE_COUNT {
                let l_intra = gauss_log_pdf(log_d, log_dots[i], SIGMA_GAP);
                let l_letter = gauss_log_pdf(log_d, log_dots[i] + LN_3, SIGMA_GAP);
                let l_word = gauss_log_pdf(log_d, log_dots[i] + LN_7, SIGMA_WORD);
                let lik = log_sum_exp3(l_intra, l_letter, l_word);
                new_w[i] = weights[i] * lik.exp();
            }
            normalize_weights(&mut new_w);
            weights = new_w;

            let mut p_i = 0.0_f32;
            let mut p_l = 0.0_f32;
            let mut p_w = 0.0_f32;
            for i in 0..PARTICLE_COUNT {
                let l_intra = gauss_log_pdf(log_d, log_dots[i], SIGMA_GAP);
                let l_letter = gauss_log_pdf(log_d, log_dots[i] + LN_3, SIGMA_GAP);
                let l_word = gauss_log_pdf(log_d, log_dots[i] + LN_7, SIGMA_WORD);
                let m = l_intra.max(l_letter).max(l_word);
                let a = (l_intra - m).exp();
                let b = (l_letter - m).exp();
                let c = (l_word - m).exp();
                let s = (a + b + c).max(1e-30);
                p_i += weights[i] * a / s;
                p_l += weights[i] * b / s;
                p_w += weights[i] * c / s;
            }

            out.push(ElementPosterior {
                active: false,
                p_dit: 0.0,
                p_dah: 0.0,
                p_intra_gap: p_i,
                p_letter_gap: p_l,
                p_word_gap: p_w,
                log_dot_mean: weighted_mean(&log_dots, &weights),
            });
        }

        // Resample if effective sample size collapses.
        let ess = effective_sample_size(&weights);
        if ess < (PARTICLE_COUNT as f32) * 0.5 {
            systematic_resample(&mut log_dots, &mut weights, &mut rng);
        }
    }

    out
}

/// Two-pass calibrated filter.
///
/// The front-end estimate of `dot_s` (median of short on-runs) can be off by
/// a factor of ~2 on signals where copy-quality is borderline — exactly the
/// regime where the Bayes filter is most useful. Running the filter forward
/// once gives us a much better steady-state log_dot estimate (the median of
/// the per-element posterior means in the *back half* of the region). We
/// then re-seed the filter with that estimate and run it forward a second
/// time to get the final per-element posteriors.
pub fn run_bayes_filter_calibrated(
    obs: &[ElementObservation],
    init_dot_s: f32,
) -> Vec<ElementPosterior> {
    if obs.is_empty() {
        return Vec::new();
    }
    let pass1 = run_bayes_filter(obs, init_dot_s);
    let half = pass1.len() / 2;
    let tail = &pass1[half..];
    let mut log_dots: Vec<f32> = tail.iter().map(|p| p.log_dot_mean).collect();
    log_dots.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let median_log = log_dots[log_dots.len() / 2];
    let calibrated_dot = median_log.exp();
    // Sanity-clamp the calibrated dot to a plausible CW range (4 WPM .. 60 WPM).
    let clamped = calibrated_dot.clamp(0.020, 0.300);
    let init_diff = (clamped.ln() - init_dot_s.max(1e-4).ln()).abs();
    if init_diff < 0.10 {
        // Filter agrees with the front-end init within ~10% in log-domain →
        // pass 1 is already well-calibrated, no need to spend the second
        // pass.
        return pass1;
    }
    run_bayes_filter(obs, clamped)
}

/// Marginal posterior → text. Each active run becomes '.' or '-'; gaps decide
/// letter / word splits via MAP. We use a tiny prior boost on intra-element
/// gaps because they are the most common gap class in any English-Morse
/// region, and an explicit tie-break that prefers the shorter gap class
/// when posteriors are within 5% of each other (avoids over-fragmenting
/// runs whose gaps are fractionally above the 3-unit boundary).
pub fn posteriors_to_text(posts: &[ElementPosterior]) -> String {
    let mut out = String::new();
    let mut current = String::new();

    for p in posts {
        if p.active {
            current.push(if p.p_dah > p.p_dit { '-' } else { '.' });
        } else {
            // MAP among (intra, letter, word) with tiny intra prior bias.
            let intra = p.p_intra_gap * 1.05;
            let letter = p.p_letter_gap;
            let word = p.p_word_gap;
            if word > letter && word > intra {
                push_morse_letter(&mut out, &mut current);
                if !out.ends_with(' ') && !out.is_empty() {
                    out.push(' ');
                }
            } else if letter > intra {
                push_morse_letter(&mut out, &mut current);
            }
            // intra-element gap: keep accumulating into `current`
        }
    }
    push_morse_letter(&mut out, &mut current);
    out
}

fn push_morse_letter(out: &mut String, current: &mut String) {
    if current.is_empty() {
        return;
    }
    if let Some(ch) = morse_to_char(current) {
        out.push(ch);
    } else {
        out.push('*');
    }
    current.clear();
}

fn morse_to_char(s: &str) -> Option<char> {
    match s {
        ".-" => Some('A'),
        "-..." => Some('B'),
        "-.-." => Some('C'),
        "-.." => Some('D'),
        "." => Some('E'),
        "..-." => Some('F'),
        "--." => Some('G'),
        "...." => Some('H'),
        ".." => Some('I'),
        ".---" => Some('J'),
        "-.-" => Some('K'),
        ".-.." => Some('L'),
        "--" => Some('M'),
        "-." => Some('N'),
        "---" => Some('O'),
        ".--." => Some('P'),
        "--.-" => Some('Q'),
        ".-." => Some('R'),
        "..." => Some('S'),
        "-" => Some('T'),
        "..-" => Some('U'),
        "...-" => Some('V'),
        ".--" => Some('W'),
        "-..-" => Some('X'),
        "-.--" => Some('Y'),
        "--.." => Some('Z'),
        ".----" => Some('1'),
        "..---" => Some('2'),
        "...--" => Some('3'),
        "....-" => Some('4'),
        "....." => Some('5'),
        "-...." => Some('6'),
        "--..." => Some('7'),
        "---.." => Some('8'),
        "----." => Some('9'),
        "-----" => Some('0'),
        "-...-" => Some('='),
        "--..--" => Some(','),
        ".-.-.-" => Some('.'),
        "..--.." => Some('?'),
        "-..-." => Some('/'),
        ".-.-." => Some('+'),
        "-....-" => Some('-'),
        ".----." => Some('\''),
        ".-..-." => Some('"'),
        "---..." => Some(':'),
        "-.-.-." => Some(';'),
        "-.--." => Some('('),
        "-.--.-" => Some(')'),
        ".-..." => Some('&'),
        ".--.-." => Some('@'),
        "..--.-" => Some('_'),
        "-.-.--" => Some('!'),
        "...-..-" => Some('$'),
        "...-.-" => Some('<'),
        _ => None,
    }
}

#[inline]
fn gauss_log_pdf(x: f32, mu: f32, sigma: f32) -> f32 {
    // -0.5 * log(2*pi) is a constant we can drop because the filter only
    // cares about *relative* per-particle and per-class likelihoods. We
    // still divide by sigma in the normalizer for shape consistency.
    let z = (x - mu) / sigma;
    -0.5 * z * z - sigma.ln()
}

#[inline]
fn log_sum_exp2(a: f32, b: f32) -> f32 {
    let m = a.max(b);
    m + ((a - m).exp() + (b - m).exp()).ln()
}

#[inline]
fn log_sum_exp3(a: f32, b: f32, c: f32) -> f32 {
    let m = a.max(b).max(c);
    m + ((a - m).exp() + (b - m).exp() + (c - m).exp()).ln()
}

fn normalize_weights(w: &mut [f32]) {
    let sum: f32 = w.iter().sum();
    if sum > 0.0 && sum.is_finite() {
        let inv = 1.0 / sum;
        for x in w.iter_mut() {
            *x *= inv;
        }
    } else {
        // Degenerate: fall back to uniform.
        let n = w.len() as f32;
        for x in w.iter_mut() {
            *x = 1.0 / n;
        }
    }
}

fn effective_sample_size(w: &[f32]) -> f32 {
    let denom: f32 = w.iter().map(|x| x * x).sum();
    if denom > 0.0 {
        1.0 / denom
    } else {
        w.len() as f32
    }
}

fn weighted_mean(values: &[f32], weights: &[f32]) -> f32 {
    let mut acc = 0.0_f32;
    for (v, w) in values.iter().zip(weights.iter()) {
        acc += v * w;
    }
    acc
}

fn systematic_resample(
    log_dots: &mut [f32; PARTICLE_COUNT],
    weights: &mut [f32; PARTICLE_COUNT],
    rng: &mut XorShift,
) {
    let n = PARTICLE_COUNT;
    let inv_n = 1.0 / n as f32;
    let u0 = rng.uniform_unit() * inv_n;
    let mut cum = weights[0];
    let mut j = 0usize;
    let mut new_log = [0.0_f32; PARTICLE_COUNT];
    for (i, slot) in new_log.iter_mut().enumerate() {
        let u = u0 + i as f32 * inv_n;
        while u > cum && j + 1 < n {
            j += 1;
            cum += weights[j];
        }
        *slot = log_dots[j];
    }
    *log_dots = new_log;
    *weights = [inv_n; PARTICLE_COUNT];
}

/// Tiny xorshift64* PRNG with a Box-Muller normal sampler. Deterministic
/// per-region (we always seed identically) so unit tests and bench runs
/// reproduce.
struct XorShift {
    state: u64,
    spare: Option<f32>,
}

impl XorShift {
    fn new(seed: u64) -> Self {
        Self {
            state: if seed == 0 {
                0x9E37_79B9_7F4A_7C15
            } else {
                seed
            },
            spare: None,
        }
    }

    #[inline]
    fn next_u64(&mut self) -> u64 {
        let mut x = self.state;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.state = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    #[inline]
    fn uniform_unit(&mut self) -> f32 {
        // 24 bits → [0, 1)
        ((self.next_u64() >> 40) as f32) / (1u32 << 24) as f32
    }

    #[inline]
    fn normal(&mut self) -> f32 {
        if let Some(s) = self.spare.take() {
            return s;
        }
        // Box–Muller. Avoid u≈0 to keep ln finite.
        let mut u1 = self.uniform_unit();
        if u1 < 1e-7 {
            u1 = 1e-7;
        }
        let u2 = self.uniform_unit();
        let mag = (-2.0 * u1.ln()).sqrt();
        let z0 = mag * (2.0 * std::f32::consts::PI * u2).cos();
        let z1 = mag * (2.0 * std::f32::consts::PI * u2).sin();
        self.spare = Some(z1);
        z0
    }
}

/// Summarize WPM drift across a region for diagnostic output. Returns
/// (start_wpm, end_wpm, range_wpm) where range = max - min across elements.
pub fn wpm_drift_summary(posts: &[ElementPosterior]) -> Option<(f32, f32, f32)> {
    if posts.is_empty() {
        return None;
    }
    let to_wpm = |log_dot: f32| 1.2_f32 / log_dot.exp().max(1e-4);
    let start = to_wpm(posts[0].log_dot_mean);
    let end = to_wpm(posts[posts.len() - 1].log_dot_mean);
    let mut lo = f32::INFINITY;
    let mut hi = f32::NEG_INFINITY;
    for p in posts {
        let w = to_wpm(p.log_dot_mean);
        if w < lo {
            lo = w;
        }
        if w > hi {
            hi = w;
        }
    }
    Some((start, end, hi - lo))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn obs_active(d: f32) -> ElementObservation {
        ElementObservation {
            active: true,
            duration_s: d,
        }
    }
    fn obs_gap(d: f32) -> ElementObservation {
        ElementObservation {
            active: false,
            duration_s: d,
        }
    }

    #[test]
    fn classifies_clean_paris_at_20_wpm() {
        // 20 WPM → dot = 0.060 s, dah = 0.180 s, letter gap = 0.180 s,
        // word gap = 0.420 s. Synthesize "PARIS" which is the canonical
        // 50-unit benchmark.
        // P = .--.   A = .-   R = .-.   I = ..   S = ...
        let dot = 0.060_f32;
        let dah = 3.0 * dot;
        let igap = dot;
        let lgap = 3.0 * dot;
        let wgap = 7.0 * dot;
        let mut o = Vec::new();
        // P
        for c in [".", "-", "-", "."] {
            o.push(if c == "." {
                obs_active(dot)
            } else {
                obs_active(dah)
            });
            o.push(obs_gap(igap));
        }
        // letter
        *o.last_mut().unwrap() = obs_gap(lgap);
        // A
        for c in [".", "-"] {
            o.push(if c == "." {
                obs_active(dot)
            } else {
                obs_active(dah)
            });
            o.push(obs_gap(igap));
        }
        *o.last_mut().unwrap() = obs_gap(lgap);
        // R
        for c in [".", "-", "."] {
            o.push(if c == "." {
                obs_active(dot)
            } else {
                obs_active(dah)
            });
            o.push(obs_gap(igap));
        }
        *o.last_mut().unwrap() = obs_gap(lgap);
        // I
        for c in [".", "."] {
            o.push(if c == "." {
                obs_active(dot)
            } else {
                obs_active(dah)
            });
            o.push(obs_gap(igap));
        }
        *o.last_mut().unwrap() = obs_gap(lgap);
        // S
        for c in [".", ".", "."] {
            o.push(if c == "." {
                obs_active(dot)
            } else {
                obs_active(dah)
            });
            o.push(obs_gap(igap));
        }
        *o.last_mut().unwrap() = obs_gap(wgap);

        let posts = run_bayes_filter(&o, dot);
        let text = posteriors_to_text(&posts);
        assert!(text.starts_with("PARIS"), "bayes decode = {text:?}");
    }

    #[test]
    fn handles_mid_region_speedup() {
        // Start at 18 WPM, drift to 28 WPM mid-stream. Truth: "TEST".
        // T=-, E=., S=..., T=-
        let mut o = Vec::new();
        let push =
            |o: &mut Vec<ElementObservation>, dot: f32, letters: &[&str], finish_word: bool| {
                let dah = 3.0 * dot;
                let igap = dot;
                let lgap = 3.0 * dot;
                let wgap = 7.0 * dot;
                for (li, l) in letters.iter().enumerate() {
                    for (i, ch) in l.chars().enumerate() {
                        o.push(if ch == '.' {
                            obs_active(dot)
                        } else {
                            obs_active(dah)
                        });
                        if i + 1 < l.chars().count() {
                            o.push(obs_gap(igap));
                        }
                    }
                    if li + 1 < letters.len() {
                        o.push(obs_gap(lgap));
                    } else if finish_word {
                        o.push(obs_gap(wgap));
                    } else {
                        o.push(obs_gap(lgap));
                    }
                }
            };
        push(&mut o, 0.0667, &["-", "."], false); // 18 WPM
        push(&mut o, 0.0429, &["...", "-"], true); // 28 WPM

        let posts = run_bayes_filter(&o, 0.060);
        let text = posteriors_to_text(&posts);
        assert!(text.contains("TEST"), "bayes drift decode = {text:?}");
    }
}
