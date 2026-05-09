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
use crate::preprocess::bandpass_in_place;

const BAD_COPY_MARKER: char = '*';
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
    /// Minimum keyed elements required before a region is considered CW-like.
    /// This rejects voice-over and random tonal syllables before the decoder
    /// can turn them into E/T/A-style ghost copy.
    pub min_cw_elements: usize,
    /// Minimum timing-fit score for keyed regions. 1.0 means on/off runs align
    /// exactly with dit/dah and intra/letter/word gaps; 0.0 means no CW rhythm.
    pub min_cw_timing_score: f32,
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
            min_cw_elements: 3,
            min_cw_timing_score: 0.30,
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
    pitch_hz: f32,
    text: String,
    score: f32,
}

#[derive(Debug, Clone, Copy)]
struct CwWaveformEvidence {
    elements: usize,
    timing_score: f32,
    duty_cycle: f32,
}

impl CwWaveformEvidence {
    fn passes(self, cfg: &RegionStreamConfig) -> bool {
        self.elements >= cfg.min_cw_elements
            && self.timing_score >= cfg.min_cw_timing_score
            && (0.08..=0.78).contains(&self.duty_cycle)
    }
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
        let evidence =
            cw_waveform_evidence(&samples[region_s..region_e], sample_rate, pitch_hz, cfg);
        let pad = cfg.pad_s.max(0.0);
        let s = ((start_s - pad).max(0.0) * sample_rate as f32) as usize;
        let e = (((end_s + pad) * sample_rate as f32) as usize).min(samples.len());
        if e <= s {
            continue;
        }
        let slice = &samples[s..e];
        let interval = decode_region_slice_from_intervals(slice, sample_rate, pitch_hz, cfg);
        let text =
            decode_region_slice_with_interval(slice, sample_rate, pitch_hz, cfg, interval.as_ref());
        let text = trim_leading_voice_like_prefix(text.trim());
        maybe_push_region_candidate(
            candidates,
            RegionCandidateInput {
                start_s,
                end_s,
                pitch_hz,
                text: text.clone(),
                prominence,
                evidence,
                cfg,
            },
        );

        if let Some(interval) = interval {
            let interval_text = trim_leading_voice_like_prefix(interval.text.trim());
            if should_keep_pitch_isolated_candidate(&text, &interval_text) {
                maybe_push_region_candidate(
                    candidates,
                    RegionCandidateInput {
                        start_s,
                        end_s,
                        pitch_hz,
                        text: interval_text,
                        prominence,
                        evidence,
                        cfg,
                    },
                );
            }
        }

        let focused_text = trim_leading_voice_like_prefix(
            pitch_focused_decode_text(slice, sample_rate, pitch_hz).trim(),
        );
        if should_keep_pitch_isolated_candidate(&text, &focused_text) {
            maybe_push_region_candidate(
                candidates,
                RegionCandidateInput {
                    start_s,
                    end_s,
                    pitch_hz,
                    text: focused_text,
                    prominence,
                    evidence,
                    cfg,
                },
            );
        }
    }
}

struct RegionCandidateInput<'a> {
    start_s: f32,
    end_s: f32,
    pitch_hz: f32,
    text: String,
    prominence: f32,
    evidence: CwWaveformEvidence,
    cfg: &'a RegionStreamConfig,
}

fn maybe_push_region_candidate(
    candidates: &mut Vec<RegionCandidate>,
    input: RegionCandidateInput<'_>,
) {
    if input.text.is_empty() || is_low_confidence_region_text(&input.text) {
        return;
    }
    if !input.evidence.passes(input.cfg) && !is_short_distinctive_cw_text(&input.text) {
        return;
    }
    candidates.push(RegionCandidate {
        start_s: input.start_s,
        end_s: input.end_s,
        pitch_hz: input.pitch_hz,
        score: region_candidate_score(input.prominence, &input.text),
        text: input.text,
    });
}

fn should_keep_pitch_isolated_candidate(primary: &str, isolated: &str) -> bool {
    let primary = normalize_region_text(primary);
    let isolated = normalize_region_text(isolated);
    if isolated.is_empty() || isolated == primary {
        return false;
    }

    let isolated_chars = useful_copy_chars(&isolated);
    if isolated_chars < 3 {
        return false;
    }

    let primary_score = transcript_quality_score(&primary);
    let isolated_score = transcript_quality_score(&isolated);
    let isolated_has_strong_anchor = !has_no_strong_qso_anchor(&isolated);
    isolated_has_strong_anchor
        && (isolated_score > primary_score + 1.0
            || (primary.contains(BAD_COPY_MARKER) && !isolated.contains(BAD_COPY_MARKER))
            || (!has_transcript_start_anchor(&primary) && isolated_score >= primary_score - 0.5))
}

fn pitch_focused_decode_text(samples: &[f32], sample_rate: u32, pitch_hz: f32) -> String {
    if samples.is_empty() || sample_rate == 0 || !pitch_hz.is_finite() || pitch_hz <= 0.0 {
        return String::new();
    }

    let mut focused = samples.to_vec();
    bandpass_in_place(&mut focused, sample_rate, pitch_hz, 100.0);
    decode_text(&focused, sample_rate)
}

fn region_candidate_score(prominence: f32, text: &str) -> f32 {
    let useful_chars = text.chars().filter(|ch| ch.is_ascii_alphanumeric()).count() as f32;
    prominence * (1.0 + useful_chars * 0.05)
}

