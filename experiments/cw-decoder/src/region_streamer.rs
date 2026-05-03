//! Region-based streaming CW decoder.
//!
//! On each decode cycle, runs [`decode_region_stream`] over the rolling audio
//! buffer to find active morse bursts (separated by background static), and
//! decodes each completed burst with the ditdah baseline.
//!
//! Unlike the v3 envelope streamer, regions are committed in append-only
//! order: a region only enters the transcript once it has been "stable" for
//! at least [`RegionStreamerConfig::stable_latency_s`] seconds (i.e. trailing
//! static has been observed long enough that no further morse from that
//! burst is expected). This eliminates the mid-region rewrites that occur
//! when a single burst is still being received.
//!
//! Designed for the real-world case where an operator transmits, pauses
//! during static, then transmits again at a different speed (e.g.
//! `IHU NVCHU 7QP W7N 7QP W7N` with bursts at 13 / 8 / 39 / 29 WPM).

use crate::region_stream::{decode_region_stream, RegionStreamConfig};

/// Tunables for the region-based streamer.
#[derive(Debug, Clone)]
pub struct RegionStreamerConfig {
    /// Underlying region detection / decode config.
    pub region: RegionStreamConfig,
    /// Trailing-static seconds required before a region is committed. Must
    /// exceed the longest expected intra-burst gap. Default 0.6s, which is
    /// larger than a word gap at 5 WPM (1.2s dot * 5 = 6.0s — actually for
    /// 5 WPM the word gap is 1.2s, so bursts that include 5 WPM word gaps
    /// would need a higher latency; default tuned for the common 8+ WPM case).
    pub stable_latency_s: f32,
}

impl Default for RegionStreamerConfig {
    fn default() -> Self {
        // Defaults in [`RegionStreamConfig::default`] (merge_gap_s = 3.0,
        // min_region_s = 0.6) are too eager for real-world bursts that
        // are separated by ~0.5-2.5s of static and may be as short as a
        // single trigraph. Tighten them so each burst stays its own
        // region.
        let region = RegionStreamConfig {
            merge_gap_s: 0.5,
            min_region_s: 0.3,
            threshold_factor: 0.30,
            pad_s: 0.15,
            ..RegionStreamConfig::default()
        };
        Self {
            region,
            stable_latency_s: 0.6,
        }
    }
}

/// One region newly committed by [`RegionStreamer::ingest`].
#[derive(Debug, Clone)]
pub struct CommittedRegion {
    pub start_s: f32,
    pub end_s: f32,
    pub text: String,
}

/// Stateful streaming wrapper around [`decode_region_stream`].
///
/// Buffers samples in absolute session time, runs region detection on every
/// `ingest` call, and reports newly stable regions in append-only order.
#[derive(Debug)]
pub struct RegionStreamer {
    sample_rate: u32,
    cfg: RegionStreamerConfig,
    buffer: Vec<f32>,
    /// End time (seconds, relative to buffer start) of the last region that
    /// has already been committed. Used to deduplicate emissions across
    /// successive decode cycles.
    last_committed_end_s: f32,
    /// Append-only running transcript of all committed regions, joined by
    /// single spaces.
    committed_text: String,
    /// Total seconds of audio that have been ingested so far. Equals
    /// `buffer.len() / sample_rate` while no buffer trimming is performed.
    head_s: f32,
}

impl RegionStreamer {
    pub fn new(sample_rate: u32) -> Self {
        Self::with_config(sample_rate, RegionStreamerConfig::default())
    }

    pub fn with_config(sample_rate: u32, cfg: RegionStreamerConfig) -> Self {
        Self {
            sample_rate,
            cfg,
            buffer: Vec::new(),
            last_committed_end_s: 0.0,
            committed_text: String::new(),
            head_s: 0.0,
        }
    }

    /// Append samples to the rolling buffer. Cheap; does no decoding.
    /// Call [`Self::try_commit`] periodically on a decode cadence to
    /// actually run region detection and emit committed regions.
    pub fn ingest(&mut self, samples: &[f32]) {
        self.buffer.extend_from_slice(samples);
        self.head_s = self.buffer.len() as f32 / self.sample_rate.max(1) as f32;
    }

    /// Run region detection over the current buffer and return any
    /// regions that have just become stable (committed). Should be
    /// invoked on a fixed cadence (typically every 250-500 ms of
    /// wallclock time) — not on every chunk feed, because region
    /// detection scans the entire buffer.
    pub fn try_commit(&mut self) -> Vec<CommittedRegion> {
        self.commit_stable_regions(false)
    }

