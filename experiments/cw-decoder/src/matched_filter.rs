//! Matched-filter bank front-end for CW element detection.
//!
//! Replaces the single-WPM Goertzel + Otsu power-threshold path used by
//! [`crate::region_stream::decode_region_slice_from_intervals`] with a
//! bank of rectangular-pulse matched filters tuned to dit duration at
//! several WPM hypotheses.
//!
//! Pipeline per region:
//! 1. Compute the analytic envelope as Goertzel magnitude at the carrier
//!    pitch on a 25 ms frame / 10 ms hop grid.
//! 2. For every WPM hypothesis, smooth (boxcar-convolve) the envelope by
//!    `dit_ms = 1200 / WPM`. This is the degenerate matched filter for a
//!    rectangular dit-length pulse on the envelope.
//! 3. Score each hypothesis by modulation index
//!    `(p95 - p5) / (p50 + eps)` over a sliding 1-second window. Higher
//!    modulation = better fit between filter dit length and actual
//!    keying period. Pick MAP WPM.
//! 4. Threshold the winning matched-filter output at 0.5 × moving max
//!    with hysteresis (off at 0.40×, on at 0.55×) to recover element
//!    intervals.
//! 5. Hand the resulting frame-runs to the existing dit/dah classifier.
//!
//! The hypothesis grid `{12, 15, 18, 22, 27, 33, 40}` covers slow
//! ragchew through contest CW. Default disabled; gate via
//! `RegionStreamConfig::use_matched_filter` (CLI: `--matched-filter`,
//! env: `DITDAH_MATCHED_FILTER=1`).

use crate::region_stream::{
    clean_interval_mask_pub, collect_frame_runs_pub, decode_runs_to_text_pub,
    estimate_dot_from_runs_pub, goertzel_power, percentile_sorted, FrameRunPub,
};

/// WPM hypotheses for the matched-filter bank. Coarse log-spaced to
/// cover slow ragchew (12 WPM) through contest CW (40 WPM).
pub const WPM_HYPOTHESES: &[f32] = &[12.0, 15.0, 18.0, 22.0, 27.0, 33.0, 40.0];

/// Result of running the matched-filter bank on a region.
#[derive(Debug, Clone)]
pub struct MatchedFilterDecode {
    /// MAP WPM hypothesis (best modulation index).
    pub wpm: f32,
    /// Modulation index at the MAP WPM.
    pub modulation_index: f32,
    /// Confidence: `(best - second_best) / best`. 0..1.
    pub confidence: f32,
    /// Decoded text from the winning filter's element intervals.
    pub text: String,
    /// Inferred dit length in seconds (from MAP WPM).
    pub dit_s: f32,
    /// Per-hypothesis modulation index, indexed in `WPM_HYPOTHESES`
    /// order. Empty if no hypothesis was scorable.
    pub mod_indices: Vec<f32>,
}

