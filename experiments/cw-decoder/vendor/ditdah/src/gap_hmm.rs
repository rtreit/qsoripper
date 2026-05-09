//! Hidden Markov model over CW run-length intervals.
//!
//! The decoder splits a smoothed power signal into alternating on/off
//! intervals; this module replaces the hand-tuned ratio thresholds (was
//! `2.0 * dot_len` for letter break, `5.0 * dot_len` for word break) with a
//! 5-state HMM whose parameters are learned from the ARRL corpus via
//! Baum-Welch.
//!
//! States:
//!   0 = Dit   (short on)
//!   1 = Dah   (long on)
//!   2 = Intra (intra-character gap)
//!   3 = Char  (inter-character gap)
//!   4 = Word  (inter-word gap)
//!
//! Observations: x_i = ln(length_i / dot_len). Each state has a Gaussian
//! emission. Transitions are parity-restricted (on→off, off→on); transitions
//! between states with mismatched parity are clamped to -inf.

#![allow(clippy::needless_range_loop, clippy::uninlined_format_args, clippy::redundant_closure)]


use std::f32::consts::PI;
use std::path::Path;
use std::sync::OnceLock;

pub const NUM_STATES: usize = 5;
pub const STATE_DIT: usize = 0;
pub const STATE_DAH: usize = 1;
pub const STATE_INTRA: usize = 2;
pub const STATE_CHAR: usize = 3;
pub const STATE_WORD: usize = 4;

#[inline]
pub fn is_on_state(s: usize) -> bool {
    s == STATE_DIT || s == STATE_DAH
}

/// HMM parameters. Stored / loaded as a tiny JSON file (<2 KB).
#[derive(Debug, Clone)]
pub struct HmmParams {
    /// Gaussian means in log-ratio space.
    pub mu: [f32; NUM_STATES],
    /// Gaussian std-devs in log-ratio space (clamped >= MIN_SIGMA).
    pub sigma: [f32; NUM_STATES],
    /// log P(s) at sequence start (impossible starts are -inf).
    pub log_pi: [f32; NUM_STATES],
    /// log A[i][j] = log P(state_t = j | state_{t-1} = i).
    pub log_a: [[f32; NUM_STATES]; NUM_STATES],
}

pub const MIN_SIGMA: f32 = 0.20;
const NEG_INF: f32 = f32::NEG_INFINITY;

impl HmmParams {
    /// A reasonable hand-seeded default (mirrors the legacy ratio thresholds).
    /// Used as the EM initialization and as a fallback.
    pub fn seed() -> Self {
        let mu = [
            0.0,            // Dit ~ 1 dot
            (3.0_f32).ln(), // Dah ~ 3 dots
            0.0,            // Intra ~ 1 dot
            (3.0_f32).ln(), // Char ~ 3 dots
            (7.0_f32).ln(), // Word ~ 7 dots
        ];
        let sigma = [0.30, 0.30, 0.30, 0.30, 0.40];
        // Start in any state — the parity mask in Viterbi/forward picks the
        // legal subset based on the polarity of the first observed run.
        let log_pi = [
            (0.40_f32).ln(), // Dit
            (0.40_f32).ln(), // Dah
            (0.06_f32).ln(), // Intra
            (0.10_f32).ln(), // Char
            (0.04_f32).ln(), // Word
        ];

        // Parity-restricted seed transitions.
        let mut a = [[0.0f32; NUM_STATES]; NUM_STATES];
        // From Dit -> off states
        a[STATE_DIT][STATE_INTRA] = 0.55;
        a[STATE_DIT][STATE_CHAR] = 0.40;
        a[STATE_DIT][STATE_WORD] = 0.05;
        // From Dah -> off states
        a[STATE_DAH][STATE_INTRA] = 0.55;
        a[STATE_DAH][STATE_CHAR] = 0.40;
        a[STATE_DAH][STATE_WORD] = 0.05;
        // From off -> on
        a[STATE_INTRA][STATE_DIT] = 0.55;
        a[STATE_INTRA][STATE_DAH] = 0.45;
        a[STATE_CHAR][STATE_DIT] = 0.55;
        a[STATE_CHAR][STATE_DAH] = 0.45;
        a[STATE_WORD][STATE_DIT] = 0.55;
        a[STATE_WORD][STATE_DAH] = 0.45;

        let mut log_a = [[NEG_INF; NUM_STATES]; NUM_STATES];
        for i in 0..NUM_STATES {
            for j in 0..NUM_STATES {
                if a[i][j] > 0.0 {
                    log_a[i][j] = a[i][j].ln();
                }
            }
        }

        HmmParams {
            mu,
            sigma,
            log_pi,
            log_a,
        }
    }

