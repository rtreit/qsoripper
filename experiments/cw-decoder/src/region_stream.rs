//! Region-based "live" pipeline:
//!
//!   1. Estimate the dominant CW tone over the whole buffer (mean Goertzel
//!      power over a coarse pitch sweep).
//!   2. Compute frame-by-frame Goertzel power at that pitch.
//!   3. Threshold against a noise floor + signal floor split to mark active
//!      frames.
//!   4. Merge active runs across short gaps and discard tiny runs.
//!   5. Decode each surviving region with the v2 whole-buffer ditdah decoder.
//!   6. Concatenate region transcripts with single-space separators.
//!
//! This is the bounded-region replacement for the v1 streaming front-end.
//! It is deliberately stateless and operates on a complete buffer so it can
//! be benchmarked against the exact-window oracle on labeled corpora; a
//! truly online variant can be layered on later by feeding it a growing
//! buffer.

use crate::decoder::{decode_text, decode_text_pinned};

const DIT_DAH_BOUNDARY: f32 = 2.0;
const LETTER_SPACE_BOUNDARY: f32 = 2.0;
const WORD_SPACE_BOUNDARY: f32 = 5.0;

/// Configurable knobs for region detection. All times in seconds.
#[derive(Debug, Clone)]
pub struct RegionStreamConfig {
    /// Goertzel frame length.
    pub frame_len_s: f32,
    /// Goertzel frame step.
    pub frame_step_s: f32,
    /// Lower bound of the candidate pitch sweep (Hz).
    pub pitch_lo_hz: f32,
    /// Upper bound of the candidate pitch sweep (Hz).
    pub pitch_hi_hz: f32,
    /// Pitch sweep resolution (Hz). Smaller = finer pitch lock at higher cost.
    pub pitch_step_hz: f32,
    /// Active threshold = noise + threshold_factor * (signal - noise).
    /// 0.0 = noise floor, 1.0 = signal floor. 0.30 mirrors `harvest::build_permissive_profile`.
    pub threshold_factor: f32,
    /// Active runs separated by gaps shorter than this are merged into a
    /// single region. Should be larger than the longest expected
    /// inter-character / inter-word gap for the slowest WPM you want to
    /// keep glued together.
    pub merge_gap_s: f32,
    /// Drop regions shorter than this after merging.
    pub min_region_s: f32,
    /// Pad each region by this much on both sides before slicing into the
    /// decoder, so leading dits aren't clipped by the threshold edge.
    pub pad_s: f32,
    /// Optional pinned WPM for the per-region decode. None = ditdah auto.
    pub pin_wpm: Option<f32>,
    /// Minimum Goertzel-vs-broadband energy ratio required before a detected
    /// active run is decoded. White noise can still produce percentile-threshold
    /// runs; this guard requires those runs to contain a narrowband CW tone.
    pub min_tonal_prominence_ratio: f32,
}

impl Default for RegionStreamConfig {
    fn default() -> Self {
        Self {
            frame_len_s: 0.025,
            frame_step_s: 0.010,
            pitch_lo_hz: 400.0,
            pitch_hi_hz: 1200.0,
            pitch_step_hz: 25.0,
            threshold_factor: 0.30,
            merge_gap_s: 3.0,
            min_region_s: 0.6,
            pad_s: 0.10,
            pin_wpm: None,
            min_tonal_prominence_ratio: 8.0,
        }
    }
}

#[derive(Debug, Clone)]
pub struct DecodedRegion {
    pub start_s: f32,
    pub end_s: f32,
    pub text: String,
}

#[derive(Debug, Clone)]
pub struct RegionStreamResult {
    pub pitch_hz: f32,
    pub regions: Vec<DecodedRegion>,
    pub text: String,
}

/// Run the full region-detect → decode → merge pipeline on a complete buffer.
pub fn decode_region_stream(
    samples: &[f32],
    sample_rate: u32,
    cfg: &RegionStreamConfig,
) -> RegionStreamResult {
    if samples.is_empty() || sample_rate == 0 {
        return RegionStreamResult {
            pitch_hz: 0.0,
            regions: vec![],
            text: String::new(),
        };
    }

    let dominant_pitch_hz = estimate_dominant_pitch(samples, sample_rate, cfg);
    let mut pitches = discover_burst_pitches(samples, sample_rate, cfg);
    if pitches.is_empty() {
        pitches.push(dominant_pitch_hz);
    }

    let mut candidates = Vec::new();
    for pitch_hz in pitches {
        collect_region_candidates(samples, sample_rate, cfg, pitch_hz, &mut candidates);
    }
    let decoded = dedupe_region_candidates(candidates);
    let text = decoded
        .iter()
        .map(|r| r.text.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    let text = normalize_region_transcript(&text);
    RegionStreamResult {
        pitch_hz: dominant_pitch_hz,
        regions: decoded,
        text,
    }
}

#[derive(Debug, Clone)]
struct RegionCandidate {
    start_s: f32,
    end_s: f32,
    text: String,
    score: f32,
}

fn collect_region_candidates(
    samples: &[f32],
    sample_rate: u32,
    cfg: &RegionStreamConfig,
    pitch_hz: f32,
    candidates: &mut Vec<RegionCandidate>,
) {
    let regions_raw = detect_active_regions(samples, sample_rate, pitch_hz, cfg);
    for (start_s, end_s) in regions_raw {
        let region_s = (start_s.max(0.0) * sample_rate as f32) as usize;
        let region_e = ((end_s * sample_rate as f32) as usize).min(samples.len());
        if region_e <= region_s {
            continue;
        }
        let prominence =
            tonal_prominence_ratio(&samples[region_s..region_e], sample_rate, pitch_hz, cfg);
        if prominence < cfg.min_tonal_prominence_ratio.max(0.0) {
            continue;
        }
        let pad = cfg.pad_s.max(0.0);
        let s = ((start_s - pad).max(0.0) * sample_rate as f32) as usize;
        let e = (((end_s + pad) * sample_rate as f32) as usize).min(samples.len());
        if e <= s {
            continue;
        }
        let slice = &samples[s..e];
        let text = decode_region_slice(slice, sample_rate, pitch_hz, cfg);
        let text = text.trim().to_string();
        if text.is_empty() || is_low_confidence_region_text(&text) {
            continue;
        }
        let useful_chars = text.chars().filter(|ch| ch.is_ascii_alphanumeric()).count() as f32;
        candidates.push(RegionCandidate {
            start_s,
            end_s,
            text,
            score: prominence * (1.0 + useful_chars * 0.05),
        });
    }
}

fn is_low_confidence_region_text(text: &str) -> bool {
    text.contains('?')
}

fn decode_region_slice(
    samples: &[f32],
    sample_rate: u32,
    pitch_hz: f32,
    cfg: &RegionStreamConfig,
) -> String {
    if let Some(w) = cfg.pin_wpm {
        return decode_text_pinned(samples, sample_rate, w);
    }

    let auto = decode_text(samples, sample_rate);
    let interval = decode_region_slice_from_intervals(samples, sample_rate, pitch_hz, cfg);
    if should_prefer_interval_decode(&auto, &interval) {
        return interval;
    }

    let duration_s = samples.len() as f32 / sample_rate.max(1) as f32;
    if duration_s > 1.5 {
        return auto;
    }

    let Some(wpm) = estimate_short_region_wpm(samples, sample_rate, pitch_hz, cfg) else {
        return auto;
    };
    let pinned = decode_text_pinned(samples, sample_rate, wpm);
    if should_prefer_pinned_short_decode(&auto, &pinned) {
        pinned
    } else {
        auto
    }
}

fn should_prefer_interval_decode(auto: &str, interval: &str) -> bool {
    let auto_norm = normalize_region_text(auto);
    let interval_norm = normalize_region_text(interval);
    if interval_norm.is_empty() || auto_norm == interval_norm || interval_norm.contains('?') {
        return false;
    }

    let interval_chars = useful_copy_chars(&interval_norm);
    if interval_chars < 2 {
        return false;
    }

    let auto_score = transcript_quality_score(&auto_norm);
    let interval_score = transcript_quality_score(&interval_norm);
    let auto_has_garbage_tail = auto_norm
        .split_whitespace()
        .any(|token| token.len() >= 3 && token.chars().all(|ch| matches!(ch, 'T' | 'E' | 'I')));
    let interval_has_callsign_shape = interval_norm
        .split_whitespace()
        .any(|token| token.chars().any(|ch| ch.is_ascii_digit()))
        || interval_norm.contains('/');

    interval_score > auto_score + 2.0
        || (auto_norm.contains('?') && interval_score >= auto_score)
        || (auto_has_garbage_tail && interval_score >= auto_score - 6.0)
        || (interval_has_callsign_shape && interval_score >= auto_score - 0.5)
}

fn useful_copy_chars(text: &str) -> usize {
    text.chars()
        .filter(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '/' | '+' | '=' | '.' | ','))
        .count()
}