    /// Treat the current buffer as the final audio, committing any
    /// remaining regions regardless of trailing-static latency. Call once
    /// at end-of-stream.
    pub fn flush(&mut self) -> Vec<CommittedRegion> {
        self.commit_stable_regions(true)
    }

    /// Full append-only transcript so far.
    pub fn transcript(&self) -> &str {
        &self.committed_text
    }

    /// Buffered audio length in seconds (relative to the current buffer
    /// start, i.e. always >= 0 and bounded by the trim policy).
    pub fn buffered_seconds(&self) -> f32 {
        self.head_s
    }

    /// Reset all streaming state: drop the buffer, clear the committed
    /// transcript, and reset the commit cursor. Used by the live mic
    /// path's "ResetLock" stdin-control message so the operator can
    /// start a new session without restarting the decoder process.
    pub fn reset(&mut self) {
        self.buffer.clear();
        self.last_committed_end_s = 0.0;
        self.head_s = 0.0;
        self.committed_text.clear();
    }

    /// Drop everything in the buffer that ends more than `keep_back_s`
    /// seconds before the last committed region. Bounds memory growth
    /// during long-running live capture without losing any in-flight
    /// (uncommitted) audio. Safe to call after `try_commit`. The
    /// committed transcript is preserved.
    pub fn trim_committed(&mut self, keep_back_s: f32) {
        let cutoff = (self.last_committed_end_s - keep_back_s.max(0.0)).max(0.0);
        if cutoff <= 0.0 {
            return;
        }
        let sr = self.sample_rate.max(1) as f32;
        let cut_samples = (cutoff * sr) as usize;
        if cut_samples == 0 || cut_samples >= self.buffer.len() {
            return;
        }
        self.buffer.drain(..cut_samples);
        let shift = cut_samples as f32 / sr;
        self.last_committed_end_s = (self.last_committed_end_s - shift).max(0.0);
        self.head_s = self.buffer.len() as f32 / sr;
    }