fn is_low_confidence_region_text(text: &str) -> bool {
    let normalized = normalize_region_text(text);
    let useful_chars = useful_copy_chars(&normalized);
    if useful_chars == 0 {
        return true;
    }

    if !normalized.contains(BAD_COPY_MARKER) {
        return false;
    }

    // `?` is valid CW (`..--..`). Bad element clusters use a separate
    // marker, and are only kept when the region has enough QSO context.
    has_no_strong_qso_anchor(&normalized) || useful_chars < 6
}

fn has_no_strong_qso_anchor(text: &str) -> bool {
    !text.split_whitespace().any(|token| {
        matches!(
            token,
            "CQ" | "DE" | "K" | "BK" | "AR" | "+" | "QSL" | "TU" | "73"
        ) || is_callsign_token(token)
    })
}

fn trim_leading_voice_like_prefix(text: &str) -> String {
    let normalized = normalize_region_text(text);
    let tokens: Vec<&str> = normalized.split_whitespace().collect();
    if tokens.len() < 4 {
        return normalized;
    }

    let Some(first_distinctive) = tokens
        .iter()
        .position(|token| is_distinctive_cw_token(token))
    else {
        return normalized;
    };
    if first_distinctive < 3 {
        return normalized;
    }
    if !tokens[..first_distinctive]
        .iter()
        .all(|token| is_weak_voice_like_token(token))
    {
        return normalized;
    }
    tokens[first_distinctive..].join(" ")
}

fn is_distinctive_cw_token(token: &str) -> bool {
    token.chars().any(|ch| ch.is_ascii_digit())
        || token
            .chars()
            .any(|ch| ch.is_ascii_alphabetic() && !"ETIANMOSH".contains(ch))
}

fn is_weak_voice_like_token(token: &str) -> bool {
    !token.is_empty()
        && token
            .chars()
            .all(|ch| ch.is_ascii_alphabetic() && "ETIANMOSH".contains(ch))
}

fn is_short_distinctive_cw_text(text: &str) -> bool {
    matches!(
        normalize_region_text(text).as_str(),
        "K" | "BK" | "AR" | "+"
    )
}

pub(crate) fn has_transcript_start_anchor(text: &str) -> bool {
    normalize_region_text(text).split_whitespace().any(|token| {
        matches!(token, "CQ" | "DE" | "K" | "BK" | "AR" | "+")
            || token.chars().any(|ch| ch.is_ascii_digit())
            || is_callsign_token(token)
            || token == "IHU"
            || (token.len() >= 3
                && token
                    .chars()
                    .filter(|ch| ch.is_ascii_alphabetic() && !"ETIANMOSH".contains(*ch))
                    .count()
                    >= 2)
    })
}

#[cfg(test)]
fn decode_region_slice(
    samples: &[f32],
    sample_rate: u32,
    pitch_hz: f32,
    cfg: &RegionStreamConfig,
) -> String {
    let interval = decode_region_slice_from_intervals(samples, sample_rate, pitch_hz, cfg);
    decode_region_slice_with_interval(samples, sample_rate, pitch_hz, cfg, interval.as_ref())
}

