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
}