    fn commit_stable_regions(&mut self, force_all: bool) -> Vec<CommittedRegion> {
        if self.buffer.is_empty() {
            return Vec::new();
        }
        let result = decode_region_stream(&self.buffer, self.sample_rate, &self.cfg.region);
        let stability_threshold = if force_all {
            self.head_s + 1.0 // commit everything
        } else {
            self.head_s - self.cfg.stable_latency_s
        };

        let mut newly = Vec::new();
        for region in &result.regions {
            if region.start_s <= self.last_committed_end_s {
                continue; // already committed in an earlier cycle
            }
            if region.end_s > stability_threshold {
                break; // region is still active; wait for more trailing static
            }
            newly.push(CommittedRegion {
                start_s: region.start_s,
                end_s: region.end_s,
                text: region.text.clone(),
            });
            self.last_committed_end_s = region.end_s;
        }

        for region in &newly {
            if !self.committed_text.is_empty() {
                self.committed_text.push(' ');
            }
            self.committed_text.push_str(&region.text);
        }
        newly
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::TAU;

    fn synthesize_tone(sample_rate: u32, freq_hz: f32, duration_s: f32) -> Vec<f32> {
        let n = (duration_s * sample_rate as f32) as usize;
        (0..n)
            .map(|i| {
                let t = i as f32 / sample_rate as f32;
                (2.0 * std::f32::consts::PI * freq_hz * t).sin() * 0.5
            })
            .collect()
    }

    fn synthesize_silence(sample_rate: u32, duration_s: f32) -> Vec<f32> {
        vec![0.0; (duration_s * sample_rate as f32) as usize]
    }

    /// Build PARIS-style morse "E" (single dit) tone bursts at a known WPM.
    fn morse_e(sample_rate: u32, wpm: f32) -> Vec<f32> {
        let dot_s = 1.2 / wpm;
        synthesize_tone(sample_rate, 700.0, dot_s)
    }

    #[test]
    fn streamer_yields_no_regions_for_empty_buffer() {
        let mut s = RegionStreamer::new(44_100);
        s.ingest(&[]);
        assert!(s.try_commit().is_empty());
        assert_eq!(s.transcript(), "");
    }

    #[test]
    fn streamer_holds_back_active_region_until_stable() {
        let sr = 44_100;
        let mut s = RegionStreamer::new(sr);
        // 1s of leading silence + a single dit (~0.1s @ 12 WPM) — not yet
        // stable because no trailing static observed.
        let mut audio = synthesize_silence(sr, 1.0);
        audio.extend(morse_e(sr, 12.0));
        s.ingest(&audio);
        let committed = s.try_commit();
        assert!(
            committed.is_empty(),
            "active region must be held back until trailing static is seen"
        );
    }

    #[test]
    fn streamer_does_not_emit_duplicate_regions() {
        let sr = 8_000;
        let mut s = RegionStreamer::new(sr);
        // Force-commit empty buffer — should not panic and yields nothing.
        let _ = s.flush();
        let _ = s.flush();
        assert_eq!(s.transcript(), "");
    }

    #[test]
    fn streamer_reset_clears_all_state() {
        let sr = 8_000;
        let mut s = RegionStreamer::new(sr);
        s.ingest(&synthesize_silence(sr, 0.5));
        s.ingest(&morse_e(sr, 12.0));
        assert!(s.buffered_seconds() > 0.0);
        s.reset();
        assert_eq!(s.transcript(), "");
        assert_eq!(s.buffered_seconds(), 0.0);
        assert!(s.try_commit().is_empty());
    }

    #[test]
    fn streamer_trim_committed_is_no_op_when_nothing_committed() {
        let sr = 8_000;
        let mut s = RegionStreamer::new(sr);
        s.ingest(&synthesize_silence(sr, 1.0));
        let before = s.buffered_seconds();
        s.trim_committed(0.5);
        assert_eq!(s.buffered_seconds(), before);
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

    fn push_silence(out: &mut Vec<f32>, sample_rate: u32, seconds: f32, phase_samples: &mut usize) {
        let n = (seconds * sample_rate as f32).round() as usize;
        out.resize(out.len() + n, 0.0);
        *phase_samples += n;
    }

    fn push_tone(
        out: &mut Vec<f32>,
        sample_rate: u32,
        pitch_hz: f32,
        seconds: f32,
        amp: f32,
        phase_samples: &mut usize,
    ) {
        let n = (seconds * sample_rate as f32).round() as usize;
        let ramp_n = ((sample_rate as f32) * 0.004).round() as usize;
        for k in 0..n {
            let rise = if ramp_n > 0 && k < ramp_n {
                0.5 * (1.0 - ((std::f32::consts::PI * k as f32) / ramp_n as f32).cos())
            } else {
                1.0
            };
            let fall = if ramp_n > 0 && k + ramp_n > n {
                let kk = (n - k) as f32;
                0.5 * (1.0 - ((std::f32::consts::PI * kk) / ramp_n as f32).cos())
            } else {
                1.0
            };
            let t = *phase_samples as f32 / sample_rate as f32;
            out.push((TAU * pitch_hz * t).sin() * amp * rise.min(fall));
            *phase_samples += 1;
        }
    }

    fn synth_morse(sample_rate: u32, pitch_hz: f32, wpm: f32, text: &str, amp: f32) -> Vec<f32> {
        let dot_s = 1.2 / wpm;
        let mut out = Vec::new();
        let mut phase_samples = 0usize;
        let chars: Vec<char> = text.to_uppercase().chars().collect();
        for (idx, ch) in chars.iter().copied().enumerate() {
            if ch.is_whitespace() {
                push_silence(&mut out, sample_rate, dot_s * 7.0, &mut phase_samples);
                continue;
            }
            let Some(code) = morse_code(ch) else {
                continue;
            };
            for (element_idx, element) in code.chars().enumerate() {
                let units = if element == '.' { 1.0 } else { 3.0 };
                push_tone(
                    &mut out,
                    sample_rate,
                    pitch_hz,
                    dot_s * units,
                    amp,
                    &mut phase_samples,
                );
                if element_idx + 1 < code.len() {
                    push_silence(&mut out, sample_rate, dot_s, &mut phase_samples);
                }
            }
            if idx + 1 < chars.len() && !chars[idx + 1].is_whitespace() {
                push_silence(&mut out, sample_rate, dot_s * 3.0, &mut phase_samples);
            }
        }
        out
    }

    fn white_noise(len: usize, amp: f32, seed: u64) -> Vec<f32> {
        let mut state = seed;
        (0..len)
            .map(|_| {
                state = state
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                let v = ((state >> 33) as u32) as f32 / u32::MAX as f32;
                (v * 2.0 - 1.0) * amp
            })
            .collect()
    }

    fn add_white_noise(samples: &mut [f32], amp: f32, seed: u64) {
        let noise = white_noise(samples.len(), amp, seed);
        for (sample, noise) in samples.iter_mut().zip(noise) {
            *sample += noise;
        }
    }

    fn add_qsb(samples: &mut [f32], sample_rate: u32, depth: f32, rate_hz: f32, floor: f32) {
        for (i, sample) in samples.iter_mut().enumerate() {
            let t = i as f32 / sample_rate as f32;
            let fade = floor + (1.0 - floor) * (0.5 + 0.5 * (TAU * rate_hz * t).sin());
            *sample *= 1.0 - depth + depth * fade;
        }
    }

    fn add_qrm_tone(samples: &mut [f32], sample_rate: u32, freq_hz: f32, amp: f32) {
        for (i, sample) in samples.iter_mut().enumerate() {
            let t = i as f32 / sample_rate as f32;
            *sample += (TAU * freq_hz * t).sin() * amp;
        }
    }

    fn normalize_for_assertion(text: &str) -> String {
        text.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    fn region_stream_transcript(
        sample_rate: u32,
        samples: &[f32],
    ) -> (String, Vec<CommittedRegion>) {
        let mut streamer = RegionStreamer::new(sample_rate);
        let chunk = (sample_rate as f32 * 0.25).round() as usize;
        let mut committed = Vec::new();
        for frame in samples.chunks(chunk.max(1)) {
            streamer.ingest(frame);
            committed.extend(streamer.try_commit());
        }
        committed.extend(streamer.flush());
        (
            normalize_for_assertion(streamer.transcript()),
            committed
                .into_iter()
                .filter(|region| !region.text.trim().is_empty())
                .collect(),
        )
    }

    fn build_multiburst(sample_rate: u32, gaps_are_static: bool) -> (Vec<f32>, String) {
        let truth = "IHU NVCHU 7QP W7N 7QP W7N";
        let bursts = [
            ("IHU", 13.0_f32),
            ("NVCHU", 8.0),
            ("7QP W7N", 39.0),
            ("7QP W7N", 29.0),
        ];
        let mut samples = synthesize_silence(sample_rate, 0.8);
        for (idx, (text, wpm)) in bursts.iter().enumerate() {
            samples.extend(synth_morse(sample_rate, 700.0, *wpm, text, 0.55));
            if idx + 1 < bursts.len() {
                let mut gap = synthesize_silence(sample_rate, 1.2 + idx as f32 * 0.35);
                if gaps_are_static {
                    add_white_noise(&mut gap, 0.025, 0xA11CE + idx as u64);
                }
                samples.extend(gap);
            }
        }
        samples.extend(synthesize_silence(sample_rate, 1.0));
        (samples, truth.to_string())
    }

    #[test]
    fn synthetic_multiburst_clean_matches_reference_copy() {
        let sr = 12_000;
        let (samples, truth) = build_multiburst(sr, false);
        let (transcript, regions) = region_stream_transcript(sr, &samples);
        assert_eq!(transcript, truth);
        assert_eq!(
            regions.len(),
            4,
            "reference training-set-b shape should remain four committed bursts: {regions:?}"
        );
    }

    #[test]
    fn synthetic_static_gaps_do_not_create_ghost_regions() {
        let sr = 12_000;
        let (samples, truth) = build_multiburst(sr, true);
        let (transcript, regions) = region_stream_transcript(sr, &samples);
        assert_eq!(transcript, truth);
        assert_eq!(
            regions.len(),
            4,
            "quiet background static between bursts must not create extra committed text: {regions:?}"
        );
    }

    #[test]
    fn synthetic_noise_only_stream_stays_empty() {
        let sr = 12_000;
        let samples = white_noise((sr as f32 * 8.0) as usize, 0.035, 0x51A71C);
        let (transcript, regions) = region_stream_transcript(sr, &samples);
        assert_eq!(transcript, "");
        assert!(
            regions.is_empty(),
            "noise-only stream must not produce ghost regions: {regions:?}"
        );
    }

    #[test]
    fn synthetic_moderate_noise_qsb_and_qrm_still_exact_copy() {
        let sr = 12_000;
        let mut samples = synthesize_silence(sr, 0.8);
        samples.extend(synth_morse(sr, 700.0, 18.0, "CQ TEST KC7AVA 73", 0.55));
        samples.extend(synthesize_silence(sr, 1.0));

        add_white_noise(&mut samples, 0.018, 0xC0DEC0DE);
        add_qsb(&mut samples, sr, 0.65, 0.45, 0.35);
        add_qrm_tone(&mut samples, sr, 830.0, 0.045);

        let (transcript, regions) = region_stream_transcript(sr, &samples);
        assert_eq!(transcript, "CQ TEST KC7AVA 73");
        assert_eq!(
            regions.len(),
            1,
            "single synthetic exchange should commit as one region: {regions:?}"
        );
    }
}
