//! Diagnose the QSB-failure mode on training-set-c/radio-20260504-190450.
//!
//! Goal: pin down WHERE in the region pipeline the second half of the
//! recording goes from "WA2IAC" to garbage T/E/A characters, and prototype
//! a block-wise adaptive log-power threshold + hysteresis fix.

use std::env;
use std::path::PathBuf;

use anyhow::{bail, Result};
use cw_decoder_poc::audio;
use cw_decoder_poc::decoder::decode_text;
use cw_decoder_poc::region_stream::{decode_region_stream, percentile_sorted, RegionStreamConfig};
use cw_decoder_poc::region_streamer::RegionStreamerConfig;

fn main() -> Result<()> {
    let args: Vec<String> = env::args().collect();
    if args.len() < 2 {
        bail!("usage: diagnose_qsb <wav-path>");
    }
    let path = PathBuf::from(&args[1]);
    let decoded = audio::decode_file(&path)?;
    let sr = decoded.sample_rate;
    let n = decoded.samples.len();
    let dur_s = n as f32 / sr.max(1) as f32;
    println!(
        "File: {}\n  sample_rate={} Hz  duration={:.2}s  samples={}",
        path.display(),
        sr,
        dur_s,
        n
    );

    // 1. Per-second tone-power envelope at 620 Hz so we can SEE the QSB.
    let target_hz = 620.0_f32;
    let bucket_s = 1.0_f32;
    let bucket_n = (sr as f32 * bucket_s) as usize;
    println!("\n# Tone-power envelope @ {target_hz:.0} Hz, {bucket_s:.1}s buckets");
    let mut max_p = 0.0_f32;
    let mut buckets: Vec<(f32, f32)> = Vec::new();
    let mut idx = 0usize;
    let mut t0 = 0.0_f32;
    while idx < n {
        let end = (idx + bucket_n).min(n);
        let p = goertzel_power(&decoded.samples[idx..end], sr, target_hz);
        buckets.push((t0, p));
        if p > max_p {
            max_p = p;
        }
        t0 += bucket_s;
        idx = end;
    }
    let scale = if max_p > 0.0 { 40.0 / max_p } else { 0.0 };
    for (t, p) in &buckets {
        let bar_n = ((*p) * scale) as usize;
        let bar = "#".repeat(bar_n.min(50));
        println!("  {t:>4.0}  {p:>5.3}  {bar}");
    }

    // 2. Production region decode (matches GUI streamer config).
    println!("\n# Current production decode (RegionStreamerConfig defaults)");
    let cfg = RegionStreamerConfig::default().region;
    let result = decode_region_stream(&decoded.samples, sr, &cfg);
    println!(
        "  pitch={:.1} Hz  regions={}",
        result.pitch_hz,
        result.regions.len()
    );
    for (i, r) in result.regions.iter().enumerate() {
        println!(
            "  region {:>2}: {:>6.2}-{:>6.2}s ({:>5.2}s)  text={:?}",
            i,
            r.start_s,
            r.end_s,
            r.end_s - r.start_s,
            r.text
        );
    }
    println!("  joined: {:?}", result.text);

    // 3. Direct decode of hand-picked slices, bypassing region detection.
    println!("\n# Bypass region detection: decode_text on hand-picked slices");
    for (label, t0, t1) in [
        ("WA2IAC #2 (truth: WA2IAC)", 19.0, 26.0),
        ("WA2IAC/1 AR (truth: WA2IAC/1 AR)", 26.0, 41.0),
        (
            "entire file (truth: IQ CQ CQ DE WA2IAC WA2IAC WA2IAC/1 AR)",
            0.0,
            41.0,
        ),
    ] {
        let s = (t0 * sr as f32) as usize;
        let e = ((t1 * sr as f32) as usize).min(n);
        if s >= e {
            continue;
        }
        let txt = decode_text(&decoded.samples[s..e], sr);
        println!("  [{t0:.2}-{t1:.2}s] {label} => {txt:?}");
    }

    // 4. Config sweep with the new block-adaptive log-power threshold.
    println!("\n# Adaptive-threshold config sweep (block_s, hop_s, off_factor_mul)");
    let pitch = result.pitch_hz;
    let frame_len = ((cfg.frame_len_s * sr as f32).round() as usize).max(64);
    let frame_step = ((cfg.frame_step_s * sr as f32).round() as usize).max(8);
    let powers = compute_pitch_powers(&decoded.samples, sr, pitch, frame_len, frame_step);
    let step_s = frame_step as f32 / sr as f32;
    let frame_s = frame_len as f32 / sr as f32;

    for (label, block_s, hop_s, off_factor, merge_gap) in [
        ("baseline 2.0/1.0/0.5", 2.0_f32, 1.0_f32, 0.5_f32, 0.5_f32),
        ("tight 1.5/0.5/0.7", 1.5_f32, 0.5_f32, 0.7_f32, 0.5_f32),
        ("smooth 3.0/1.0/0.5", 3.0_f32, 1.0_f32, 0.5_f32, 0.5_f32),
        (
            "aggressive-off 2.0/1.0/0.4",
            2.0_f32,
            1.0_f32,
            0.4_f32,
            0.5_f32,
        ),
        (
            "very-smooth 4.0/1.0/0.5",
            4.0_f32,
            1.0_f32,
            0.5_f32,
            0.5_f32,
        ),
        ("baseline+merge1.5", 2.0_f32, 1.0_f32, 0.5_f32, 1.5_f32),
        ("baseline+merge2.5", 2.0_f32, 1.0_f32, 0.5_f32, 2.5_f32),
    ] {
        let mut proto_cfg = cfg.clone();
        proto_cfg.pin_wpm = None;
        proto_cfg.merge_gap_s = merge_gap;
        let active =
            adaptive_active_mask_with(&powers, &proto_cfg, step_s, block_s, hop_s, off_factor);
        let runs = collect_runs(&active, step_s, frame_s);
        let merged = merge_runs(runs, proto_cfg.merge_gap_s);
        let regions: Vec<(f32, f32)> = merged
            .into_iter()
            .filter(|(s, e)| (e - s) >= proto_cfg.min_region_s)
            .collect();
        let region_strs: Vec<String> = regions
            .iter()
            .map(|(s, e)| {
                let s_idx = ((*s).max(0.0) * sr as f32) as usize;
                let e_idx = ((*e).max(0.0) * sr as f32) as usize;
                let e_idx = e_idx.min(n);
                let s_idx = s_idx.min(e_idx);
                if e_idx > s_idx {
                    decode_text(&decoded.samples[s_idx..e_idx], sr)
                } else {
                    String::new()
                }
            })
            .collect();
        let joined: Vec<String> = region_strs
            .iter()
            .filter(|t| !t.is_empty() && !t.contains('?'))
            .cloned()
            .collect();
        println!(
            "  {:32} -> {} regions, region-texts={:?}, ?-filtered: {:?}",
            label,
            regions.len(),
            region_strs,
            joined.join(" ")
        );
    }

    Ok(())
}