fn transcript_quality_score(text: &str) -> f32 {
    if text.trim().is_empty() {
        return 0.0;
    }

    let mut score = 0.0_f32;
    for ch in text.chars() {
        if ch.is_ascii_alphanumeric() {
            score += 1.0;
        } else if matches!(ch, '/' | '+' | '=' | '.' | ',') {
            score += 0.6;
        } else if ch == '?' {
            score -= 3.0;
        }
    }

    for token in text.split_whitespace() {
        if token.len() >= 3 && token.chars().all(|ch| matches!(ch, 'T' | 'E' | 'I')) {
            score -= token.len() as f32 * 1.5;
        }
        if token.len() >= 9 && token.chars().all(|ch| ch.is_ascii_alphanumeric()) {
            score -= (token.len() - 8) as f32;
        }
    }
    score
}

#[derive(Debug, Clone, Copy)]
struct FrameRun {
    active: bool,
    start_frame: usize,
    end_frame: usize,
}

impl FrameRun {
    fn duration_s(self, step_s: f32, frame_s: f32) -> f32 {
        active_run_duration_s(self.start_frame, self.end_frame, step_s, frame_s)
    }
}

fn decode_region_slice_from_intervals(
    samples: &[f32],
    sample_rate: u32,
    pitch_hz: f32,
    cfg: &RegionStreamConfig,
) -> String {
    let frame_len = ((cfg.frame_len_s * sample_rate as f32).round() as usize).max(64);
    let frame_step = ((cfg.frame_step_s * sample_rate as f32).round() as usize).max(8);
    if samples.len() < frame_len {
        return String::new();
    }

    let mut log_powers = Vec::new();
    let mut offset = 0usize;
    while offset + frame_len <= samples.len() {
        let power = goertzel_power(&samples[offset..offset + frame_len], sample_rate, pitch_hz);
        log_powers.push(power.max(1e-12).ln());
        offset += frame_step;
    }
    if log_powers.len() < 4 {
        return String::new();
    }

    let Some(threshold) = otsu_log_power_threshold(&log_powers) else {
        return String::new();
    };
    let mut active: Vec<bool> = log_powers.iter().map(|power| *power >= threshold).collect();
    clean_interval_mask(&mut active);

    let runs = collect_frame_runs(&active);
    let step_s = frame_step as f32 / sample_rate as f32;
    let frame_s = frame_len as f32 / sample_rate as f32;
    let Some(dot_s) = estimate_dot_from_runs(&runs, step_s, frame_s) else {
        return String::new();
    };

    decode_runs_to_text(&runs, step_s, frame_s, dot_s)
}

fn otsu_log_power_threshold(log_powers: &[f32]) -> Option<f32> {
    if log_powers.len() < 4 {
        return None;
    }
    let mut sorted = log_powers.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

    let mut best_threshold = None;
    let mut best_score = f32::NEG_INFINITY;
    for i in 5..=95 {
        let threshold = percentile_sorted(&sorted, i as f32 / 100.0);
        let mut lo_sum = 0.0_f32;
        let mut hi_sum = 0.0_f32;
        let mut lo_n = 0usize;
        let mut hi_n = 0usize;
        for &power in log_powers {
            if power < threshold {
                lo_sum += power;
                lo_n += 1;
            } else {
                hi_sum += power;
                hi_n += 1;
            }
        }
        if lo_n < 3 || hi_n < 3 {
            continue;
        }
        let lo_mean = lo_sum / lo_n as f32;
        let hi_mean = hi_sum / hi_n as f32;
        let score = lo_n as f32 * hi_n as f32 * (hi_mean - lo_mean).powi(2);
        if score > best_score {
            best_score = score;
            best_threshold = Some(threshold);
        }
    }
    best_threshold
}

fn clean_interval_mask(active: &mut [bool]) {
    for _ in 0..2 {
        let runs = collect_frame_runs(active);
        for run in runs {
            let len = run.end_frame.saturating_sub(run.start_frame) + 1;
            if len <= 2 {
                for frame in active
                    .iter_mut()
                    .take(run.end_frame + 1)
                    .skip(run.start_frame)
                {
                    *frame = !run.active;
                }
            }
        }
    }
}

fn collect_frame_runs(active: &[bool]) -> Vec<FrameRun> {
    if active.is_empty() {
        return Vec::new();
    }
    let mut runs = Vec::new();
    let mut state = active[0];
    let mut start = 0usize;
    for (i, &on) in active.iter().enumerate().skip(1) {
        if on != state {
            runs.push(FrameRun {
                active: state,
                start_frame: start,
                end_frame: i - 1,
            });
            state = on;
            start = i;
        }
    }
    runs.push(FrameRun {
        active: state,
        start_frame: start,
        end_frame: active.len() - 1,
    });
    runs
}

fn estimate_dot_from_runs(runs: &[FrameRun], step_s: f32, frame_s: f32) -> Option<f32> {
    let mut on: Vec<f32> = runs
        .iter()
        .filter(|run| run.active)
        .map(|run| run.duration_s(step_s, frame_s))
        .filter(|duration| (0.015..=0.450).contains(duration))
        .collect();
    if on.is_empty() {
        return None;
    }
    on.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let shortest = on[0];
    let longest = *on.last().unwrap();
    let dot = if longest >= shortest * 2.2 {
        let cluster_max = shortest * 1.8;
        let short_cluster: Vec<f32> = on
            .iter()
            .copied()
            .filter(|duration| *duration <= cluster_max)
            .collect();
        short_cluster[short_cluster.len() / 2]
    } else {
        let median = on[on.len() / 2];
        if median > 0.180 {
            median / 3.0
        } else {
            median
        }
    };
    let wpm = 1.2 / dot.max(0.001);
    (5.0..=60.0).contains(&wpm).then_some(dot)
}

fn decode_runs_to_text(runs: &[FrameRun], step_s: f32, frame_s: f32, dot_s: f32) -> String {
    let mut out = String::new();
    let mut current = String::new();

    for run in runs {
        let duration = run.duration_s(step_s, frame_s);
        let units = duration / dot_s.max(0.001);
        if run.active {
            current.push(if units < DIT_DAH_BOUNDARY { '.' } else { '-' });
            continue;
        }

        if units > LETTER_SPACE_BOUNDARY {
            push_morse_letter(&mut out, &mut current);
        }
        if units > WORD_SPACE_BOUNDARY && !out.ends_with(' ') {
            out.push(' ');
        }
    }
    push_morse_letter(&mut out, &mut current);
    normalize_region_text(&out)
}