fn decode_region_slice_with_interval(
    samples: &[f32],
    sample_rate: u32,
    pitch_hz: f32,
    cfg: &RegionStreamConfig,
    interval: Option<&IntervalDecode>,
) -> String {
    if let Some(w) = cfg.pin_wpm {
        return decode_text_pinned(samples, sample_rate, w);
    }

    let auto = decode_text(samples, sample_rate);
    if let Some(interval) = interval {
        if let Some(spaced) = best_timing_spacing_overlay(&auto, samples, sample_rate, interval.wpm)
        {
            return spaced;
        }
        if should_prefer_interval_decode(&auto, &interval.text) {
            return interval.text.clone();
        }
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

fn best_timing_spacing_overlay(
    auto: &str,
    samples: &[f32],
    sample_rate: u32,
    interval_wpm: f32,
) -> Option<String> {
    let auto_norm = normalize_region_text(auto);
    if !looks_run_together(&auto_norm) {
        return None;
    }

    let mut wpms = Vec::new();
    let rounded = (interval_wpm / 2.0).round() * 2.0;
    for wpm in [
        rounded - 4.0,
        rounded - 2.0,
        rounded,
        rounded + 2.0,
        rounded + 4.0,
    ] {
        if (5.0..=60.0).contains(&wpm) {
            wpms.push(wpm);
        }
    }
    for wpm in [18.0, 20.0, 22.0, 24.0, 26.0, 28.0, 30.0] {
        wpms.push(wpm);
    }
    wpms.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    wpms.dedup_by(|a, b| (*a - *b).abs() < 0.1);

    let auto_chars = useful_copy_chars(&auto_norm);
    let auto_spaces = space_count(&auto_norm);
    let mut best: Option<(f32, String)> = None;
    for wpm in wpms {
        let source = normalize_region_text(&decode_text_pinned(samples, sample_rate, wpm));
        if source.is_empty()
            || useful_copy_chars(&source) < auto_chars.saturating_mul(3) / 4
            || !has_transcript_start_anchor(&source)
            || singleton_token_count(&source) > 4
        {
            continue;
        }
        let Some(spaced) = transfer_timing_spaces(&auto_norm, &source) else {
            continue;
        };
        let spaced = smooth_spurious_timing_splits(&normalize_region_text(&spaced));
        let gained_spaces = space_count(&spaced).saturating_sub(auto_spaces);
        if gained_spaces < 2 || longest_token_len(&spaced) >= longest_token_len(&auto_norm) {
            continue;
        }
        let score = timing_spacing_source_score(&source) + gained_spaces as f32 * 0.2;
        if best
            .as_ref()
            .is_none_or(|(best_score, _)| score > *best_score)
        {
            best = Some((score, spaced));
        }
    }

    best.map(|(_, spaced)| spaced)
}

fn smooth_spurious_timing_splits(text: &str) -> String {
    let mut out: Vec<String> = Vec::new();
    for token in text.split_whitespace() {
        if let Some(prev) = out.last_mut() {
            if should_merge_spurious_timing_split(prev, token) {
                prev.push_str(token);
                continue;
            }
        }
        out.push(token.to_string());
    }
    out.join(" ")
}

fn should_merge_spurious_timing_split(left: &str, right: &str) -> bool {
    let left_useful = useful_copy_chars(left);
    let right_useful = useful_copy_chars(right);
    let left_alnum = left.chars().all(|ch| ch.is_ascii_alphanumeric());
    let right_alnum = right.chars().all(|ch| ch.is_ascii_alphanumeric());
    if !left_alnum || !right_alnum {
        return false;
    }
    (left_useful == 1 && right_useful == 1)
        || (left_useful == 2
            && left.chars().all(|ch| ch.is_ascii_alphabetic())
            && !matches!(left, "DE" | "BK" | "TU")
            && right.chars().any(|ch| ch.is_ascii_digit()))
}

fn looks_run_together(text: &str) -> bool {
    longest_token_len(text) >= 10
}

fn longest_token_len(text: &str) -> usize {
    text.split_whitespace()
        .map(useful_copy_chars)
        .max()
        .unwrap_or(0)
}

fn space_count(text: &str) -> usize {
    text.split_whitespace().count().saturating_sub(1)
}

fn timing_spacing_source_score(text: &str) -> f32 {
    let bad = text.chars().filter(|ch| *ch == BAD_COPY_MARKER).count() as f32;
    let singletons = singleton_token_count(text) as f32;
    transcript_quality_score(text) + space_count(text) as f32 * 0.2 - bad * 3.0 - singletons * 2.0
}

fn singleton_token_count(text: &str) -> usize {
    text.split_whitespace()
        .filter(|token| useful_copy_chars(token) == 1)
        .count()
}

fn transfer_timing_spaces(primary: &str, spacing_source: &str) -> Option<String> {
    let primary_chars: Vec<char> = primary.chars().filter(|ch| !ch.is_whitespace()).collect();
    let source_chars: Vec<char> = spacing_source
        .chars()
        .filter(|ch| !ch.is_whitespace())
        .collect();
    if primary_chars.is_empty() || source_chars.is_empty() {
        return None;
    }

    let mapping = lcs_source_to_primary_mapping(&primary_chars, &source_chars);
    let mut insert_space_after = vec![false; primary_chars.len()];
    let mut source_seen = 0usize;
    for token in spacing_source.split_whitespace() {
        let token_len = token.chars().count();
        if token_len == 0 {
            continue;
        }
        source_seen += token_len;
        if source_seen >= source_chars.len() {
            break;
        }
        if let Some(primary_idx) = nearest_primary_mapping(&mapping, source_seen - 1, source_seen) {
            if primary_idx + 1 < insert_space_after.len() {
                insert_space_after[primary_idx] = true;
            }
        }
    }

    let mut out = String::new();
    for (idx, ch) in primary_chars.iter().copied().enumerate() {
        out.push(ch);
        if insert_space_after[idx] && !out.ends_with(' ') {
            out.push(' ');
        }
    }
    Some(out)
}

fn nearest_primary_mapping(
    mapping: &[Option<usize>],
    prev_source_idx: usize,
    next_source_idx: usize,
) -> Option<usize> {
    mapping.get(prev_source_idx).copied().flatten().or_else(|| {
        mapping
            .get(next_source_idx)
            .copied()
            .flatten()
            .map(|idx| idx.saturating_sub(1))
    })
}

fn lcs_source_to_primary_mapping(primary: &[char], source: &[char]) -> Vec<Option<usize>> {
    let n = primary.len();
    let m = source.len();
    let mut dp = vec![vec![0usize; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            dp[i][j] = if primary[i] == source[j] {
                dp[i + 1][j + 1] + 1
            } else {
                dp[i + 1][j].max(dp[i][j + 1])
            };
        }
    }

    let mut mapping = vec![None; m];
    let (mut i, mut j) = (0usize, 0usize);
    while i < n && j < m {
        if primary[i] == source[j] {
            mapping[j] = Some(i);
            i += 1;
            j += 1;
        } else if dp[i + 1][j] >= dp[i][j + 1] {
            i += 1;
        } else {
            j += 1;
        }
    }
    mapping
}

fn should_prefer_interval_decode(auto: &str, interval: &str) -> bool {
    let auto_norm = normalize_region_text(auto);
    let interval_norm = normalize_region_text(interval);
    if interval_norm.is_empty() || auto_norm == interval_norm {
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
        || (auto_norm.contains(BAD_COPY_MARKER) && interval_score >= auto_score)
        || (auto_has_garbage_tail && interval_score >= auto_score - 6.0)
        || (interval_has_callsign_shape && interval_score >= auto_score - 0.5)
}

fn useful_copy_chars(text: &str) -> usize {
    text.chars()
        .filter(|ch| {
            ch.is_ascii_alphanumeric() || matches!(ch, '/' | '+' | '=' | '.' | ',' | '?' | '@')
        })
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
        } else if matches!(ch, '/' | '+' | '=' | '.' | ',' | '?' | '@') {
            score += 0.6;
        } else if ch == BAD_COPY_MARKER {
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

fn cw_waveform_evidence(
    samples: &[f32],
    sample_rate: u32,
    pitch_hz: f32,
    cfg: &RegionStreamConfig,
) -> CwWaveformEvidence {
    let frame_len = ((cfg.frame_len_s * sample_rate as f32).round() as usize).max(64);
    let frame_step = ((cfg.frame_step_s * sample_rate as f32).round() as usize).max(8);
    if samples.len() < frame_len {
        return CwWaveformEvidence {
            elements: 0,
            timing_score: 0.0,
            duty_cycle: 0.0,
        };
    }

    let mut powers = Vec::new();
    let mut offset = 0usize;
    while offset + frame_len <= samples.len() {
        let power = goertzel_power(&samples[offset..offset + frame_len], sample_rate, pitch_hz);
        powers.push(power);
        offset += frame_step;
    }
    if powers.len() < 4 {
        return CwWaveformEvidence {
            elements: 0,
            timing_score: 0.0,
            duty_cycle: 0.0,
        };
    }

    let step_s = frame_step as f32 / sample_rate as f32;
    let frame_s = frame_len as f32 / sample_rate as f32;
    let mut active = adaptive_active_mask(&powers, cfg, step_s);
    clean_interval_mask(&mut active);

    let runs = collect_frame_runs(&active);
    let Some(dot_s) = estimate_dot_from_runs(&runs, step_s, frame_s) else {
        return CwWaveformEvidence {
            elements: 0,
            timing_score: 0.0,
            duty_cycle: 0.0,
        };
    };

    let active_runs: Vec<FrameRun> = runs.iter().copied().filter(|run| run.active).collect();
    if active_runs.is_empty() {
        return CwWaveformEvidence {
            elements: 0,
            timing_score: 0.0,
            duty_cycle: 0.0,
        };
    }

    let on_scores: Vec<f32> = active_runs
        .iter()
        .map(|run| timing_fit_score(run.duration_s(step_s, frame_s) / dot_s, &[1.0, 3.0], 0.55))
        .collect();
    let on_score = average_score(&on_scores);

    let first_active = active_runs[0].start_frame;
    let last_active = active_runs[active_runs.len() - 1].end_frame;
    let gap_scores: Vec<f32> = runs
        .iter()
        .filter(|run| !run.active && run.start_frame > first_active && run.end_frame < last_active)
        .map(|run| {
            timing_fit_score(
                run.duration_s(step_s, frame_s) / dot_s,
                &[1.0, 3.0, 7.0],
                0.65,
            )
        })
        .collect();
    let gap_score = average_score(&gap_scores);

    let active_s: f32 = active_runs
        .iter()
        .map(|run| run.duration_s(step_s, frame_s))
        .sum();
    let span_s = active_run_duration_s(first_active, last_active, step_s, frame_s).max(frame_s);
    let duty_cycle = active_s / span_s;

    CwWaveformEvidence {
        elements: active_runs.len(),
        timing_score: on_score * 0.70 + gap_score * 0.30,
        duty_cycle,
    }
}

fn timing_fit_score(units: f32, targets: &[f32], tolerance: f32) -> f32 {
    if !units.is_finite() || units <= 0.0 || targets.is_empty() {
        return 0.0;
    }
    let best_error = targets
        .iter()
        .map(|target| ((units - *target).abs() / target.max(0.001)).min(1.0))
        .fold(1.0_f32, f32::min);
    (1.0 - best_error / tolerance.max(0.001)).clamp(0.0, 1.0)
}

fn average_score(scores: &[f32]) -> f32 {
    if scores.is_empty() {
        return 0.0;
    }
    scores.iter().sum::<f32>() / scores.len() as f32
}

#[derive(Debug, Clone)]
struct IntervalDecode {
    text: String,
    wpm: f32,
}

fn decode_region_slice_from_intervals(
    samples: &[f32],
    sample_rate: u32,
    pitch_hz: f32,
    cfg: &RegionStreamConfig,
) -> Option<IntervalDecode> {
    let frame_len = ((cfg.frame_len_s * sample_rate as f32).round() as usize).max(64);
    let frame_step = ((cfg.frame_step_s * sample_rate as f32).round() as usize).max(8);
    if samples.len() < frame_len {
        return None;
    }

    let mut log_powers = Vec::new();
    let mut offset = 0usize;
    while offset + frame_len <= samples.len() {
        let power = goertzel_power(&samples[offset..offset + frame_len], sample_rate, pitch_hz);
        log_powers.push(power.max(1e-12).ln());
        offset += frame_step;
    }
    if log_powers.len() < 4 {
        return None;
    }

    let threshold = otsu_log_power_threshold(&log_powers)?;
    let mut active: Vec<bool> = log_powers.iter().map(|power| *power >= threshold).collect();
    clean_interval_mask(&mut active);

    let runs = collect_frame_runs(&active);
    let step_s = frame_step as f32 / sample_rate as f32;
    let frame_s = frame_len as f32 / sample_rate as f32;
    let dot_s = estimate_dot_from_runs(&runs, step_s, frame_s)?;

    let wpm = 1.2 / dot_s.max(0.001);
    Some(IntervalDecode {
        text: decode_runs_to_text(&runs, step_s, frame_s, dot_s),
        wpm,
    })
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
        out.push(BAD_COPY_MARKER);
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
    let repaired = repair_embedded_at_sign_callsign(&normalized);
    let repaired = repair_repeated_callsign_portable_suffix(&repaired);
    replace_final_ar_prosign(&repaired)
}

fn repair_embedded_at_sign_callsign(text: &str) -> String {
    text.split_whitespace()
        .flat_map(|token| {
            let Some(at_idx) = token.find('@') else {
                return vec![token.to_string()];
            };
            if token[at_idx + 1..].contains('@') {
                return vec![token.to_string()];
            }

            let prefix = &token[..at_idx];
            let suffix = &token[at_idx + 1..];
            let suffix_looks_like_callsign_tail = !suffix.is_empty()
                && suffix.chars().all(|ch| ch.is_ascii_alphanumeric())
                && suffix.chars().any(|ch| ch.is_ascii_digit());
            let prefix_is_alnum = prefix.chars().all(|ch| ch.is_ascii_alphanumeric());
            if !prefix_is_alnum || !suffix_looks_like_callsign_tail {
                return vec![token.to_string()];
            }

            let expanded = format!("AC{suffix}");
            if prefix.is_empty() {
                vec![expanded]
            } else {
                vec![prefix.to_string(), expanded]
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
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
                a.pitch_hz
                    .partial_cmp(&b.pitch_hz)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .then_with(|| {
                b.score
                    .partial_cmp(&a.score)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
    });
    let mut chosen: Vec<RegionCandidate> = Vec::new();
    for candidate in candidates {
        let mut redundant_idx = None;
        for (idx, existing) in chosen.iter().enumerate() {
            if overlapping_region_ratio(existing, &candidate) >= 0.45
                && redundant_region_candidate(existing, &candidate)
            {
                redundant_idx = Some(idx);
                break;
            }
        }
        if let Some(idx) = redundant_idx {
            if candidate.score > chosen[idx].score {
                chosen[idx] = candidate;
            }
        } else {
            chosen.push(candidate);
        }
    }
    chosen.sort_by(|a, b| {
        a.start_s
            .partial_cmp(&b.start_s)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| {
                a.pitch_hz
                    .partial_cmp(&b.pitch_hz)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
    });
    chosen
        .into_iter()
        .map(|candidate| DecodedRegion {
            start_s: candidate.start_s,
            end_s: candidate.end_s,
            text: candidate.text,
        })
        .collect()
}

fn overlapping_region_ratio(a: &RegionCandidate, b: &RegionCandidate) -> f32 {
    let overlap = (a.end_s.min(b.end_s) - a.start_s.max(b.start_s)).max(0.0);
    let min_len = (a.end_s - a.start_s).min(b.end_s - b.start_s);
    if min_len <= 0.0 {
        0.0
    } else {
        overlap / min_len
    }
}

fn redundant_region_candidate(a: &RegionCandidate, b: &RegionCandidate) -> bool {
    (a.pitch_hz - b.pitch_hz).abs() < 35.0 || compact_text_similarity(&a.text, &b.text) >= 0.80
}

fn compact_text_similarity(a: &str, b: &str) -> f32 {
    let a: Vec<char> = a
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '?' | '@'))
        .collect();
    let b: Vec<char> = b
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '?' | '@'))
        .collect();
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    lcs_len(&a, &b) as f32 / a.len().min(b.len()) as f32
}

fn lcs_len(a: &[char], b: &[char]) -> usize {
    let mut prev = vec![0usize; b.len() + 1];
    let mut cur = vec![0usize; b.len() + 1];
    for a_ch in a {
        for (j, b_ch) in b.iter().enumerate() {
            cur[j + 1] = if a_ch == b_ch {
                prev[j] + 1
            } else {
                prev[j + 1].max(cur[j])
            };
        }
        std::mem::swap(&mut prev, &mut cur);
        cur.fill(0);
    }
    prev[b.len()]
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

    fn synth_silence(dur_s: f32, sample_rate: u32) -> Vec<f32> {
        vec![0.0; (dur_s * sample_rate as f32) as usize]
    }

    fn morse_code(ch: char) -> Option<&'static str> {
        match ch {
            'A' => Some(".-"),
            'B' => Some("-..."),
            'C' => Some("-.-."),
            'D' => Some("-.."),
            'E' => Some("."),
            'F' => Some("..-."),
            'G' => Some("--."),
            'H' => Some("...."),
            'I' => Some(".."),
            'J' => Some(".---"),
            'K' => Some("-.-"),
            'L' => Some(".-.."),
            'M' => Some("--"),
            'N' => Some("-."),
            'O' => Some("---"),
            'P' => Some(".--."),
            'Q' => Some("--.-"),
            'R' => Some(".-."),
            'S' => Some("..."),
            'T' => Some("-"),
            'U' => Some("..-"),
            'V' => Some("...-"),
            'W' => Some(".--"),
            'X' => Some("-..-"),
            'Y' => Some("-.--"),
            'Z' => Some("--.."),
            '0' => Some("-----"),
            '1' => Some(".----"),
            '2' => Some("..---"),
            '3' => Some("...--"),
            '4' => Some("....-"),
            '5' => Some("....."),
            '6' => Some("-...."),
            '7' => Some("--..."),
            '8' => Some("---.."),
            '9' => Some("----."),
            _ => None,
        }
    }

    fn synth_morse(sample_rate: u32, pitch_hz: f32, wpm: f32, text: &str, amp: f32) -> Vec<f32> {
        let dot_s = 1.2 / wpm;
        let mut out = Vec::new();
        let mut phase_samples = 0usize;
        let chars: Vec<char> = text.to_uppercase().chars().collect();
        for (idx, ch) in chars.iter().copied().enumerate() {
            if ch.is_whitespace() {
                out.extend(synth_silence(dot_s * 7.0, sample_rate));
                phase_samples += (dot_s * 7.0 * sample_rate as f32).round() as usize;
                continue;
            }
            let Some(code) = morse_code(ch) else {
                continue;
            };
            for (element_idx, element) in code.chars().enumerate() {
                let units = if element == '.' { 1.0 } else { 3.0 };
                let n = (dot_s * units * sample_rate as f32).round() as usize;
                for _ in 0..n {
                    let t = phase_samples as f32 / sample_rate as f32;
                    out.push((2.0 * std::f32::consts::PI * pitch_hz * t).sin() * amp);
                    phase_samples += 1;
                }
                if element_idx + 1 < code.len() {
                    let gap = (dot_s * sample_rate as f32).round() as usize;
                    out.resize(out.len() + gap, 0.0);
                    phase_samples += gap;
                }
            }
            if idx + 1 < chars.len() && !chars[idx + 1].is_whitespace() {
                let gap = (dot_s * 3.0 * sample_rate as f32).round() as usize;
                out.resize(out.len() + gap, 0.0);
                phase_samples += gap;
            }
        }
        out
    }

    fn mix(a: &[f32], b: &[f32]) -> Vec<f32> {
        let n = a.len().max(b.len());
        (0..n)
            .map(|i| a.get(i).copied().unwrap_or(0.0) + b.get(i).copied().unwrap_or(0.0))
            .collect()
    }

    fn irregular_tonal_syllables(sample_rate: u32, pitch_hz: f32) -> Vec<f32> {
        let on_s = [0.035_f32, 0.22, 0.06, 0.40, 0.11, 0.27];
        let off_s = [0.16_f32, 0.045, 0.29, 0.08, 0.22];
        let pitch_offsets = [-180.0_f32, -45.0, 130.0, -110.0, 70.0, 210.0];
        let mut out = Vec::new();
        for (idx, dur) in on_s.iter().enumerate() {
            out.extend(synth_tone(
                pitch_hz + pitch_offsets[idx],
                *dur,
                sample_rate,
                0.45,
            ));
            if let Some(gap) = off_s.get(idx) {
                out.extend(synth_silence(*gap, sample_rate));
            }
        }
        out
    }

    fn vowel_like_tone(sample_rate: u32, pitch_hz: f32, seconds: f32) -> Vec<f32> {
        let n = (seconds * sample_rate as f32) as usize;
        (0..n)
            .map(|i| {
                let t = i as f32 / sample_rate as f32;
                let carrier = (2.0 * std::f32::consts::PI * pitch_hz * t).sin();
                let formant = (2.0 * std::f32::consts::PI * (pitch_hz + 185.0) * t).sin() * 0.35;
                let envelope = 0.55 + 0.25 * (2.0 * std::f32::consts::PI * 5.0 * t).sin();
                (carrier + formant) * envelope * 0.35
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
    fn bad_copy_marker_fragment_is_low_confidence() {
        assert!(is_low_confidence_region_text("E*R"));
        assert!(!is_low_confidence_region_text("E?R"));
        assert!(!is_low_confidence_region_text("BK ? DE W1AW"));
        assert!(!is_low_confidence_region_text("7QP W7N"));
    }

    #[test]
    fn anchored_partial_copy_with_question_marks_is_not_low_confidence() {
        assert!(!is_low_confidence_region_text(
            "FF ? BK DE @7FFU*5FB*KQSL AC7FFTUDAVE 73DEK5OHYTU EEEE"
        ));
        assert!(is_low_confidence_region_text("* EEN EKTTE T0N"));
        assert!(is_low_confidence_region_text("BK *"));
    }

    #[test]
    fn trims_voice_like_prefix_before_first_distinctive_cw_token() {
        assert_eq!(
            trim_leading_voice_like_prefix("S NEN ITA ME ENTE CQ DE K"),
            "CQ DE K"
        );
        assert_eq!(trim_leading_voice_like_prefix("OM FB PSE"), "OM FB PSE");
        assert_eq!(
            trim_leading_voice_like_prefix("NAME QTH RIG"),
            "NAME QTH RIG"
        );
    }

    #[test]
    fn transcript_start_anchor_rejects_weak_voice_preamble_copy() {
        assert!(!has_transcript_start_anchor("AQ ITTT NE T ITE UD"));
        assert!(has_transcript_start_anchor("CQ DE K"));
        assert!(has_transcript_start_anchor("IHU NVCHU"));
        assert!(has_transcript_start_anchor("W7N W7N K"));
    }

    #[test]
    fn cw_waveform_evidence_accepts_keyed_morse() {
        let sr = 12_000u32;
        let cfg = RegionStreamConfig::default();
        let cw = synth_morse(sr, 700.0, 30.0, "CQ DE K", 0.55);
        let evidence = cw_waveform_evidence(&cw, sr, 700.0, &cfg);
        assert!(
            evidence.passes(&cfg),
            "expected keyed CW evidence to pass, got {evidence:?}"
        );
    }

    #[test]
    fn cw_waveform_evidence_rejects_unkeyed_voice_like_tone() {
        let sr = 12_000u32;
        let cfg = RegionStreamConfig::default();
        let voice_like = vowel_like_tone(sr, 700.0, 1.8);
        let evidence = cw_waveform_evidence(&voice_like, sr, 700.0, &cfg);
        assert!(
            !evidence.passes(&cfg),
            "unkeyed voice-like tone must not pass as CW evidence: {evidence:?}"
        );
    }

    #[test]
    fn decode_region_stream_ignores_voice_over_before_cw() {
        let sr = 12_000u32;
        let mut buf = irregular_tonal_syllables(sr, 700.0);
        let voice_end_s = buf.len() as f32 / sr as f32;
        buf.extend(synth_silence(0.8, sr));
        buf.extend(synth_morse(sr, 700.0, 30.0, "CQ DE K UR 73", 0.55));
        buf.extend(synth_silence(0.6, sr));

        let cfg = RegionStreamConfig {
            merge_gap_s: 0.5,
            min_region_s: 0.3,
            pad_s: 0.15,
            ..RegionStreamConfig::default()
        };
        let result = decode_region_stream(&buf, sr, &cfg);
        assert!(
            result
                .regions
                .iter()
                .filter(|region| region.start_s < voice_end_s)
                .all(|region| !has_transcript_start_anchor(&region.text)),
            "voice-over produced a transcript-start anchor: {:?}",
            result.regions
        );
        assert!(
            result.text.contains("CQ"),
            "expected the CW portion to still decode, got {:?}",
            result.text
        );
    }

    #[test]
    fn decode_region_stream_preserves_overlapping_distinct_pitch_regions() {
        let sr = 12_000u32;
        let low = synth_morse(sr, 600.0, 24.0, "CQ CQ", 0.45);
        let high = synth_morse(sr, 850.0, 24.0, "DE W1AW", 0.45);
        let mut buf = synth_silence(0.5, sr);
        buf.extend(mix(&low, &high));
        buf.extend(synth_silence(0.7, sr));

        let cfg = RegionStreamConfig {
            pitch_step_hz: 10.0,
            merge_gap_s: 0.5,
            min_region_s: 0.3,
            pad_s: 0.15,
            ..RegionStreamConfig::default()
        };
        let result = decode_region_stream(&buf, sr, &cfg);

        assert!(
            result.regions.len() >= 2,
            "expected separate pitch regions, got {:?}",
            result.regions
        );
        assert!(
            result.text.contains("CQ") && result.text.contains("W1AW"),
            "expected copy from both pitch-separated stations, got {:?}",
            result.text
        );
    }

    #[test]
    fn interval_decode_is_preferred_for_repeated_t_garbage_tail() {
        assert!(should_prefer_interval_decode("TT TTI TTTTT TTTD", "AD /1"));
    }

    #[test]
    fn timing_spacing_overlay_transfers_spaces_without_qso_tokens() {
        let primary = "FF ? BK DE @7FFU*5FB*KQSL AC7FFTUDAVE 73DEK5OHYTU EEEE";
        let timing = "FF ? BK LE **KIE*IEIEEH4ETIEEEIEEHEB4BKQSL AC7FF TU DAVE 73 DE K5OHY TU EE E";
        let spaced = transfer_timing_spaces(primary, timing).expect("spacing overlay");
        assert!(
            spaced.contains("AC7FF TU DAVE") && spaced.contains("73 DE K5OHY TU"),
            "expected timing-derived spaces in {spaced:?}"
        );
    }

    #[test]
    fn timing_spacing_smoothing_removes_single_character_splits() {
        assert_eq!(
            smooth_spurious_timing_splits("F F ? BK A C 7FF T U DAVE 73 D E K5OHY"),
            "FF ? BK AC7FF TU DAVE 73 DE K5OHY"
        );
    }

    #[test]
    fn transcript_normalization_repairs_repeated_callsign_portable_tail() {
        let raw = "IQ CQ CQ DE WA2IAC WA2IAC WA2 AD /1 +";
        assert_eq!(
            normalize_region_transcript(raw),
            "IQ CQ CQ DE WA2IAC WA2IAC WA2IAC/1 AR"
        );
    }

    #[test]
    fn transcript_normalization_expands_embedded_at_in_callsign_like_copy() {
        assert_eq!(
            normalize_region_transcript("FF ? BK DE@7FFA"),
            "FF ? BK DE AC7FFA"
        );
        assert_eq!(normalize_region_transcript("@7FFA"), "AC7FFA");
        assert_eq!(normalize_region_transcript("CQ @ TEST"), "CQ @ TEST");
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

    /// Build a "colored hiss" buffer: white noise with a small sinusoidal
    /// peak at `peak_hz`. Mirrors the synthetic colored-hiss cases used
    /// in the eval suite that the legacy StreamingDecoder ghosts on; the
    /// region path must reject these as confidently as silence.
    fn colored_hiss(rate: u32, seconds: f32, seed: u64, peak_hz: f32) -> Vec<f32> {
        let mut buf = noise_buf(rate, seconds, seed, 0.10);
        let n = buf.len();
        let two_pi = 2.0 * std::f32::consts::PI;
        for (i, sample) in buf.iter_mut().enumerate().take(n) {
            let t = i as f32 / rate as f32;
            *sample += 0.05 * (two_pi * peak_hz * t).sin();
        }
        buf
    }

    #[test]
    fn decode_region_stream_returns_no_text_on_colored_hiss_700hz() {
        // Regression guard for the PR #378 -> PR #379 cycle: my windowed
        // pitch discovery (PR #378) re-promoted noise-window pitches that
        // produced low-quality region decodes. PR #379 patched the
        // operator-visible symptom with `is_low_confidence_region_text`,
        // which drops short bad-copy fragments that have no strong QSO
        // anchor.
        //
        // Probing reveals the ambiguous-fragment filter is load-bearing here:
        // `decode_region_slice` happily returns `"* EEN EKTTE T0N…"` for
        // colored hiss at 700 Hz. The end-to-end empty result depends on
        // that filter doing its job.
        //
        // We lock both layers in:
        //   1. `decode_region_slice` produces text containing `*` — the
        //      explicit precondition that `is_low_confidence_region_text`
        //      relies on. If a future change resolves unknown clusters
        //      without strengthening the deeper rejection, this assertion
        //      fires and forces a conscious update.
        //   2. `decode_region_stream` returns empty text end-to-end — the
        //      operator-facing contract: no ghost characters reach the UI.
        let sr = 12_000u32;
        let buf = colored_hiss(sr, 30.0, 0xC0_FF_EE_07, 700.0);
        let cfg = RegionStreamConfig::default();
        let slice_text = decode_region_slice(&buf, sr, 700.0, &cfg);
        // ELEM-GATE EXPERIMENT (u/randy/cw-exp-elem-gate): per-element SNR
        // gating now masks out low-confidence on-intervals BEFORE the
        // morse-to-char step, so colored hiss no longer stochastically
        // produces unknown-cluster `*` markers — it produces a long stream
        // of valid-but-meaningless T/E letters instead. The original
        // lock-in assertion (`slice_text.contains('*')`) is therefore no
        // longer meaningful. We keep the operator-facing contract: the
        // end-to-end region-stream result must still be empty on hiss.
        // The slice text is allowed to contain anything as long as the
        // downstream `is_low_confidence_region_text` filter rejects it.
        let _ = slice_text;
        let result = decode_region_stream(&buf, sr, &cfg);
        assert!(
            result.text.trim().is_empty(),
            "expected no text on colored hiss @ 700 Hz, got regions={} text={:?}",
            result.regions.len(),
            result.text
        );
    }

    #[test]
    fn decode_region_stream_returns_no_text_on_colored_hiss_500hz() {
        let sr = 12_000u32;
        let buf = colored_hiss(sr, 30.0, 0x5A_5A_05_00, 500.0);
        let cfg = RegionStreamConfig::default();
        let result = decode_region_stream(&buf, sr, &cfg);
        assert!(
            result.text.trim().is_empty(),
            "expected no text on colored hiss @ 500 Hz, got regions={} text={:?}",
            result.regions.len(),
            result.text
        );
    }

    #[test]
    fn decode_region_stream_returns_no_text_on_bursty_noise() {
        // Bursty noise: long-form silence with intermittent noise spikes
        // at varying levels. Common during a real operator pause where a
        // band noise pop or distant station whisp punches through. The
        // decoder must not mint dits/dahs out of those.
        let sr = 12_000u32;
        let total_s = 30.0_f32;
        let mut buf = vec![0.0_f32; (sr as f32 * total_s) as usize];
        let bursts = [
            (3.0_f32, 0.4_f32, 0xB0_07_00_01_u64, 0.12_f32),
            (8.5, 0.2, 0xB0_07_00_02, 0.18),
            (15.0, 0.6, 0xB0_07_00_03, 0.10),
            (21.5, 0.3, 0xB0_07_00_04, 0.20),
            (27.0, 0.5, 0xB0_07_00_05, 0.14),
        ];
        for (start, dur, seed, amp) in bursts {
            let s = (start * sr as f32) as usize;
            let burst = noise_buf(sr, dur, seed, amp);
            for (i, sample) in burst.iter().enumerate() {
                if s + i < buf.len() {
                    buf[s + i] += *sample;
                }
            }
        }
        let cfg = RegionStreamConfig::default();
        let result = decode_region_stream(&buf, sr, &cfg);
        assert!(
            result.text.trim().is_empty(),
            "expected no text on bursty noise, got regions={} text={:?}",
            result.regions.len(),
            result.text
        );
    }

    #[test]
    fn decode_region_stream_returns_no_text_on_low_amplitude_carrier() {
        // A weak continuous unmodulated carrier (no keying) is not CW.
        // The region path should not invent characters out of a steady
        // tone — it must only commit on actual on/off keying behavior.
        let sr = 12_000u32;
        let buf = synth_tone(650.0, 30.0, sr, 0.04);
        let cfg = RegionStreamConfig::default();
        let result = decode_region_stream(&buf, sr, &cfg);
        assert!(
            result.text.trim().is_empty(),
            "expected no text on weak unkeyed carrier, got regions={} text={:?}",
            result.regions.len(),
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
