//! Per-region structured trace for the region-based streaming decoder.
//!
//! This is **diagnostic instrumentation** for the layer pipeline. It is
//! orthogonal to the existing transcript event stream — enabling a trace
//! sink does not change decode output. Behind a CLI flag (`--trace`) or
//! `DITDAH_TRACE_PATH` env var, the binary writes one NDJSON line per
//! committed region to a file or stderr.
//!
//! Schema (see EXPERIMENT_REPORT.md for the data dictionary):
//!
//! ```jsonc
//! {
//!   "type": "region_trace",
//!   "region": {"index": 3, "start_s": 12.34, "end_s": 18.90, "pad_s": 0.15, "merge_gap_s": 0.5},
//!   "pitch": {"hz": 704.1, "peak_ratio": 8.2, "method": "goertzel_max"},
//!   "wpm":   {"estimate": 22.5, "method": "median_short_runs", "confidence": 0.71},
//!   "threshold": {"value": -10.4, "method": "otsu_log_power", "noise_floor": -12.6, "scale": "log_power"},
//!   "intervals": [
//!     {"kind": "on", "start_s": 0.012, "dur_s": 0.041, "snr_db": 9.8,
//!      "power": 7.4e-5, "noise_p20": 5.1e-6,
//!      "score_dot": -0.20, "score_dash": -4.10, "decided": "dot"},
//!     {"kind": "off", "start_s": 0.053, "dur_s": 0.042,
//!      "score_intra": -0.10, "score_letter": -3.20, "score_word": -7.10, "decided": "intra"}
//!   ],
//!   "decoded": {"raw_morse": ".-- .-", "text": "WA", "confidence": 1.0, "method": "baseline"}
//! }
//! ```
//!
//! Many fields are best-effort and may be `null`. The most important
//! addition is per-element `snr_db` — it lets downstream tooling see
//! exactly which dits/dahs got eaten by the threshold.

use serde_json::{json, Value};

use crate::region_stream::{
    goertzel_power, percentile_sorted, tonal_prominence_ratio_for_trace, RegionStreamConfig,
};

/// Build a `region_trace` JSON payload for a single region of the
/// rolling buffer. The samples slice is the **full buffer** (so absolute
/// times match the streamer's timeline); `region_start_s` / `region_end_s`
/// are the region bounds in that buffer; `pitch_hz` is the pitch chosen by
/// the detection layer for this region; `decoded_text` is the transcript
/// produced by the baseline decoder.
///
/// All trace work is performed on a `pad_s`-padded slice mirroring the
/// real decode path, so the intervals reported here line up with the
/// frames the decoder actually saw.
#[allow(clippy::too_many_arguments)]
pub fn build_region_trace(
    samples: &[f32],
    sample_rate: u32,
    cfg: &RegionStreamConfig,
    region_index: usize,
    region_start_s: f32,
    region_end_s: f32,
    pitch_hz: f32,
    decoded_text: &str,
    decoder_method: &str,
) -> Value {
    let pad = cfg.pad_s.max(0.0);
    let sr = sample_rate.max(1) as f32;
    let s = ((region_start_s - pad).max(0.0) * sr) as usize;
    let e = (((region_end_s + pad) * sr) as usize).min(samples.len());
    let slice: &[f32] = if e > s { &samples[s..e] } else { &[] };
    let slice_offset_s = s as f32 / sr;

    let frame_len = ((cfg.frame_len_s * sr).round() as usize).max(64);
    let frame_step = ((cfg.frame_step_s * sr).round() as usize).max(8);

    // Compute Goertzel powers at the chosen pitch for the padded slice.
    let mut powers = Vec::new();
    if slice.len() >= frame_len && pitch_hz.is_finite() && pitch_hz > 0.0 {
        let mut offset = 0usize;
        while offset + frame_len <= slice.len() {
            powers.push(goertzel_power(
                &slice[offset..offset + frame_len],
                sample_rate,
                pitch_hz,
            ));
            offset += frame_step;
        }
    }

    let step_s = frame_step as f32 / sr;
    let frame_s = frame_len as f32 / sr;

    let (threshold_log, noise_floor_log, threshold_lin, noise_floor_lin, intervals_json) =
        if powers.len() >= 4 {
            let log_powers: Vec<f32> = powers.iter().map(|p| p.max(1e-12).ln()).collect();
            let mut sorted_log = log_powers.clone();
            sorted_log.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            let noise_floor_log = percentile_sorted(&sorted_log, 0.20);
            let threshold_log =
                otsu_log_power_threshold_pub(&log_powers).unwrap_or(noise_floor_log);
            let noise_floor_lin = noise_floor_log.exp();
            let threshold_lin = threshold_log.exp();

            let active: Vec<bool> = log_powers.iter().map(|p| *p >= threshold_log).collect();
            // Estimate dot duration from the on-runs so we can score
            // each interval against dot/dash/intra/letter/word targets.
            let runs = collect_runs(&active);
            let dot_s = estimate_dot_from_on_runs(&runs, step_s, frame_s);

            let intervals = build_intervals(
                &runs,
                &log_powers,
                &powers,
                step_s,
                frame_s,
                slice_offset_s,
                region_start_s,
                noise_floor_lin,
                dot_s,
            );
            (
                Some(threshold_log),
                Some(noise_floor_log),
                Some(threshold_lin),
                Some(noise_floor_lin),
                intervals,
            )
        } else {
            (None, None, None, None, Vec::new())
        };

    // WPM estimate: median of on-run durations as dot, then 1.2/dot.
    let wpm_estimate = wpm_from_intervals(&intervals_json);

    // Pitch peak ratio (signal vs broadband background).
    let peak_ratio = if !slice.is_empty() && pitch_hz.is_finite() && pitch_hz > 0.0 {
        Some(tonal_prominence_ratio_for_trace(
            slice,
            sample_rate,
            pitch_hz,
            cfg,
        ))
    } else {
        None
    };

    // Reconstruct raw morse from decided intervals (best-effort).
    let raw_morse = raw_morse_from_intervals(&intervals_json);

    json!({
        "type": "region_trace",
        "region": {
            "index": region_index,
            "start_s": round3(region_start_s),
            "end_s": round3(region_end_s),
            "pad_s": pad,
            "merge_gap_s": cfg.merge_gap_s,
            "min_region_s": cfg.min_region_s,
            "duration_s": round3((region_end_s - region_start_s).max(0.0)),
        },
        "pitch": {
            "hz": pitch_hz_or_null(pitch_hz),
            "peak_ratio": peak_ratio.map(round3_opt),
            "method": "goertzel_max",
        },
        "wpm": {
            "estimate": wpm_estimate.map(round2_opt),
            "method": "median_short_runs",
            "confidence": Value::Null,
        },
        "threshold": {
            "value": threshold_lin.map(scientific_opt),
            "value_log": threshold_log.map(round3_opt),
            "noise_floor": noise_floor_lin.map(scientific_opt),
            "noise_floor_log": noise_floor_log.map(round3_opt),
            "method": "otsu_log_power",
            "scale": "linear_with_log_companion",
        },
        "intervals": intervals_json,
        "decoded": {
            "raw_morse": raw_morse,
            "text": decoded_text,
            "confidence": 1.0,
            "method": decoder_method,
        },
    })
}