    pub fn to_json(&self) -> String {
        // Hand-rolled JSON to avoid pulling serde into ditdah's deps.
        fn arr<F: Fn(usize) -> f32>(n: usize, f: F) -> String {
            let mut s = String::from("[");
            for i in 0..n {
                if i > 0 {
                    s.push_str(", ");
                }
                let v = f(i);
                if v.is_finite() {
                    s.push_str(&format!("{:.6}", v));
                } else if v == NEG_INF {
                    s.push_str("\"-inf\"");
                } else {
                    s.push_str("\"nan\"");
                }
            }
            s.push(']');
            s
        }

        let mut out = String::new();
        out.push_str("{\n");
        out.push_str("  \"version\": 1,\n");
        out.push_str("  \"states\": [\"Dit\", \"Dah\", \"Intra\", \"Char\", \"Word\"],\n");
        out.push_str(&format!("  \"mu\": {},\n", arr(NUM_STATES, |i| self.mu[i])));
        out.push_str(&format!(
            "  \"sigma\": {},\n",
            arr(NUM_STATES, |i| self.sigma[i])
        ));
        out.push_str(&format!(
            "  \"log_pi\": {},\n",
            arr(NUM_STATES, |i| self.log_pi[i])
        ));
        out.push_str("  \"log_a\": [\n");
        for i in 0..NUM_STATES {
            out.push_str("    ");
            out.push_str(&arr(NUM_STATES, |j| self.log_a[i][j]));
            if i + 1 < NUM_STATES {
                out.push(',');
            }
            out.push('\n');
        }
        out.push_str("  ]\n");
        out.push_str("}\n");
        out
    }