fn compute_pitch_powers(
    samples: &[f32],
    sr: u32,
    pitch_hz: f32,
    frame_len: usize,
    frame_step: usize,
) -> Vec<f32> {
    let mut out = Vec::new();
    let mut offset = 0usize;
    while offset + frame_len <= samples.len() {
        out.push(goertzel_power(
            &samples[offset..offset + frame_len],
            sr,
            pitch_hz,
        ));
        offset += frame_step;
    }
    out
}

fn adaptive_active_mask_with(
    powers: &[f32],
    cfg: &RegionStreamConfig,
    step_s: f32,
    block_s: f32,
    hop_s: f32,
    off_factor_mul: f32,
) -> Vec<bool> {
    if powers.len() < 4 {
        return vec![];
    }
    let log_powers: Vec<f32> = powers.iter().map(|&p| (p.max(1e-12)).ln()).collect();

    let mut sorted_global = log_powers.clone();
    sorted_global.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let global_p05 = percentile_sorted(&sorted_global, 0.05);

    let block_frames = ((block_s / step_s) as usize).max(40);
    let hop_frames = ((hop_s / step_s) as usize).max(20);
    let half = block_frames / 2;

    let n = log_powers.len();
    let min_contrast = (2.5_f32).ln();

    let mut centers: Vec<usize> = Vec::new();
    let mut on_thr: Vec<f32> = Vec::new();
    let mut off_thr: Vec<f32> = Vec::new();

    let mut center = 0usize;
    while center < n {
        let lo = center.saturating_sub(half);
        let hi = (center + half).min(n);
        let mut local: Vec<f32> = log_powers[lo..hi].to_vec();
        local.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let p35 = percentile_sorted(&local, 0.35);
        let p85 = percentile_sorted(&local, 0.85);
        let contrast = p85 - p35;
        let factor = cfg.threshold_factor.max(0.0);
        let (on_t, off_t) = if contrast >= min_contrast {
            let on_t = p35 + (p85 - p35) * factor;
            let off_t = p35 + (p85 - p35) * (factor * off_factor_mul).max(0.0);
            (on_t, off_t)
        } else {
            let on_t = global_p05 + (4.0_f32).ln();
            (on_t, on_t)
        };
        centers.push(center);
        on_thr.push(on_t);
        off_thr.push(off_t);
        center += hop_frames;
    }

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

fn collect_runs(active: &[bool], step_s: f32, frame_s: f32) -> Vec<(f32, f32)> {
    let mut runs = Vec::new();
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
    runs
}

fn merge_runs(runs: Vec<(f32, f32)>, gap: f32) -> Vec<(f32, f32)> {
    let mut merged: Vec<(f32, f32)> = Vec::new();
    for run in runs {
        if let Some(last) = merged.last_mut() {
            if run.0 - last.1 <= gap.max(0.0) {
                last.1 = run.1;
                continue;
            }
        }
        merged.push(run);
    }
    merged
}

fn goertzel_power(samples: &[f32], sample_rate: u32, target_hz: f32) -> f32 {
    if samples.is_empty() || sample_rate == 0 {
        return 0.0;
    }
    let omega = (2.0 * std::f32::consts::PI * target_hz) / sample_rate as f32;
    let coeff = 2.0 * omega.cos();
    let mut q1 = 0.0_f32;
    let mut q2 = 0.0_f32;
    for &s in samples {
        let q0 = coeff * q1 - q2 + s;
        q2 = q1;
        q1 = q0;
    }
    let p = q1 * q1 + q2 * q2 - coeff * q1 * q2;
    (p / samples.len() as f32).max(0.0)
}