fn pitch_hz_or_null(hz: f32) -> Value {
    if hz.is_finite() && hz > 0.0 {
        json!(round1(hz))
    } else {
        Value::Null
    }
}

fn round1(x: f32) -> f32 {
    (x * 10.0).round() / 10.0
}
fn round2(x: f32) -> f32 {
    (x * 100.0).round() / 100.0
}
fn round3(x: f32) -> f32 {
    (x * 1000.0).round() / 1000.0
}
fn round2_opt(x: f32) -> f32 {
    round2(x)
}
fn round3_opt(x: f32) -> f32 {
    round3(x)
}
fn scientific_opt(x: f32) -> Value {
    if x.is_finite() {
        // Keep three significant digits.
        let formatted = format!("{x:.4e}");
        formatted.parse::<f64>().map(Value::from).unwrap_or(json!(x))
    } else {
        Value::Null
    }
}

#[derive(Debug, Clone, Copy)]
struct Run {
    active: bool,
    start_frame: usize,
    end_frame: usize,
}

impl Run {
    fn duration_s(self, step_s: f32, frame_s: f32) -> f32 {
        self.end_frame.saturating_sub(self.start_frame) as f32 * step_s + frame_s
    }
    fn start_s(self, step_s: f32) -> f32 {
        self.start_frame as f32 * step_s
    }
    fn frame_count(self) -> usize {
        self.end_frame.saturating_sub(self.start_frame) + 1
    }
}

fn collect_runs(active: &[bool]) -> Vec<Run> {
    if active.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut state = active[0];
    let mut start = 0usize;
    for (i, &on) in active.iter().enumerate().skip(1) {
        if on != state {
            out.push(Run {
                active: state,
                start_frame: start,
                end_frame: i - 1,
            });
            state = on;
            start = i;
        }
    }
    out.push(Run {
        active: state,
        start_frame: start,
        end_frame: active.len() - 1,
    });
    out
}

fn estimate_dot_from_on_runs(runs: &[Run], step_s: f32, frame_s: f32) -> Option<f32> {
    let mut on: Vec<f32> = runs
        .iter()
        .filter(|r| r.active)
        .map(|r| r.duration_s(step_s, frame_s))
        .filter(|d| (0.015..=0.450).contains(d))
        .collect();
    if on.is_empty() {
        return None;
    }
    on.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let shortest = on[0];
    let longest = *on.last().unwrap();
    let dot = if longest >= shortest * 2.2 {
        let cluster_max = shortest * 1.8;
        let short: Vec<f32> = on.iter().copied().filter(|d| *d <= cluster_max).collect();
        short[short.len() / 2]
    } else {
        let median = on[on.len() / 2];
        if median > 0.180 {
            median / 3.0
        } else {
            median
        }
    };
    Some(dot)
}