    pub fn from_json(text: &str) -> Result<Self, String> {
        // Tiny purpose-built parser. We rely on the format we emit.
        fn parse_num(tok: &str) -> Result<f32, String> {
            let t = tok.trim().trim_matches(',').trim();
            let t = t.trim_matches('"');
            if t == "-inf" || t == "-Infinity" {
                return Ok(NEG_INF);
            }
            t.parse::<f32>().map_err(|e| format!("bad num '{t}': {e}"))
        }
        fn extract_array<'a>(text: &'a str, key: &str) -> Result<&'a str, String> {
            let needle = format!("\"{key}\"");
            let i = text
                .find(&needle)
                .ok_or_else(|| format!("missing key {key}"))?;
            let rest = &text[i + needle.len()..];
            let lb = rest.find('[').ok_or("missing [")?;
            // Find matching closing bracket (handles single level for flat arrays).
            let mut depth = 0i32;
            let mut end = 0usize;
            for (idx, ch) in rest[lb..].char_indices() {
                match ch {
                    '[' => depth += 1,
                    ']' => {
                        depth -= 1;
                        if depth == 0 {
                            end = lb + idx;
                            break;
                        }
                    }
                    _ => {}
                }
            }
            if end == 0 {
                return Err(format!("unterminated array {key}"));
            }
            Ok(&rest[lb + 1..end])
        }
        fn parse_flat(arr_body: &str) -> Result<Vec<f32>, String> {
            arr_body
                .split(',')
                .map(|t| parse_num(t))
                .collect::<Result<Vec<_>, _>>()
        }

        let mu_v = parse_flat(extract_array(text, "mu")?)?;
        let sigma_v = parse_flat(extract_array(text, "sigma")?)?;
        let pi_v = parse_flat(extract_array(text, "log_pi")?)?;
        if mu_v.len() != NUM_STATES || sigma_v.len() != NUM_STATES || pi_v.len() != NUM_STATES {
            return Err("flat arrays wrong length".to_string());
        }

        // log_a is nested: parse 5 inner rows.
        let body = extract_array(text, "log_a")?;
        // Find each inner [...] in body.
        let mut rows: Vec<Vec<f32>> = Vec::new();
        let mut start = None;
        let mut depth = 0i32;
        for (idx, ch) in body.char_indices() {
            match ch {
                '[' => {
                    if depth == 0 {
                        start = Some(idx + 1);
                    }
                    depth += 1;
                }
                ']' => {
                    depth -= 1;
                    if depth == 0 {
                        let s = start.take().ok_or("malformed log_a")?;
                        let row = parse_flat(&body[s..idx])?;
                        if row.len() != NUM_STATES {
                            return Err("log_a row wrong length".to_string());
                        }
                        rows.push(row);
                    }
                }
                _ => {}
            }
        }
        if rows.len() != NUM_STATES {
            return Err("log_a wrong number of rows".to_string());
        }

        let mut p = HmmParams::seed();
        for i in 0..NUM_STATES {
            p.mu[i] = mu_v[i];
            p.sigma[i] = sigma_v[i].max(MIN_SIGMA);
            p.log_pi[i] = pi_v[i];
            for j in 0..NUM_STATES {
                p.log_a[i][j] = rows[i][j];
            }
        }
        Ok(p)
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        std::fs::write(path, self.to_json())
    }

    pub fn load(path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("read: {e}"))?;
        Self::from_json(&text)
    }

    /// log N(x | mu, sigma)
    #[inline]
    pub fn log_emission(&self, state: usize, x: f32) -> f32 {
        let mu = self.mu[state];
        let sigma = self.sigma[state].max(MIN_SIGMA);
        let z = (x - mu) / sigma;
        -0.5 * z * z - sigma.ln() - 0.5 * (2.0 * PI).ln()
    }
}

/// Globally-installed HMM, populated lazily from the file in `DITDAH_HMM`.
/// Empty/unset env disables the HMM. The path may also be a literal `seed`
/// to use the hand-seeded model (useful for sanity tests without a trained
/// JSON file).
static GLOBAL: OnceLock<Option<HmmParams>> = OnceLock::new();

pub fn global() -> Option<&'static HmmParams> {
    GLOBAL
        .get_or_init(|| {
            let val = std::env::var("DITDAH_HMM").ok()?;
            let v = val.trim();
            if v.is_empty() || v == "0" {
                return None;
            }
            if v == "seed" || v == "1" {
                return Some(HmmParams::seed());
            }
            match HmmParams::load(Path::new(v)) {
                Ok(p) => {
                    log::info!("Loaded HMM gap classifier from {v}");
                    Some(p)
                }
                Err(e) => {
                    log::warn!("DITDAH_HMM={v} but load failed: {e}; falling back to seed");
                    Some(HmmParams::seed())
                }
            }
        })
        .as_ref()
}

/// One run-length observation: the polarity (`is_on`) and the length in
/// power-signal samples.
#[derive(Debug, Clone, Copy)]
pub struct Run {
    pub is_on: bool,
    pub len_samples: usize,
}