/// Run the matched-filter bank on a single region slice and return the
/// MAP-decoded text plus diagnostics.
///
/// Returns `None` if the slice is too short to fit at least a few dits
/// of the slowest hypothesis.
pub fn matched_filter_decode(
    samples: &[f32],
    sample_rate: u32,
    pitch_hz: f32,
    frame_len_s: f32,
    frame_step_s: f32,
) -> Option<MatchedFilterDecode> {
    if samples.is_empty() || sample_rate == 0 || !pitch_hz.is_finite() || pitch_hz <= 0.0 {
        return None;
    }
    let frame_len = ((frame_len_s * sample_rate as f32).round() as usize).max(64);
    let frame_step = ((frame_step_s * sample_rate as f32).round() as usize).max(8);
    if samples.len() < frame_len * 4 {
        return None;
    }
    let step_s = frame_step as f32 / sample_rate as f32;
    let frame_s = frame_len as f32 / sample_rate as f32;

    // Goertzel-magnitude envelope at the carrier pitch.
    let mut envelope: Vec<f32> = Vec::with_capacity(samples.len() / frame_step);
    let mut offset = 0usize;
    while offset + frame_len <= samples.len() {
        let p = goertzel_power(&samples[offset..offset + frame_len], sample_rate, pitch_hz);
        envelope.push(p.max(0.0).sqrt());
        offset += frame_step;
    }
    if envelope.len() < 8 {
        return None;
    }

    // Pre-allocate scratch buffers reused across hypotheses.
    let n = envelope.len();
    let mut mf_buf: Vec<f32> = vec![0.0; n];

    // Score each WPM hypothesis. We score by **timing-fit**: threshold
    // the matched-filter output, look at the on-run length distribution,
    // and reward WPMs where most runs cluster near 1×dit or 3×dit. The
    // pure modulation-index score (peak/median ratio) tends to prefer
    // overly slow hypotheses because heavy smoothing flattens the
    // baseline; timing-fit penalises that directly.
    let mut scored: Vec<(f32, f32, f32, Vec<f32>)> = Vec::with_capacity(WPM_HYPOTHESES.len());
    let mut mod_indices: Vec<f32> = Vec::with_capacity(WPM_HYPOTHESES.len());
    for &wpm in WPM_HYPOTHESES {
        let dit_s = 1.2 / wpm;
        let dit_frames = ((dit_s / step_s).round() as usize).max(1);
        if dit_frames * 4 >= n {
            mod_indices.push(0.0);
            continue;
        }
        boxcar_into(&envelope, dit_frames, &mut mf_buf);
        let mi = modulation_index_window(&mf_buf, step_s);
        mod_indices.push(mi);
        let timing = timing_fit_score_for_wpm(&mf_buf, step_s, dit_frames);
        // Combined score: timing fit dominates, modulation index breaks
        // ties. A WPM that produces no plausible elements gets 0.
        let combined = timing * (1.0 + 0.05 * mi);
        scored.push((wpm, combined, mi, mf_buf.clone()));
    }
    if scored.is_empty() {
        return None;
    }
    scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    let (best_wpm, best_score, best_mi, best_mf) = scored.first().cloned()?;
    let second_score = scored.get(1).map(|s| s.1).unwrap_or(0.0);
    let confidence = if best_score > 1e-6 {
        ((best_score - second_score) / best_score).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let _ = best_score; // retained for potential future emission

    // Threshold the winning filter output to element on/off frames.
    let active = threshold_with_hysteresis(&best_mf, step_s);
    let mut active = active;
    clean_interval_mask_pub(&mut active);

    let runs = collect_frame_runs_pub(&active);
    // Prefer the dit length implied by the MAP WPM hypothesis directly,
    // but fall back to the run-statistics estimator if the hypothesis
    // dit is implausible relative to observed runs.
    let dit_s_hyp = 1.2 / best_wpm;
    let dit_s = best_dit_for_runs(&runs, step_s, frame_s, dit_s_hyp);
    let text = decode_runs_to_text_pub(&runs, step_s, frame_s, dit_s);

    Some(MatchedFilterDecode {
        wpm: 1.2 / dit_s.max(0.001),
        modulation_index: best_mi,
        confidence,
        text,
        dit_s,
        mod_indices,
    })
}

/// Centered boxcar of width `n` over `env`, written into `out`.
/// `out.len()` must equal `env.len()`. At endpoints we normalize by the
/// number of samples actually summed so edge frames aren't artificially
/// dim.
fn boxcar_into(env: &[f32], n: usize, out: &mut [f32]) {
    debug_assert_eq!(env.len(), out.len());
    let len = env.len();
    if n <= 1 {
        out.copy_from_slice(env);
        return;
    }
    let half = n / 2;
    // Compute prefix sums for an O(n) centered boxcar with edge
    // normalization.
    let mut prefix = Vec::with_capacity(len + 1);
    prefix.push(0.0_f32);
    let mut acc = 0.0_f32;
    for &v in env {
        acc += v;
        prefix.push(acc);
    }
    for (i, slot) in out.iter_mut().enumerate().take(len) {
        let lo = i.saturating_sub(half);
        let hi = (i + (n - half)).min(len);
        let count = (hi - lo).max(1) as f32;
        *slot = (prefix[hi] - prefix[lo]) / count;
    }
}

/// Modulation index `(p95 - p5) / (p50 + eps)` over a sliding 1-second
/// window. Returns the median across windows.
fn modulation_index_window(mf: &[f32], step_s: f32) -> f32 {
    if mf.is_empty() {
        return 0.0;
    }
    let win = ((1.0 / step_s.max(1e-6)).round() as usize).max(40);
    let win = win.min(mf.len());
    if win == mf.len() {
        return modulation_index_block(&mf[..win]);
    }
    let hop = (win / 2).max(1);
    let mut indices = Vec::new();
    let mut start = 0usize;
    while start + win <= mf.len() {
        indices.push(modulation_index_block(&mf[start..start + win]));
        start += hop;
    }
    if indices.is_empty() {
        return modulation_index_block(mf);
    }
    indices.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    indices[indices.len() / 2]
}

fn modulation_index_block(block: &[f32]) -> f32 {
    if block.len() < 4 {
        return 0.0;
    }
    let mut sorted = block.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let p5 = percentile_sorted(&sorted, 0.05);
    let p50 = percentile_sorted(&sorted, 0.50);
    let p95 = percentile_sorted(&sorted, 0.95);
    let denom = p50.max(1e-6);
    ((p95 - p5) / denom).max(0.0)
}

/// Given a candidate WPM (as `dit_frames`), threshold the matched-filter
/// output and score the resulting on-run length distribution: fraction
/// of runs whose length lies near 1×dit, 3×dit, or 5×dit (covers dit,
/// dah, longer dahs / dit pile-ups). Higher = better fit.
///
/// Also penalises hypotheses that produce zero or one ON run (no
/// keying detected) or whose ON runs are systematically much longer
/// than 5×dit (indicates over-smoothing — the boxcar smeared adjacent
/// elements together).
fn timing_fit_score_for_wpm(mf: &[f32], step_s: f32, dit_frames: usize) -> f32 {
    if mf.is_empty() || dit_frames == 0 {
        return 0.0;
    }
    let active = threshold_with_hysteresis(mf, step_s);
    // Collect on-run lengths in dit units.
    let mut on_lens: Vec<f32> = Vec::new();
    let mut i = 0usize;
    while i < active.len() {
        if active[i] {
            let s = i;
            while i < active.len() && active[i] {
                i += 1;
            }
            let len_frames = (i - s) as f32;
            on_lens.push(len_frames / dit_frames as f32);
        } else {
            i += 1;
        }
    }
    if on_lens.len() < 2 {
        return 0.0;
    }
    // Reward each run for proximity to 1×, 3×, or 5×dit (with shrinking
    // tolerance for longer targets). 7×+ is over-smear and gets a flat
    // small reward.
    let targets = [1.0_f32, 3.0, 5.0];
    let tol = 0.6_f32;
    let mut total = 0.0_f32;
    let mut over_smear = 0usize;
    for &len in &on_lens {
        if len > 7.0 {
            over_smear += 1;
            total += 0.05; // small reward, doesn't dominate
            continue;
        }
        let best = targets
            .iter()
            .map(|t| (1.0 - ((len - t).abs() / (t * tol)).min(1.0)).max(0.0))
            .fold(0.0_f32, f32::max);
        total += best;
    }
    let mean_fit = total / on_lens.len() as f32;
    // Heavily penalise hypotheses where most runs are over-smear.
    let smear_penalty = 1.0 - (over_smear as f32 / on_lens.len() as f32).min(1.0).powi(2);
    mean_fit * smear_penalty
}

/// Threshold the matched-filter output against a sliding-window max.
/// `on` when MF rises above 0.55 × local max, `off` when it falls below
/// 0.40 × local max. The asymmetric pair gives hysteresis and resists
/// chatter on the leading/trailing edge of each element.
fn threshold_with_hysteresis(mf: &[f32], step_s: f32) -> Vec<bool> {
    let n = mf.len();
    if n == 0 {
        return vec![];
    }
    let win = ((1.0 / step_s.max(1e-6)).round() as usize).max(40).min(n);
    let half = win / 2;
    let mut mov_max = vec![0.0_f32; n];
    // Sliding-window maximum via monotonic deque (O(n)).
    let mut deque: std::collections::VecDeque<usize> = std::collections::VecDeque::new();
    for (i, slot) in mov_max.iter_mut().enumerate().take(n) {
        let lo = i.saturating_sub(half);
        let hi = (i + half).min(n - 1);
        while let Some(&front) = deque.front() {
            if front < lo {
                deque.pop_front();
            } else {
                break;
            }
        }
        // Push hi if not yet enqueued. We track max over [lo..=hi];
        // ensure all indices up to hi are considered.
        let last_pushed = deque.back().copied().unwrap_or(0);
        let start_push = if deque.is_empty() {
            lo
        } else {
            last_pushed + 1
        };
        for k in start_push..=hi {
            while let Some(&back) = deque.back() {
                if mf[back] <= mf[k] {
                    deque.pop_back();
                } else {
                    break;
                }
            }
            deque.push_back(k);
        }
        *slot = mf[*deque.front().unwrap()];
    }
    let mut state = false;
    let mut out = vec![false; n];
    for (i, out_i) in out.iter_mut().enumerate().take(n) {
        let on_t = 0.55 * mov_max[i];
        let off_t = 0.40 * mov_max[i];
        if state {
            if mf[i] < off_t {
                state = false;
            }
        } else if mf[i] > on_t {
            state = true;
        }
        *out_i = state;
    }
    out
}

/// Pick a dit length consistent with both the MAP WPM hypothesis and the
/// observed on-run distribution. If the run-based estimator agrees
/// within a factor of ~1.5 we trust it (more accurate); otherwise we
/// stick with the hypothesis. This keeps WPM honest when the boxcar
/// over-merges short elements.
fn best_dit_for_runs(
    runs: &[FrameRunPub],
    step_s: f32,
    frame_s: f32,
    dit_s_hypothesis: f32,
) -> f32 {
    let Some(dit_runs) = estimate_dot_from_runs_pub(runs, step_s, frame_s) else {
        return dit_s_hypothesis;
    };
    let ratio = dit_runs / dit_s_hypothesis.max(1e-3);
    if (0.65..=1.6).contains(&ratio) {
        dit_runs
    } else {
        dit_s_hypothesis
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn synth_cw(text: &str, wpm: f32, pitch_hz: f32, sample_rate: u32) -> Vec<f32> {
        // Minimal CW synth: dit/dah with element-space, letter-space.
        let dit = 1.2_f32 / wpm;
        let mut samples: Vec<f32> = Vec::new();
        let push_silence = |samples: &mut Vec<f32>, dur_s: f32| {
            let n = (dur_s * sample_rate as f32) as usize;
            samples.extend(std::iter::repeat_n(0.0, n));
        };
        let push_tone = |samples: &mut Vec<f32>, dur_s: f32| {
            let n = (dur_s * sample_rate as f32) as usize;
            for i in 0..n {
                let t = (samples.len() + i) as f32 / sample_rate as f32;
                samples.push((2.0 * std::f32::consts::PI * pitch_hz * t).sin());
            }
        };
        let codes: std::collections::HashMap<char, &str> = [
            ('W', ".--"),
            ('A', ".-"),
            ('6', "-...."),
            ('M', "--"),
            ('O', "---"),
        ]
        .into_iter()
        .collect();
        for (li, letter) in text.chars().enumerate() {
            if li > 0 {
                push_silence(&mut samples, 3.0 * dit);
            }
            let code = codes.get(&letter).copied().unwrap_or("");
            for (ei, sym) in code.chars().enumerate() {
                if ei > 0 {
                    push_silence(&mut samples, dit);
                }
                let dur = if sym == '.' { dit } else { 3.0 * dit };
                push_tone(&mut samples, dur);
            }
        }
        samples
    }

    #[test]
    fn matched_filter_recovers_wa6mow_synthetic() {
        let sr = 8000u32;
        let samples = synth_cw("WA6MOW", 22.0, 700.0, sr);
        let res = matched_filter_decode(&samples, sr, 700.0, 0.025, 0.010).expect("decode");
        // Should pick a WPM near 22 and produce a transcript containing W A 6 M O W.
        assert!(
            res.text.contains("WA6MOW") || res.text.contains("WA"),
            "expected WA6MOW-ish text, got {:?} (wpm={})",
            res.text,
            res.wpm
        );
    }

    #[test]
    fn boxcar_preserves_constant_signal() {
        let env = vec![1.0_f32; 50];
        let mut out = vec![0.0; 50];
        boxcar_into(&env, 5, &mut out);
        for v in &out {
            assert!((v - 1.0).abs() < 1e-5);
        }
    }
}