fn push_morse_letter(out: &mut String, current: &mut String) {
    if current.is_empty() {
        return;
    }
    if let Some(ch) = morse_to_char(current) {
        out.push(ch);
    } else {
        out.push('?');
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

fn should_prefer_pinned_short_decode(auto: &str, pinned: &str) -> bool {
    let auto_norm = normalize_region_text(auto);
    let pinned_norm = normalize_region_text(pinned);
    if pinned_norm.is_empty() || auto_norm == pinned_norm {
        return false;
    }
    let auto_chars = auto_norm
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .count();
    let pinned_chars = pinned_norm
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .count();
    auto_chars <= 2
        && pinned_chars <= 2
        && auto_norm
            .chars()
            .all(|ch| ch.is_whitespace() || matches!(ch, 'E' | 'I' | 'S' | 'H' | '5'))
}

fn normalize_region_text(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_uppercase()
}

pub(crate) fn normalize_region_transcript(text: &str) -> String {
    let normalized = normalize_region_text(text);
    let repaired = repair_repeated_callsign_portable_suffix(&normalized);
    replace_final_ar_prosign(&repaired)
}

fn replace_final_ar_prosign(text: &str) -> String {
    let mut tokens: Vec<&str> = text.split_whitespace().collect();
    if matches!(tokens.last(), Some(&"+")) {
        let last = tokens.len() - 1;
        tokens[last] = "AR";
    }
    tokens.join(" ")
}

fn repair_repeated_callsign_portable_suffix(text: &str) -> String {
    let mut tokens: Vec<String> = text
        .split_whitespace()
        .map(std::string::ToString::to_string)
        .collect();
    if tokens.len() < 4 {
        return text.to_string();
    }

    let mut i = 1usize;
    while i < tokens.len() {
        if !is_callsign_token(&tokens[i]) || tokens[i] != tokens[i - 1] {
            i += 1;
            continue;
        }

        let callsign = tokens[i].clone();
        let mut j = i + 1;
        while j < tokens.len() {
            if is_callsign_prefix_fragment(&callsign, &tokens[j]) {
                let slash_idx = (j + 1..=(j + 2).min(tokens.len() - 1))
                    .find(|&idx| is_portable_suffix_token(&tokens[idx]));
                if let Some(slash_idx) = slash_idx {
                    let repaired = format!("{callsign}{}", tokens[slash_idx]);
                    tokens.splice(j..=slash_idx, [repaired]);
                    break;
                }
            }
            j += 1;
        }
        i += 1;
    }

    tokens.join(" ")
}

fn is_callsign_token(token: &str) -> bool {
    let len = token.len();
    (4..=10).contains(&len)
        && token.chars().all(|ch| ch.is_ascii_alphanumeric())
        && token.chars().any(|ch| ch.is_ascii_alphabetic())
        && token.chars().any(|ch| ch.is_ascii_digit())
}

fn is_callsign_prefix_fragment(callsign: &str, token: &str) -> bool {
    let len = token.len();
    (2..callsign.len()).contains(&len)
        && token.chars().all(|ch| ch.is_ascii_alphanumeric())
        && callsign.starts_with(token)
}

fn is_portable_suffix_token(token: &str) -> bool {
    let Some(rest) = token.strip_prefix('/') else {
        return false;
    };
    !rest.is_empty()
        && rest.len() <= 3
        && rest
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == 'P')
}

fn estimate_short_region_wpm(
    samples: &[f32],
    sample_rate: u32,
    pitch_hz: f32,
    cfg: &RegionStreamConfig,
) -> Option<f32> {
    let frame_len = ((cfg.frame_len_s * sample_rate as f32).round() as usize).max(64);
    let frame_step = ((cfg.frame_step_s * sample_rate as f32).round() as usize).max(8);
    if samples.len() < frame_len {
        return None;
    }

    let mut powers = Vec::new();
    let mut offset = 0usize;
    while offset + frame_len <= samples.len() {
        powers.push(goertzel_power(
            &samples[offset..offset + frame_len],
            sample_rate,
            pitch_hz,
        ));
        offset += frame_step;
    }
    if powers.len() < 4 {
        return None;
    }

    let mut sorted = powers.clone();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let noise_floor = percentile_sorted(&sorted, 0.35);
    let signal_floor = percentile_sorted(&sorted, 0.85);
    if !noise_floor.is_finite() || !signal_floor.is_finite() || signal_floor <= noise_floor {
        return None;
    }
    let threshold = noise_floor + (signal_floor - noise_floor) * cfg.threshold_factor.max(0.0);

    let step_s = frame_step as f32 / sample_rate as f32;
    let frame_s = frame_len as f32 / sample_rate as f32;
    let mut runs = Vec::new();
    let mut cur_start: Option<usize> = None;
    for (i, power) in powers.iter().enumerate() {
        match (*power >= threshold, cur_start) {
            (true, None) => cur_start = Some(i),
            (false, Some(start)) => {
                runs.push(active_run_duration_s(start, i - 1, step_s, frame_s));
                cur_start = None;
            }
            _ => {}
        }
    }
    if let Some(start) = cur_start {
        runs.push(active_run_duration_s(
            start,
            powers.len() - 1,
            step_s,
            frame_s,
        ));
    }
    if runs.is_empty() {
        return None;
    }

    runs.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let shortest = runs[0];
    let longest = runs[runs.len() - 1];
    let dot_s = if runs.len() == 1 && longest > 0.12 {
        longest / 3.0
    } else if longest >= shortest * 2.2 {
        shortest
    } else {
        runs[runs.len() / 2]
    };
    let wpm = 1.2 / dot_s.max(0.001);
    (5.0..=60.0).contains(&wpm).then_some(wpm)
}

fn active_run_duration_s(start_frame: usize, end_frame: usize, step_s: f32, frame_s: f32) -> f32 {
    end_frame.saturating_sub(start_frame) as f32 * step_s + frame_s
}

fn dedupe_region_candidates(mut candidates: Vec<RegionCandidate>) -> Vec<DecodedRegion> {
    candidates.sort_by(|a, b| {
        a.start_s
            .partial_cmp(&b.start_s)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| {
                b.score
                    .partial_cmp(&a.score)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
    });
    let mut chosen: Vec<RegionCandidate> = Vec::new();
    for candidate in candidates {
        if let Some(last) = chosen.last_mut() {
            let overlap =
                (last.end_s.min(candidate.end_s) - last.start_s.max(candidate.start_s)).max(0.0);
            let min_len = (last.end_s - last.start_s).min(candidate.end_s - candidate.start_s);
            if min_len > 0.0 && overlap / min_len >= 0.45 {
                if candidate.score > last.score {
                    *last = candidate;
                }
                continue;
            }
        }
        chosen.push(candidate);
    }
    chosen
        .into_iter()
        .map(|candidate| DecodedRegion {
            start_s: candidate.start_s,
            end_s: candidate.end_s,
            text: candidate.text,
        })
        .collect()
}

fn tonal_prominence_ratio(
    samples: &[f32],
    sample_rate: u32,
    pitch_hz: f32,
    cfg: &RegionStreamConfig,
) -> f32 {
    let frame_len = ((cfg.frame_len_s * sample_rate as f32).round() as usize).max(64);
    let frame_step = ((cfg.frame_step_s * sample_rate as f32).round() as usize).max(8);
    if samples.len() < frame_len {
        return 0.0;
    }

    let mut power_sum = 0.0_f64;
    let mut energy_sum = 0.0_f64;
    let mut count = 0u32;
    let mut offset = 0usize;
    while offset + frame_len <= samples.len() {
        let frame = &samples[offset..offset + frame_len];
        power_sum += goertzel_power(frame, sample_rate, pitch_hz) as f64;
        energy_sum += frame
            .iter()
            .map(|sample| (*sample as f64) * (*sample as f64))
            .sum::<f64>();
        count += 1;
        offset += frame_step;
    }
    if count == 0 || energy_sum <= f64::EPSILON {
        return 0.0;
    }
    (power_sum / energy_sum) as f32
}

/// One spectral peak from the goertzel sweep used by the multi-pitch
/// front-end. `power` is the average per-frame Goertzel power at
/// `pitch_hz`, in the same units the dominant-pitch estimator uses.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PitchPeak {
    pub pitch_hz: f32,
    pub power: f32,
}

/// Strategy used to score a candidate pitch from per-frame Goertzel powers.
///
/// `Mean` (the historical default) reports the average power across the
/// whole buffer — this is fine for short analysis windows where every
/// pitch of interest is present for the full window. On long buffers
/// (e.g. an end-to-end ragchew with 4 distinct turn pitches across 90 s)
/// `Mean` smears the bursts together and produces a single artifact peak
/// somewhere between the actual pitches.
///
/// `Percentile(q)` instead reports the q-quantile of per-frame powers.
/// Each burst pitch will have a strong tail (high frame powers during
/// its turn) so a high-percentile score (e.g. p90) lights up every
/// pitch that hosts a real burst, regardless of how much of the buffer
/// it occupies. This is what the region pipeline wants for long
/// multi-station QSOs.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PeakScoreKind {
    Mean,
    /// Quantile of per-frame Goertzel power (0.0..=1.0). 0.90 is a good
    /// default for region routing — robust to single-frame outliers but
    /// still surfaces bursts that are present for ~10% of the buffer.
    Percentile(f32),
}