/// Viterbi over the parity-restricted HMM. Returns one state index per run.
pub fn viterbi(runs: &[Run], dot_len_samples: f32, p: &HmmParams) -> Vec<usize> {
    let n = runs.len();
    if n == 0 {
        return Vec::new();
    }
    let dot = dot_len_samples.max(1.0);

    let obs: Vec<f32> = runs
        .iter()
        .map(|r| (r.len_samples as f32 / dot).max(1e-3).ln())
        .collect();

    // Mask: state must match parity of run.
    let allowed = |t: usize, s: usize| -> bool {
        let on = is_on_state(s);
        on == runs[t].is_on
    };

    let mut delta = [NEG_INF; NUM_STATES];
    let mut psi: Vec<[usize; NUM_STATES]> = vec![[0; NUM_STATES]; n];

    for s in 0..NUM_STATES {
        if allowed(0, s) {
            delta[s] = p.log_pi[s] + p.log_emission(s, obs[0]);
        }
    }

    for t in 1..n {
        let mut next = [NEG_INF; NUM_STATES];
        for s in 0..NUM_STATES {
            if !allowed(t, s) {
                continue;
            }
            let em = p.log_emission(s, obs[t]);
            let mut best = NEG_INF;
            let mut best_i = 0usize;
            for i in 0..NUM_STATES {
                let v = delta[i] + p.log_a[i][s];
                if v > best {
                    best = v;
                    best_i = i;
                }
            }
            next[s] = best + em;
            psi[t][s] = best_i;
        }
        delta = next;
    }

    let mut last = 0usize;
    let mut best = NEG_INF;
    for s in 0..NUM_STATES {
        if delta[s] > best {
            best = delta[s];
            last = s;
        }
    }
    let mut path = vec![0usize; n];
    path[n - 1] = last;
    for t in (1..n).rev() {
        path[t - 1] = psi[t][path[t]];
    }
    path
}

/// Forward-backward log posteriors `gamma[t][s] = log P(state_t = s | obs)`.
/// Used during EM training. Returns (gamma, log-likelihood, log-xi sums).
fn forward_backward(
    runs: &[Run],
    dot_len_samples: f32,
    p: &HmmParams,
) -> (Vec<[f32; NUM_STATES]>, f32, [[f64; NUM_STATES]; NUM_STATES]) {
    let n = runs.len();
    let dot = dot_len_samples.max(1.0);
    let obs: Vec<f32> = runs
        .iter()
        .map(|r| (r.len_samples as f32 / dot).max(1e-3).ln())
        .collect();
    let allowed = |t: usize, s: usize| is_on_state(s) == runs[t].is_on;

    // Forward (log space).
    let mut alpha: Vec<[f32; NUM_STATES]> = vec![[NEG_INF; NUM_STATES]; n];
    for s in 0..NUM_STATES {
        if allowed(0, s) {
            alpha[0][s] = p.log_pi[s] + p.log_emission(s, obs[0]);
        }
    }
    for t in 1..n {
        for s in 0..NUM_STATES {
            if !allowed(t, s) {
                continue;
            }
            let em = p.log_emission(s, obs[t]);
            let mut acc = NEG_INF;
            for i in 0..NUM_STATES {
                acc = log_add(acc, alpha[t - 1][i] + p.log_a[i][s]);
            }
            alpha[t][s] = acc + em;
        }
    }

    let mut log_lik = NEG_INF;
    for s in 0..NUM_STATES {
        log_lik = log_add(log_lik, alpha[n - 1][s]);
    }

    // Backward.
    let mut beta: Vec<[f32; NUM_STATES]> = vec![[NEG_INF; NUM_STATES]; n];
    for s in 0..NUM_STATES {
        if allowed(n - 1, s) {
            beta[n - 1][s] = 0.0;
        }
    }
    for t in (0..n - 1).rev() {
        for s in 0..NUM_STATES {
            if !allowed(t, s) {
                continue;
            }
            let mut acc = NEG_INF;
            for j in 0..NUM_STATES {
                if !allowed(t + 1, j) {
                    continue;
                }
                let em_next = p.log_emission(j, obs[t + 1]);
                acc = log_add(acc, p.log_a[s][j] + em_next + beta[t + 1][j]);
            }
            beta[t][s] = acc;
        }
    }

    // gamma[t][s] = alpha + beta - log_lik
    let mut gamma: Vec<[f32; NUM_STATES]> = vec![[NEG_INF; NUM_STATES]; n];
    for t in 0..n {
        for s in 0..NUM_STATES {
            if alpha[t][s] != NEG_INF && beta[t][s] != NEG_INF {
                gamma[t][s] = alpha[t][s] + beta[t][s] - log_lik;
            }
        }
    }

    // xi sums in linear space (kept f64 for stability across long sequences).
    let mut xi_sum = [[0.0f64; NUM_STATES]; NUM_STATES];
    for t in 0..n - 1 {
        // Compute per-pair posteriors and accumulate.
        // First compute log-norm of pair.
        let mut row_norm = NEG_INF;
        let mut row = [[NEG_INF; NUM_STATES]; NUM_STATES];
        for i in 0..NUM_STATES {
            if !allowed(t, i) {
                continue;
            }
            for j in 0..NUM_STATES {
                if !allowed(t + 1, j) {
                    continue;
                }
                let em_next = p.log_emission(j, obs[t + 1]);
                let v = alpha[t][i] + p.log_a[i][j] + em_next + beta[t + 1][j] - log_lik;
                row[i][j] = v;
                row_norm = log_add(row_norm, v);
            }
        }
        // Already normalized by log_lik so row_norm should be ~0; ignore re-norm.
        let _ = row_norm;
        for i in 0..NUM_STATES {
            for j in 0..NUM_STATES {
                if row[i][j] != NEG_INF {
                    xi_sum[i][j] += row[i][j].exp() as f64;
                }
            }
        }
    }

    (gamma, log_lik, xi_sum)
}