#[allow(clippy::too_many_arguments)]
fn build_intervals(
    runs: &[Run],
    log_powers: &[f32],
    powers: &[f32],
    step_s: f32,
    frame_s: f32,
    slice_offset_s: f32,
    region_start_s: f32,
    noise_floor_lin: f32,
    dot_s: Option<f32>,
) -> Vec<Value> {
    let mut out = Vec::with_capacity(runs.len());
    let dot = dot_s.unwrap_or(0.06).max(0.001);
    for run in runs {
        let dur = run.duration_s(step_s, frame_s);
        // Absolute start time in the region's audio (relative to the region's
        // start_s, not the buffer or padded slice).
        let start_abs = slice_offset_s + run.start_s(step_s);
        let start_rel = start_abs - region_start_s;
        let units = dur / dot;

        if run.active {
            // Mean linear power & log power across the on-run.
            let (mean_power, mean_log_power) = mean_power_for_run(run, powers, log_powers);
            let snr_db = if noise_floor_lin > 0.0 && mean_power > 0.0 {
                Some(10.0 * (mean_power / noise_floor_lin).log10())
            } else {
                None
            };
            let score_dot = (units - 1.0).abs();
            let score_dash = (units - 3.0).abs();
            let decided = if units < 2.0 { "dot" } else { "dash" };
            out.push(json!({
                "kind": "on",
                "start_s": round3(start_rel),
                "dur_s": round3(dur),
                "units": round3(units),
                "power": scientific_opt(mean_power),
                "log_power": round3(mean_log_power),
                "noise_p20": scientific_opt(noise_floor_lin),
                "snr_db": snr_db.map(round2),
                "score_dot": round3(-score_dot),
                "score_dash": round3(-score_dash),
                "decided": decided,
            }));
        } else {
            let score_intra = (units - 1.0).abs();
            let score_letter = (units - 3.0).abs();
            let score_word = (units - 7.0).abs();
            let decided = if units < 2.0 {
                "intra"
            } else if units < 5.0 {
                "letter"
            } else {
                "word"
            };
            out.push(json!({
                "kind": "off",
                "start_s": round3(start_rel),
                "dur_s": round3(dur),
                "units": round3(units),
                "score_intra": round3(-score_intra),
                "score_letter": round3(-score_letter),
                "score_word": round3(-score_word),
                "decided": decided,
            }));
        }
    }
    out
}

fn mean_power_for_run(run: &Run, powers: &[f32], log_powers: &[f32]) -> (f32, f32) {
    let n = run.frame_count().max(1);
    let end = (run.end_frame + 1).min(powers.len());
    if run.start_frame >= end {
        return (0.0, 0.0);
    }
    let lin: f32 = powers[run.start_frame..end].iter().copied().sum::<f32>() / n as f32;
    let lg: f32 = log_powers[run.start_frame..end].iter().copied().sum::<f32>() / n as f32;
    (lin, lg)
}

fn wpm_from_intervals(intervals: &[Value]) -> Option<f32> {
    let mut on_durs: Vec<f32> = intervals
        .iter()
        .filter(|v| v.get("kind").and_then(|k| k.as_str()) == Some("on"))
        .filter_map(|v| v.get("dur_s").and_then(|d| d.as_f64()).map(|d| d as f32))
        .collect();
    if on_durs.is_empty() {
        return None;
    }
    on_durs.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let shortest = on_durs[0];
    let longest = *on_durs.last().unwrap();
    let dot = if longest >= shortest * 2.2 {
        shortest
    } else {
        on_durs[on_durs.len() / 2]
    };
    let wpm = 1.2 / dot.max(0.001);
    if (4.0..=80.0).contains(&wpm) {
        Some(wpm)
    } else {
        None
    }
}

fn raw_morse_from_intervals(intervals: &[Value]) -> String {
    let mut out = String::new();
    for v in intervals {
        let kind = v.get("kind").and_then(|k| k.as_str()).unwrap_or("");
        let decided = v.get("decided").and_then(|k| k.as_str()).unwrap_or("");
        match (kind, decided) {
            ("on", "dot") => out.push('.'),
            ("on", "dash") => out.push('-'),
            ("off", "letter") => out.push(' '),
            ("off", "word") => out.push_str(" / "),
            _ => {}
        }
    }
    // Collapse repeated spaces.
    let mut cleaned = String::new();
    let mut last_space = false;
    for c in out.chars() {
        let is_space = c == ' ';
        if !(is_space && last_space) {
            cleaned.push(c);
        }
        last_space = is_space;
    }
    cleaned.trim().to_string()
}

/// Public re-export of the otsu log-power threshold used by the decoder.
fn otsu_log_power_threshold_pub(log_powers: &[f32]) -> Option<f32> {
    crate::region_stream::otsu_log_power_threshold_for_trace(log_powers)
}