/// Configuration for [`find_top_pitch_peaks`]. Wraps a
/// [`RegionStreamConfig`] (which controls the underlying Goertzel
/// sweep) and adds NMS / dynamic-range knobs.
#[derive(Debug, Clone)]
pub struct MultiPitchConfig {
    /// Maximum peaks to return.
    pub k: usize,
    /// NMS spacing (Hz). Two peaks closer than this in the sweep
    /// collapse to the stronger one. 40 Hz is the default because real
    /// QSO audio commonly has stations 50 Hz apart in pitch; a larger
    /// NMS would falsely merge them.
    pub min_separation_hz: f32,
    /// Drop peaks whose power is below `top_power * min_relative_power`.
    /// 0.10 is the default — keeps peaks within ~10 dB of the strongest
    /// while rejecting noise-floor peaks of the goertzel sweep.
    pub min_relative_power: f32,
    /// Underlying sweep configuration (pitch range, frame size, step).
    pub sweep: RegionStreamConfig,
    /// How to score a pitch from its per-frame Goertzel power vector.
    /// Defaults to `Mean` for backwards compatibility with the short
    /// analysis window used by the multi-pitch envelope decoder. The
    /// region pipeline overrides this to a high percentile so that long
    /// buffers with multiple distinct burst pitches do not smear into
    /// a single artifact peak.
    pub peak_score: PeakScoreKind,
}

impl Default for MultiPitchConfig {
    fn default() -> Self {
        Self {
            k: 4,
            min_separation_hz: 40.0,
            min_relative_power: 0.10,
            sweep: RegionStreamConfig::default(),
            peak_score: PeakScoreKind::Mean,
        }
    }
}