#[inline]
fn log_add(a: f32, b: f32) -> f32 {
    if a == NEG_INF {
        return b;
    }
    if b == NEG_INF {
        return a;
    }
    let (m, x) = if a > b { (a, b) } else { (b, a) };
    m + (1.0 + (x - m).exp()).ln()
}

/// Sufficient statistics accumulated across an EM iteration.
#[derive(Debug, Clone)]
pub struct EmAccum {
    pub gamma_sum: [f64; NUM_STATES],
    pub gamma_x: [f64; NUM_STATES],
    pub gamma_x2: [f64; NUM_STATES],
    pub xi_sum: [[f64; NUM_STATES]; NUM_STATES],
    pub pi_count: [f64; NUM_STATES],
    pub log_lik: f64,
    pub n_obs: usize,
    pub n_seq: usize,
}

impl EmAccum {
    pub fn new() -> Self {
        Self {
            gamma_sum: [0.0; NUM_STATES],
            gamma_x: [0.0; NUM_STATES],
            gamma_x2: [0.0; NUM_STATES],
            xi_sum: [[0.0; NUM_STATES]; NUM_STATES],
            pi_count: [0.0; NUM_STATES],
            log_lik: 0.0,
            n_obs: 0,
            n_seq: 0,
        }
    }
}

impl Default for EmAccum {
    fn default() -> Self {
        Self::new()
    }
}

/// Run one E-step pass over a batch of training sequences and accumulate
/// sufficient statistics. Returns total log-likelihood.
pub fn em_e_step(sequences: &[(Vec<Run>, f32)], p: &HmmParams, acc: &mut EmAccum) {
    for (runs, dot_len) in sequences {
        if runs.len() < 2 {
            continue;
        }
        let dot = dot_len.max(1.0);
        let obs: Vec<f32> = runs
            .iter()
            .map(|r| (r.len_samples as f32 / dot).max(1e-3).ln())
            .collect();
        let (gamma, ll, xi_sum) = forward_backward(runs, *dot_len, p);
        if !ll.is_finite() {
            continue;
        }
        acc.log_lik += ll as f64;
        acc.n_obs += runs.len();
        acc.n_seq += 1;
        for s in 0..NUM_STATES {
            acc.pi_count[s] += gamma[0][s].exp() as f64;
        }
        for t in 0..runs.len() {
            for s in 0..NUM_STATES {
                let g = gamma[t][s].exp() as f64;
                if !g.is_finite() || g == 0.0 {
                    continue;
                }
                acc.gamma_sum[s] += g;
                acc.gamma_x[s] += g * obs[t] as f64;
                acc.gamma_x2[s] += g * (obs[t] as f64) * (obs[t] as f64);
            }
        }
        for i in 0..NUM_STATES {
            for j in 0..NUM_STATES {
                acc.xi_sum[i][j] += xi_sum[i][j];
            }
        }
    }
}