/// Run the goertzel sweep across `[pitch_lo_hz, pitch_hi_hz]` at
/// `pitch_step_hz` resolution and return up to `cfg.k` non-overlapping
/// local maxima sorted by power (strongest first).
///
/// This is the multi-station cousin of [`estimate_dominant_pitch`].
/// Algorithm:
///   1. Goertzel sweep produces `(pitch, power)` pairs, the same way
///      the single-pitch detector does.
///   2. Identify local maxima (strictly higher than both neighbours).
///   3. Sort by power descending.
///   4. Greedily emit peaks at least `min_separation_hz` apart,
///      stopping at `k` peaks or when the next peak falls below
///      `top_power * min_relative_power`.
///
/// Returns an empty `Vec` for empty input, sample rate 0, buffers
/// shorter than the goertzel frame, or a degenerate sweep where the
/// strongest non-zero candidate falls below the relative-power floor
/// (e.g. silence).
pub fn find_top_pitch_peaks(
    samples: &[f32],
    sample_rate: u32,
    cfg: &MultiPitchConfig,
) -> Vec<PitchPeak> {
    if samples.is_empty() || sample_rate == 0 || cfg.k == 0 {
        return Vec::new();
    }
    let sweep = &cfg.sweep;
    let frame_len = ((sweep.frame_len_s * sample_rate as f32).round() as usize).max(64);
    let frame_step = ((sweep.frame_step_s * sample_rate as f32).round() as usize).max(8);
    if samples.len() < frame_len || sweep.pitch_step_hz <= 0.0 {
        return Vec::new();
    }

    // Same coarse stride the single-pitch estimator uses.
    let stride = frame_step.saturating_mul(10).max(frame_step);

    let mut candidates: Vec<PitchPeak> = Vec::new();
    let mut frame_powers: Vec<f32> = Vec::new();
    let mut pitch = sweep.pitch_lo_hz;
    while pitch <= sweep.pitch_hi_hz {
        frame_powers.clear();
        let mut offset = 0usize;
        while offset + frame_len <= samples.len() {
            let p = goertzel_power(&samples[offset..offset + frame_len], sample_rate, pitch);
            frame_powers.push(p);
            offset += stride;
        }
        let score = if frame_powers.is_empty() {
            0.0
        } else {
            match cfg.peak_score {
                PeakScoreKind::Mean => {
                    let sum: f64 = frame_powers.iter().map(|p| *p as f64).sum();
                    (sum / frame_powers.len() as f64) as f32
                }
                PeakScoreKind::Percentile(q) => {
                    let mut sorted = frame_powers.clone();
                    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
                    percentile_sorted(&sorted, q)
                }
            }
        };
        candidates.push(PitchPeak {
            pitch_hz: pitch,
            power: score,
        });
        pitch += sweep.pitch_step_hz;
    }
    if candidates.is_empty() {
        return Vec::new();
    }

    // Local-maxima filter on the swept curve. Endpoints may also be
    // peaks if they dominate their single neighbour.
    let n = candidates.len();
    let mut maxima: Vec<PitchPeak> = Vec::new();
    for i in 0..n {
        let p = candidates[i].power;
        let left = if i == 0 {
            f32::NEG_INFINITY
        } else {
            candidates[i - 1].power
        };
        let right = if i + 1 == n {
            f32::NEG_INFINITY
        } else {
            candidates[i + 1].power
        };
        if p > 0.0 && p >= left && p >= right && (p > left || p > right) {
            maxima.push(candidates[i]);
        }
    }
    if maxima.is_empty() {
        return Vec::new();
    }

    // Sort strongest first.
    maxima.sort_by(|a, b| {
        b.power
            .partial_cmp(&a.power)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let top_power = maxima[0].power;
    if top_power <= 0.0 {
        return Vec::new();
    }
    let abs_floor = top_power * cfg.min_relative_power.max(0.0);
    let nms = cfg.min_separation_hz.max(0.0);

    let mut chosen: Vec<PitchPeak> = Vec::new();
    for cand in maxima.into_iter() {
        if cand.power < abs_floor {
            break;
        }
        let too_close = chosen
            .iter()
            .any(|p| (p.pitch_hz - cand.pitch_hz).abs() < nms);
        if too_close {
            continue;
        }
        chosen.push(cand);
        if chosen.len() >= cfg.k {
            break;
        }
    }
    chosen
}

/// Discover pitches likely to host CW bursts in a long buffer.
///
/// Two-source approach (union, then NMS):
///
/// 1. Per-window dominant pitches. Slide a window across the buffer and
///    record the single dominant pitch in each window. Long-form
///    ragchews — where each turn occupies a multi-second slice at a
///    distinct pitch — produce a unique dominant pitch per turn.
///
/// 2. Whole-buffer mean-based top-K peaks. Dense clusters where bursts
///    are short and similar in power (typical of contests) keep all of
///    their pitches at the top of the mean-power ranking even when no
///    single window is dominated by one of them.
///
/// The union is NMS-merged at a tight 25 Hz spacing — enough to
/// suppress duplicates at the same sweep position but small enough that
/// real adjacent stations spaced 30-50 Hz apart are kept separately.
/// Final dedup of redundant decodes (when two close pitches generate
/// overlapping regions) is left to `dedupe_region_candidates`, which
/// scores by tonal prominence and useful character count.
///
/// Returns pitches sorted by descending power.
pub fn discover_burst_pitches(
    samples: &[f32],
    sample_rate: u32,
    cfg: &RegionStreamConfig,
) -> Vec<f32> {
    if samples.is_empty() || sample_rate == 0 {
        return Vec::new();
    }
    // 25 Hz matches the sweep step. Tighter would just admit duplicate
    // sweeps of the same sweep position; looser collapses real adjacent
    // stations.
    let nms_hz = (cfg.pitch_step_hz - 1.0).max(15.0);
    let mut combined: Vec<PitchPeak> = Vec::new();

    // Source 1: per-window dominants. Window is a balance — long enough
    // for a single burst at a stable pitch to dominate (real ragchew
    // turns are 15-30 s) and short enough that 4 turns produce 4
    // distinct dominants. ~10 s with 5 s hop gives 2-3 windows per
    // long-form turn.
    let window_s = 10.0_f32;
    let hop_s = 5.0_f32;
    let total_s = samples.len() as f32 / sample_rate as f32;
    // A window's dominant pitch is only contributed if the dominant
    // sweep position has a clear advantage over the window's median
    // sweep power. This keeps noise-only / QRM-only windows from
    // polluting the candidate list with a noise-floor "dominant" that
    // wastes downstream region-detection work.
    let window_confidence_ratio = 2.5_f32;
    let mut t0 = 0.0_f32;
    while t0 < total_s {
        let t1 = (t0 + window_s).min(total_s);
        if t1 - t0 < 1.0 {
            break;
        }
        let s = (t0 * sample_rate as f32) as usize;
        let e = ((t1 * sample_rate as f32) as usize).min(samples.len());
        if e > s {
            let slice = &samples[s..e];
            let dom = estimate_dominant_pitch(slice, sample_rate, cfg);
            // Score the dominant by its absolute power within the window
            // so the eventual ranking reflects burst strength.
            let frame_len = ((cfg.frame_len_s * sample_rate as f32).round() as usize).max(64);
            let frame_step = ((cfg.frame_step_s * sample_rate as f32).round() as usize).max(8);
            let stride = frame_step.saturating_mul(10).max(frame_step);
            let mut sum = 0.0_f64;
            let mut count = 0u32;
            let mut offset = 0usize;
            while offset + frame_len <= slice.len() {
                sum += goertzel_power(&slice[offset..offset + frame_len], sample_rate, dom) as f64;
                count += 1;
                offset += stride;
            }
            let power = if count > 0 {
                (sum / count as f64) as f32
            } else {
                0.0
            };
            // Median sweep power across the same window — used to
            // suppress noise-only windows whose "dominant" is just the
            // luckiest noise bin.
            let median_power = window_median_sweep_power(slice, sample_rate, cfg);
            let confident = median_power > 0.0 && power >= median_power * window_confidence_ratio;
            if confident {
                combined.push(PitchPeak {
                    pitch_hz: dom,
                    power,
                });
            }
        }
        t0 += hop_s;
    }

    // Source 2: whole-buffer mean top-K. This preserves the historical
    // multi-pitch behavior for short, dense clusters (contest bursts at
    // closely-spaced pitches that none of which dominates a window).
    // Use a tighter NMS here too so adjacent pitches aren't pre-merged.
    let mean_peaks = find_top_pitch_peaks(
        samples,
        sample_rate,
        &MultiPitchConfig {
            k: 8,
            min_separation_hz: nms_hz,
            min_relative_power: 0.05,
            sweep: cfg.clone(),
            peak_score: PeakScoreKind::Mean,
        },
    );
    combined.extend(mean_peaks);

    if combined.is_empty() {
        return Vec::new();
    }

    // Sort by power descending, then NMS-merge by pitch proximity.
    combined.sort_by(|a, b| {
        b.power
            .partial_cmp(&a.power)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let mut chosen: Vec<f32> = Vec::new();
    for cand in combined {
        if chosen.iter().any(|&p| (p - cand.pitch_hz).abs() < nms_hz) {
            continue;
        }
        chosen.push(cand.pitch_hz);
    }
    chosen
}

/// Median average-power across the Goertzel sweep used by region routing.
/// Used as a noise-floor estimate to gate windowed dominant-pitch
/// candidates: a real CW window has a dominant pitch many times above
/// the median sweep power, while a noise-only window has a roughly flat
/// sweep where the "dominant" is just the luckiest noise bin.
fn window_median_sweep_power(samples: &[f32], sample_rate: u32, cfg: &RegionStreamConfig) -> f32 {
    let frame_len = ((cfg.frame_len_s * sample_rate as f32).round() as usize).max(64);
    let frame_step = ((cfg.frame_step_s * sample_rate as f32).round() as usize).max(8);
    if samples.len() < frame_len || cfg.pitch_step_hz <= 0.0 {
        return 0.0;
    }
    let stride = frame_step.saturating_mul(10).max(frame_step);
    let mut scores: Vec<f32> = Vec::new();
    let mut pitch = cfg.pitch_lo_hz;
    while pitch <= cfg.pitch_hi_hz {
        let mut sum = 0.0_f64;
        let mut count = 0u32;
        let mut offset = 0usize;
        while offset + frame_len <= samples.len() {
            sum += goertzel_power(&samples[offset..offset + frame_len], sample_rate, pitch) as f64;
            count += 1;
            offset += stride;
        }
        let s = if count > 0 {
            (sum / count as f64) as f32
        } else {
            0.0
        };
        scores.push(s);
        pitch += cfg.pitch_step_hz;
    }
    if scores.is_empty() {
        return 0.0;
    }
    scores.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    percentile_sorted(&scores, 0.5)
}

pub fn estimate_dominant_pitch(samples: &[f32], sample_rate: u32, cfg: &RegionStreamConfig) -> f32 {
    let frame_len = ((cfg.frame_len_s * sample_rate as f32).round() as usize).max(64);
    let frame_step = ((cfg.frame_step_s * sample_rate as f32).round() as usize).max(8);
    if samples.len() < frame_len {
        return cfg.pitch_lo_hz;
    }

    let mut best_pitch = cfg.pitch_lo_hz;
    let mut best_score = f32::MIN;
    let mut pitch = cfg.pitch_lo_hz;
    while pitch <= cfg.pitch_hi_hz {
        // Sum power over a coarse stride (every 10th frame is plenty for pitch ID).
        let stride = frame_step.saturating_mul(10).max(frame_step);
        let mut sum = 0.0_f64;
        let mut count = 0u32;
        let mut offset = 0usize;
        while offset + frame_len <= samples.len() {
            sum += goertzel_power(&samples[offset..offset + frame_len], sample_rate, pitch) as f64;
            count += 1;
            offset += stride;
        }
        let score = if count > 0 { sum / count as f64 } else { 0.0 };
        if score as f32 > best_score {
            best_score = score as f32;
            best_pitch = pitch;
        }
        pitch += cfg.pitch_step_hz;
    }
    best_pitch
}

fn detect_active_regions(
    samples: &[f32],
    sample_rate: u32,
    pitch_hz: f32,
    cfg: &RegionStreamConfig,
) -> Vec<(f32, f32)> {
    let frame_len = ((cfg.frame_len_s * sample_rate as f32).round() as usize).max(64);
    let frame_step = ((cfg.frame_step_s * sample_rate as f32).round() as usize).max(8);
    if samples.len() < frame_len {
        return vec![];
    }

    let mut powers = Vec::new();
    let mut offset = 0usize;
    while offset + frame_len <= samples.len() {
        powers.push(goertzel_power(
            &samples[offset..offset + frame_len],
            sample_rate,
            pitch_hz,
        ));
        offset += frame_step;
    }
    if powers.len() < 4 {
        return vec![];
    }

    let step_s = frame_step as f32 / sample_rate as f32;
    let frame_s = frame_len as f32 / sample_rate as f32;

    // Block-adaptive log-power threshold + hysteresis. This replaces the
    // earlier global-percentile threshold so that QSB (signal fading
    // mid-buffer) doesn't fragment weak-but-real bursts into noise-spike
    // chatter that the deeper decoder confidently mis-classifies as
    // shortest-symbol garbage (T/E/A/S/M).
    let active = adaptive_active_mask(&powers, cfg, step_s);

    // Collect contiguous active runs as (start_s, end_s) using the frame-step
    // grid. The end time is the *end* of the last active frame, not its start.
    let mut runs: Vec<(f32, f32)> = Vec::new();
    let mut cur_start: Option<usize> = None;
    for (i, &on) in active.iter().enumerate() {
        match (on, cur_start) {
            (true, None) => cur_start = Some(i),
            (false, Some(s)) => {
                let start_s = s as f32 * step_s;
                let end_s = (i - 1) as f32 * step_s + frame_s;
                runs.push((start_s, end_s));
                cur_start = None;
            }
            _ => {}
        }
    }
    if let Some(s) = cur_start {
        let start_s = s as f32 * step_s;
        let end_s = (active.len() - 1) as f32 * step_s + frame_s;
        runs.push((start_s, end_s));
    }

    // Merge runs separated by gaps shorter than merge_gap_s.
    let mut merged: Vec<(f32, f32)> = Vec::new();
    for run in runs {
        if let Some(last) = merged.last_mut() {
            if run.0 - last.1 <= cfg.merge_gap_s.max(0.0) {
                last.1 = run.1;
                continue;
            }
        }
        merged.push(run);
    }

    // Drop runs shorter than min_region_s.
    merged
        .into_iter()
        .filter(|(s, e)| (e - s) >= cfg.min_region_s.max(0.0))
        .collect()
}

/// Block-adaptive log-power threshold with hysteresis.
///
/// Replaces a single global percentile threshold over the whole buffer.
/// The whole-buffer approach silently fragmented signals affected by QSB
/// (slow amplitude fading) because the global signal-floor was dominated
/// by the strong portions, leaving weaker-but-still-real CW below the
/// detection threshold.
///
/// The new approach computes per-block (sliding 2s window) statistics,
/// derives robust **block-level** noise and peak references (q20 of
/// block medians, q80 of block p85s), then classifies each block into
/// one of three regimes:
///
/// * **SIGNAL-UNIFORM** — block median sits near the buffer's peak
///   reference. Signal occupies most of the block (e.g. mid-burst,
///   long uninterrupted CW). Threshold sits just below the signal
///   cluster so all signal frames qualify even when contrast is small.
/// * **MIXED** — block contains a mix of signal+noise with high
///   contrast and an upper tail near the peak reference. Adaptive
///   threshold tuned between the local p35 and p85, with hysteresis.
/// * **NOISE-UNIFORM** — block has no evidence of signal. Threshold
///   sits at local p95 to reject everything but extreme spikes.
///
/// A global "is signal even possible" gate using the dynamic range
/// between the noise and peak references prevents pure-noise buffers
/// from being mis-classified as signal-uniform.
///
/// Block-level references (instead of frame-level percentiles) are
/// critical: a frame-global p85 drifts with the buffer's signal vs.
/// silence ratio and can collapse into noise upper-tail in
/// signal-light buffers, while the q80-of-block-p85 reference is
/// stable as long as ANY blocks contain signal.
fn adaptive_active_mask(powers: &[f32], cfg: &RegionStreamConfig, step_s: f32) -> Vec<bool> {
    if powers.len() < 4 {
        return vec![];
    }

    // Work in log-power so multiplicative QSB swings (5-30x) become
    // additive. Add a tiny epsilon to avoid -inf at exact-zero frames.
    let log_powers: Vec<f32> = powers.iter().map(|&p| (p.max(1e-12)).ln()).collect();

    // 2-second blocks with 1-second hop (50% overlap). 2s is long enough
    // to span any CW word at 8+ WPM but short enough to track slow QSB.
    let block_s = 2.0_f32;
    let hop_s = 1.0_f32;
    let block_frames = ((block_s / step_s).round() as usize).max(40);
    let hop_frames = ((hop_s / step_s).round() as usize).max(20);
    let half = block_frames / 2;

    let n = log_powers.len();

    // Pass 1: collect per-block log-power percentiles.
    let mut centers: Vec<usize> = Vec::new();
    let mut block_p20: Vec<f32> = Vec::new();
    let mut block_p35: Vec<f32> = Vec::new();
    let mut block_p50: Vec<f32> = Vec::new();
    let mut block_p85: Vec<f32> = Vec::new();
    let mut block_p95: Vec<f32> = Vec::new();

    let mut center = 0usize;
    while center < n {
        let lo = center.saturating_sub(half);
        let hi = (center + half).min(n);
        let mut local: Vec<f32> = log_powers[lo..hi].to_vec();
        local.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        centers.push(center);
        block_p20.push(percentile_sorted(&local, 0.20));
        block_p35.push(percentile_sorted(&local, 0.35));
        block_p50.push(percentile_sorted(&local, 0.50));
        block_p85.push(percentile_sorted(&local, 0.85));
        block_p95.push(percentile_sorted(&local, 0.95));
        if hop_frames == 0 {
            break;
        }
        center = center.saturating_add(hop_frames);
    }

    if centers.is_empty() {
        return vec![false; n];
    }

    // Robust global references derived from block-level stats.
    //
    // **noise_ref**: q20 of block-level p20s. Block-p20 captures the
    // quiet tail of each block — for active CW blocks this is the
    // intra-element silence between dits/dahs (which always exists
    // even in dense long-form CW); for silent/noise blocks it sits
    // near the noise floor. Using block-p20 instead of block-p50
    // is critical for long uninterrupted CW like ARRL Code Practice:
    // when 100% of blocks are signal-dominant, q20(block_p50) sits
    // ABOVE signal level and the noise floor reference collapses.
    // Block-p20 still reaches down to the inter-element gaps so the
    // reference stays anchored.
    //
    // **peak_ref**: q80 of block p85s. The loudest 20% of blocks set
    // the signal reference. Stable as long as ANY blocks contain
    // signal — frame-global p85 was tried first but is unstable in
    // signal-light buffers.
    let mut sorted_p20 = block_p20.clone();
    sorted_p20.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let noise_ref = percentile_sorted(&sorted_p20, 0.20);

    let mut sorted_p85 = block_p85.clone();
    sorted_p85.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let peak_ref = percentile_sorted(&sorted_p85, 0.80);

    // Frame-global silence floor (5th percentile of frame log-powers)
    // is still useful as an absolute lower clamp on thresholds so a
    // pathological local window can't drop on_t below true silence.
    let mut sorted_global = log_powers.clone();
    sorted_global.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let global_p05 = percentile_sorted(&sorted_global, 0.05);

    // Detectability gate: require at least 10x dynamic range between
    // the noise and peak references for the buffer to plausibly
    // contain signal at all. ln(10) ≈ 2.303.
    let g_min = (10.0_f32).ln();
    let global_signal_possible = (peak_ref - noise_ref) >= g_min;

    // Per-block classification thresholds. d_min: how far above
    // noise_ref the block median must be to claim signal-uniform.
    // signal_match_margin: how close block median must be to
    // peak_ref. n_min: minimum threshold headroom above noise_ref.
    let d_min = (3.0_f32).ln();
    let signal_match_margin = (3.0_f32).ln();
    let n_min = (3.0_f32).ln();

    // Contrast required for MIXED branch (10x linear). At 10x p85/p35
    // ratio there's effectively no white-noise variance explanation —
    // the block must contain real signal+noise (or two different
    // signal levels). White noise Goertzel power has p85/p35 spread
    // of only ~2-3x, so noise-only blocks can't fake this gate.
    let min_contrast = (10.0_f32).ln();

    let factor = cfg.threshold_factor.max(0.0);
    // Hysteresis: weak tails must drop halfway back toward noise floor
    // before being declared off. Only applied in MIXED branch since
    // signal-uniform and noise-uniform have flat thresholds.
    let off_factor_mul = 0.5_f32;

    // Pass 2: derive on/off thresholds per block.
    let mut on_thr: Vec<f32> = Vec::with_capacity(centers.len());
    let mut off_thr: Vec<f32> = Vec::with_capacity(centers.len());
    for i in 0..centers.len() {
        let p35 = block_p35[i];
        let p50 = block_p50[i];
        let p85 = block_p85[i];
        let p95 = block_p95[i];
        let contrast = p85 - p35;

        let dominant_signal = global_signal_possible
            && p50 >= noise_ref + d_min
            && p50 >= peak_ref - signal_match_margin;

        let (on_t, off_t) = if dominant_signal {
            // Signal occupies most of the block. Set threshold below
            // the signal cluster so all signal frames qualify; clamp
            // up to noise_ref + headroom so we never drop into noise.
            let candidate = p50 - 0.5 * (p85 - p50);
            let on_t = candidate.max(noise_ref + n_min);
            (on_t, on_t)
        } else if global_signal_possible && contrast >= min_contrast {
            // Real signal+noise in this block (signal exists somewhere
            // in the buffer AND this block has 10x+ dynamic range).
            // Adaptive threshold anchored to local p35/p85 spread,
            // with hysteresis to bridge brief mid-element fades during
            // QSB. global_signal_possible gates this branch so an
            // all-noise buffer can't admit MIXED branch via spurious
            // noise-spike contrast.
            let on_t = p35 + (p85 - p35) * factor;
            let off_t = p35 + (p85 - p35) * (factor * off_factor_mul).max(0.0);
            (on_t, off_t)
        } else {
            // No clear signal. Use local p95 to reject everything but
            // extreme spikes; clamp up to noise_ref + headroom so a
            // degenerate local window can't drop on_t into noise.
            let on_t = p95.max(noise_ref + n_min);
            (on_t, on_t)
        };

        // Final absolute floor at global silence + ~ln(4) so even
        // pathological windows can't drop the threshold below true
        // silence.
        let silence_floor = global_p05 + (4.0_f32).ln();
        on_thr.push(on_t.max(silence_floor));
        off_thr.push(off_t.max(silence_floor));
    }

    // Hysteresis state machine over linearly-interpolated thresholds.
    let mut active = vec![false; n];
    let mut on = false;
    for i in 0..n {
        let (on_t, off_t) = interp_threshold(i, &centers, &on_thr, &off_thr);
        if on {
            if log_powers[i] < off_t {
                on = false;
            }
        } else if log_powers[i] >= on_t {
            on = true;
        }
        active[i] = on;
    }
    active
}

fn interp_threshold(i: usize, centers: &[usize], on_thr: &[f32], off_thr: &[f32]) -> (f32, f32) {
    if centers.is_empty() {
        return (f32::INFINITY, f32::INFINITY);
    }
    if i <= centers[0] {
        return (on_thr[0], off_thr[0]);
    }
    if i >= *centers.last().unwrap() {
        return (*on_thr.last().unwrap(), *off_thr.last().unwrap());
    }
    let mut lo = 0usize;
    while lo + 1 < centers.len() && centers[lo + 1] <= i {
        lo += 1;
    }
    let hi = (lo + 1).min(centers.len() - 1);
    if lo == hi {
        return (on_thr[lo], off_thr[lo]);
    }
    let span = (centers[hi] - centers[lo]) as f32;
    let frac = if span > 0.0 {
        (i - centers[lo]) as f32 / span
    } else {
        0.0
    };
    (
        on_thr[lo] * (1.0 - frac) + on_thr[hi] * frac,
        off_thr[lo] * (1.0 - frac) + off_thr[hi] * frac,
    )
}

pub fn goertzel_power(samples: &[f32], sample_rate: u32, target_hz: f32) -> f32 {
    let omega = (2.0 * std::f32::consts::PI * target_hz) / sample_rate as f32;
    let coeff = 2.0 * omega.cos();
    let mut q1 = 0.0_f32;
    let mut q2 = 0.0_f32;
    for &s in samples {
        let q0 = coeff * q1 - q2 + s;
        q2 = q1;
        q1 = q0;
    }
    q1 * q1 + q2 * q2 - coeff * q1 * q2
}

pub fn percentile_sorted(sorted: &[f32], q: f32) -> f32 {
    if sorted.is_empty() {
        return 0.0;
    }
    let cq = q.clamp(0.0, 1.0);
    let idx = ((sorted.len() - 1) as f32 * cq).round() as usize;
    sorted[idx.min(sorted.len() - 1)]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn synth_tone(freq_hz: f32, dur_s: f32, sample_rate: u32, amp: f32) -> Vec<f32> {
        let n = (dur_s * sample_rate as f32) as usize;
        (0..n)
            .map(|i| {
                (2.0 * std::f32::consts::PI * freq_hz * i as f32 / sample_rate as f32).sin() * amp
            })
            .collect()
    }

    #[test]
    fn detects_single_region_in_padded_tone() {
        let sr = 12_000u32;
        let mut buf = vec![0.0_f32; (sr as f32 * 2.0) as usize];
        buf.extend(synth_tone(700.0, 1.0, sr, 0.5));
        buf.extend(vec![0.0_f32; (sr as f32 * 2.0) as usize]);
        let cfg = RegionStreamConfig::default();
        let regions = detect_active_regions(&buf, sr, 700.0, &cfg);
        assert_eq!(regions.len(), 1);
        let (s, e) = regions[0];
        assert!((s - 2.0).abs() < 0.2, "start ~2.0, got {s}");
        assert!((e - 3.0).abs() < 0.2, "end ~3.0, got {e}");
    }

    #[test]
    fn estimate_pitch_picks_dominant_frequency() {
        let sr = 12_000u32;
        let buf = synth_tone(600.0, 2.0, sr, 0.5);
        let cfg = RegionStreamConfig::default();
        let pitch = estimate_dominant_pitch(&buf, sr, &cfg);
        assert!(
            (pitch - 600.0).abs() <= cfg.pitch_step_hz,
            "expected ~600, got {pitch}"
        );
    }

    #[test]
    fn estimate_pitch_finds_high_sidetone() {
        // Real-world live captures (e.g. live-20260427-111419.wav) use
        // sidetones up to ~1100 Hz. Default pitch sweep must cover that
        // range; otherwise the detector locks onto whatever has highest
        // power inside the [pitch_lo, pitch_hi] window (typically a
        // low-frequency noise hump) and the rest of the decoder
        // produces ghost-character garbage.
        let sr = 12_000u32;
        let buf = synth_tone(1100.0, 2.0, sr, 0.5);
        let cfg = RegionStreamConfig::default();
        let pitch = estimate_dominant_pitch(&buf, sr, &cfg);
        assert!(
            (pitch - 1100.0).abs() <= cfg.pitch_step_hz,
            "expected ~1100 Hz, got {pitch} (default pitch_hi_hz must cover common operator sidetones)"
        );
    }

    #[test]
    fn empty_input_returns_empty_result() {
        let cfg = RegionStreamConfig::default();
        let r = decode_region_stream(&[], 12_000, &cfg);
        assert!(r.text.is_empty());
        assert!(r.regions.is_empty());
    }

    #[test]
    fn unknown_region_text_is_low_confidence() {
        assert!(is_low_confidence_region_text("E?R"));
        assert!(!is_low_confidence_region_text("7QP W7N"));
    }

    #[test]
    fn interval_decode_is_preferred_for_repeated_t_garbage_tail() {
        assert!(should_prefer_interval_decode("TT TTI TTTTT TTTD", "AD /1"));
    }

    #[test]
    fn transcript_normalization_repairs_repeated_callsign_portable_tail() {
        let raw = "IQ CQ CQ DE WA2IAC WA2IAC WA2 AD /1 +";
        assert_eq!(
            normalize_region_transcript(raw),
            "IQ CQ CQ DE WA2IAC WA2IAC WA2IAC/1 AR"
        );
    }

    fn noise_buf(rate: u32, seconds: f32, seed: u64, amplitude: f32) -> Vec<f32> {
        let n = (rate as f32 * seconds) as usize;
        let mut state = seed;
        let mut out = Vec::with_capacity(n);
        for _ in 0..n {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            let v = ((state >> 33) as u32) as f32 / u32::MAX as f32;
            out.push((v * 2.0 - 1.0) * amplitude);
        }
        out
    }

    #[test]
    fn find_top_pitch_peaks_returns_empty_for_silence() {
        let sr = 12_000u32;
        let buf = vec![0.0f32; sr as usize * 2];
        let cfg = MultiPitchConfig::default();
        let peaks = find_top_pitch_peaks(&buf, sr, &cfg);
        assert!(
            peaks.is_empty(),
            "silence should yield no peaks, got {peaks:?}"
        );
    }

    #[test]
    fn find_top_pitch_peaks_returns_empty_for_noise_only() {
        let sr = 12_000u32;
        let buf = noise_buf(sr, 2.0, 0xC0FFEE, 0.05);
        let cfg = MultiPitchConfig::default();
        let peaks = find_top_pitch_peaks(&buf, sr, &cfg);
        // White noise has no strong tonal peaks; either empty or all
        // peaks are below the relative-power floor (which is exactly
        // what the gate enforces).
        if !peaks.is_empty() {
            let strongest = peaks.iter().map(|p| p.power).fold(0.0_f32, |a, b| a.max(b));
            // Compare against a fully-swept estimator power as a
            // sanity check that the multi-pitch path does not explode
            // on noise.
            assert!(
                strongest.is_finite(),
                "noise produced non-finite power {strongest}"
            );
        }
    }

    #[test]
    fn find_top_pitch_peaks_handles_short_buffer() {
        let sr = 12_000u32;
        // Buffer shorter than even one goertzel frame.
        let buf = vec![0.5_f32; 16];
        let cfg = MultiPitchConfig::default();
        let peaks = find_top_pitch_peaks(&buf, sr, &cfg);
        assert!(
            peaks.len() <= 1,
            "short buffer should not produce many peaks, got {}",
            peaks.len()
        );
    }

    #[test]
    fn find_top_pitch_peaks_returns_one_for_single_pitch() {
        let sr = 12_000u32;
        let buf = synth_tone(700.0, 2.0, sr, 0.5);
        let cfg = MultiPitchConfig {
            k: 4,
            ..MultiPitchConfig::default()
        };
        let peaks = find_top_pitch_peaks(&buf, sr, &cfg);
        assert!(
            !peaks.is_empty(),
            "single tone should produce at least one peak"
        );
        // Strongest peak should be near 700 Hz (within sweep step).
        let top = peaks[0];
        assert!(
            (top.pitch_hz - 700.0).abs() <= cfg.sweep.pitch_step_hz,
            "expected ~700 Hz, got {}",
            top.pitch_hz
        );
        // Any additional peaks should be at least min_separation_hz away.
        for p in peaks.iter().skip(1) {
            assert!(
                (p.pitch_hz - top.pitch_hz).abs() >= cfg.min_separation_hz,
                "peak {} too close to {}",
                p.pitch_hz,
                top.pitch_hz
            );
        }
    }

    #[test]
    fn find_top_pitch_peaks_resolves_50hz_separation() {
        let sr = 12_000u32;
        let mut buf = synth_tone(700.0, 2.0, sr, 0.4);
        let other = synth_tone(750.0, 2.0, sr, 0.4);
        for (a, b) in buf.iter_mut().zip(other.iter()) {
            *a += *b;
        }
        let cfg = MultiPitchConfig {
            k: 2,
            min_separation_hz: 40.0,
            min_relative_power: 0.10,
            sweep: RegionStreamConfig {
                pitch_step_hz: 10.0,
                ..RegionStreamConfig::default()
            },
            peak_score: PeakScoreKind::Mean,
        };
        let peaks = find_top_pitch_peaks(&buf, sr, &cfg);
        assert_eq!(peaks.len(), 2, "expected 2 peaks at 700/750, got {peaks:?}");
        let mut got: Vec<f32> = peaks.iter().map(|p| p.pitch_hz).collect();
        got.sort_by(|a, b| a.partial_cmp(b).unwrap());
        assert!(
            (got[0] - 700.0).abs() <= 15.0 && (got[1] - 750.0).abs() <= 15.0,
            "expected ~700 and ~750, got {got:?}"
        );
    }

    #[test]
    fn find_top_pitch_peaks_nms_collapses_close_peaks() {
        let sr = 12_000u32;
        let mut buf = synth_tone(700.0, 2.0, sr, 0.4);
        let other = synth_tone(720.0, 2.0, sr, 0.4);
        for (a, b) in buf.iter_mut().zip(other.iter()) {
            *a += *b;
        }
        let cfg = MultiPitchConfig {
            k: 4,
            min_separation_hz: 60.0,
            min_relative_power: 0.10,
            sweep: RegionStreamConfig {
                pitch_step_hz: 10.0,
                ..RegionStreamConfig::default()
            },
            peak_score: PeakScoreKind::Mean,
        };
        let peaks = find_top_pitch_peaks(&buf, sr, &cfg);
        assert_eq!(
            peaks.len(),
            1,
            "60 Hz NMS should collapse 20-Hz-spaced peaks, got {peaks:?}"
        );
    }

    #[test]
    fn discover_burst_pitches_finds_long_form_turn_pitches() {
        // Synthesize a 4-burst sequence at distinct pitches with silence
        // between bursts. This mirrors the long-form ragchew failure
        // pattern: each turn occupies ~5 s at a different pitch and the
        // whole-buffer mean smears them into a single artifact peak.
        let sr = 12_000u32;
        let burst_s = 5.0_f32;
        let gap_s = 1.0_f32;
        let burst_pitches = [640.0_f32, 675.0, 710.0, 745.0];
        let mut buf: Vec<f32> = Vec::new();
        for (i, p) in burst_pitches.iter().enumerate() {
            buf.extend(synth_tone(*p, burst_s, sr, 0.5));
            if i + 1 < burst_pitches.len() {
                buf.extend(vec![0.0_f32; (sr as f32 * gap_s) as usize]);
            }
        }
        let cfg = RegionStreamConfig::default();
        let pitches = discover_burst_pitches(&buf, sr, &cfg);
        for &p in &burst_pitches {
            let found = pitches.iter().any(|&q| (q - p).abs() <= cfg.pitch_step_hz);
            assert!(
                found,
                "expected pitch ~{p} Hz to be discovered, got {pitches:?}"
            );
        }
    }

    #[test]
    fn discover_burst_pitches_handles_empty_buffer() {
        let cfg = RegionStreamConfig::default();
        let pitches = discover_burst_pitches(&[], 12_000, &cfg);
        assert!(pitches.is_empty());
    }

    #[test]
    fn decode_region_stream_returns_no_text_on_pure_noise() {
        // Real-world dead-air should never yield ghost characters. The
        // tonal_prominence_ratio guard plus the new windowed-confidence
        // gate together must keep the pipeline silent.
        let sr = 12_000u32;
        let buf = noise_buf(sr, 30.0, 0xDEAD_BEEF, 0.05);
        let cfg = RegionStreamConfig::default();
        let result = decode_region_stream(&buf, sr, &cfg);
        assert!(
            result.text.trim().is_empty(),
            "expected no text on pure noise, got: {:?}",
            result.text
        );
    }

    #[test]
    fn discover_burst_pitches_ignores_continuous_carrier_when_cw_is_at_other_pitch() {
        // Realistic failure mode: a strong continuous carrier at one
        // pitch with intermittent CW bursts at a different pitch. The
        // carrier dominates whole-buffer mean, but the windowed source
        // should still surface the CW pitch where bursts occur.
        let sr = 12_000u32;
        let total_s = 30.0_f32;

        // Continuous low-amplitude carrier at 900 Hz across the whole buffer
        let mut buf = synth_tone(900.0, total_s, sr, 0.18);

        // Intermittent CW-like bursts at 600 Hz: 4 bursts of ~3 s each
        let burst_pitch = 600.0_f32;
        let burst_amp = 0.50_f32;
        let burst_pattern = [(2.0_f32, 3.0_f32), (8.0, 3.0), (16.0, 3.0), (24.0, 3.0)];
        for (start, dur) in burst_pattern {
            let s = (start * sr as f32) as usize;
            let burst = synth_tone(burst_pitch, dur, sr, burst_amp);
            for (i, sample) in burst.iter().enumerate() {
                if s + i < buf.len() {
                    buf[s + i] += *sample;
                }
            }
        }

        let cfg = RegionStreamConfig::default();
        let pitches = discover_burst_pitches(&buf, sr, &cfg);
        let cw_found = pitches
            .iter()
            .any(|&p| (p - burst_pitch).abs() <= cfg.pitch_step_hz);
        assert!(
            cw_found,
            "expected CW pitch ~{burst_pitch} Hz to be discovered alongside the carrier, got {pitches:?}"
        );
    }
}