/// M-step: re-estimate parameters from accumulated stats.
pub fn em_m_step(acc: &EmAccum) -> HmmParams {
    let mut p = HmmParams::seed();
    // Means / variances.
    for s in 0..NUM_STATES {
        if acc.gamma_sum[s] > 1e-6 {
            let mean = acc.gamma_x[s] / acc.gamma_sum[s];
            let var = (acc.gamma_x2[s] / acc.gamma_sum[s] - mean * mean).max(1e-4);
            p.mu[s] = mean as f32;
            p.sigma[s] = (var.sqrt() as f32).max(MIN_SIGMA);
        }
    }
    // Transitions.
    for i in 0..NUM_STATES {
        let row_sum: f64 = acc.xi_sum[i].iter().sum();
        for j in 0..NUM_STATES {
            if row_sum > 1e-9 && acc.xi_sum[i][j] > 0.0 {
                p.log_a[i][j] = ((acc.xi_sum[i][j] / row_sum) as f32).max(1e-9).ln();
            } else {
                p.log_a[i][j] = NEG_INF;
            }
        }
    }
    // Initial.
    let pi_sum: f64 = acc.pi_count.iter().sum();
    for s in 0..NUM_STATES {
        if pi_sum > 1e-9 && acc.pi_count[s] > 0.0 {
            p.log_pi[s] = ((acc.pi_count[s] / pi_sum) as f32).max(1e-9).ln();
        } else {
            p.log_pi[s] = NEG_INF;
        }
    }
    p
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seed_round_trip() {
        let p = HmmParams::seed();
        let s = p.to_json();
        let q = HmmParams::from_json(&s).unwrap();
        for i in 0..NUM_STATES {
            assert!((p.mu[i] - q.mu[i]).abs() < 1e-3);
            assert!((p.sigma[i] - q.sigma[i]).abs() < 1e-3);
            assert!(
                (p.log_pi[i] - q.log_pi[i]).abs() < 1e-3
                    || (p.log_pi[i].is_infinite() && q.log_pi[i].is_infinite())
            );
        }
    }

    #[test]
    fn viterbi_classifies_clean_intervals() {
        // Synthetic: dit, intra, dah, char, dit, word, dit
        let runs = vec![
            Run {
                is_on: true,
                len_samples: 10,
            },
            Run {
                is_on: false,
                len_samples: 10,
            },
            Run {
                is_on: true,
                len_samples: 30,
            },
            Run {
                is_on: false,
                len_samples: 30,
            },
            Run {
                is_on: true,
                len_samples: 10,
            },
            Run {
                is_on: false,
                len_samples: 70,
            },
            Run {
                is_on: true,
                len_samples: 10,
            },
        ];
        let p = HmmParams::seed();
        let path = viterbi(&runs, 10.0, &p);
        assert_eq!(path[0], STATE_DIT);
        assert_eq!(path[1], STATE_INTRA);
        assert_eq!(path[2], STATE_DAH);
        assert_eq!(path[3], STATE_CHAR);
        assert_eq!(path[4], STATE_DIT);
        assert_eq!(path[5], STATE_WORD);
        assert_eq!(path[6], STATE_DIT);
    }
}
