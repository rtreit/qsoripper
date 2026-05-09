//! Train the gap-classifier HMM via Baum-Welch on the ARRL corpus.
//!
//! Reads a manifest.jsonl (one chunk per line with `wav_path`, `wpm`),
//! extracts run-length intervals from each chunk through ditdah's DSP
//! front-end, then runs EM until convergence. Writes the trained
//! parameters to a small JSON file.
//!
//! Usage:
//!     train-hmm \
//!         --manifest <path-to-manifest.jsonl> \
//!         --corpus-root <dir-containing-chunks> \
//!         --out         <out.json> \
//!         [--max-iters 15] [--max-chunks 0] [--tol 1e-4] \
//!         [--wpm 20,25,30] [--seed-from <existing.json>]

#![allow(clippy::needless_range_loop, clippy::uninlined_format_args)]


use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{bail, Context, Result};
use clap::Parser;
use ditdah::gap_hmm::{em_e_step, em_m_step, EmAccum, HmmParams, Run, NUM_STATES};
use rayon::prelude::*;

#[derive(Parser, Debug)]
#[command(name = "train-hmm", about = "Baum-Welch trainer for ditdah gap HMM")]
struct Cli {
    /// Path to manifest.jsonl with one JSON object per line; required keys
    /// are `wav_path` and `wpm`.
    #[arg(long)]
    manifest: PathBuf,

    /// Directory the manifest's relative `wav_path` entries are resolved
    /// against. If a wav_path is absolute, this is ignored.
    #[arg(long)]
    corpus_root: PathBuf,

    /// Output JSON file for trained parameters.
    #[arg(long)]
    out: PathBuf,

    /// Max EM iterations.
    #[arg(long, default_value_t = 12)]
    max_iters: usize,

    /// Stop when |delta-LL/N| < tol between consecutive iterations.
    #[arg(long, default_value_t = 1e-4)]
    tol: f64,

    /// Limit the number of chunks loaded (0 = all).
    #[arg(long, default_value_t = 0)]
    max_chunks: usize,

    /// Comma-separated list of WPM values to keep (e.g. "20,25,30").
    /// Empty = keep all.
    #[arg(long, default_value = "20,25,30")]
    wpm: String,

    /// Seed parameters from an existing JSON file (instead of the
    /// hand-tuned default).
    #[arg(long)]
    seed_from: Option<PathBuf>,

    /// If set, dump per-state element counts after each EM iteration.
    #[arg(long)]
    verbose: bool,
}

#[derive(Debug)]
struct Chunk {
    wav: PathBuf,
    wpm: f32,
}

fn parse_manifest(path: &Path, root: &Path, wpm_filter: &BTreeSet<u32>) -> Result<Vec<Chunk>> {
    let text = std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        // Tiny field extractor: we know the manifest format.
        let wav_path = field_str(line, "wav_path").unwrap_or_default();
        let wpm = field_num(line, "wpm").unwrap_or(0.0) as f32;
        if wav_path.is_empty() {
            continue;
        }
        if !wpm_filter.is_empty() && !wpm_filter.contains(&(wpm.round() as u32)) {
            continue;
        }
        let wav_pb = Path::new(&wav_path);
        let resolved = if wav_pb.is_absolute() {
            wav_pb.to_path_buf()
        } else {
            // The manifest typically stores `data/cw-samples/arrl-archive/...`.
            // Strip any leading prefix that's already part of `root` to avoid
            // doubling. We try both join styles.
            let direct = root.join(wav_pb);
            if direct.exists() {
                direct
            } else {
                let last = wav_pb.components().rev().take(3).collect::<Vec<_>>();
                let mut suffix = PathBuf::new();
                for c in last.into_iter().rev() {
                    suffix.push(c.as_os_str());
                }
                root.join(suffix)
            }
        };
        out.push(Chunk { wav: resolved, wpm });
    }
    Ok(out)
}

fn field_str(line: &str, key: &str) -> Option<String> {
    let needle = format!("\"{key}\"");
    let i = line.find(&needle)?;
    let rest = &line[i + needle.len()..];
    let j = rest.find(':')?;
    let after = rest[j + 1..].trim_start();
    if !after.starts_with('"') {
        return None;
    }
    let q = &after[1..];
    let end = q.find('"')?;
    Some(q[..end].to_string())
}

fn field_num(line: &str, key: &str) -> Option<f64> {
    let needle = format!("\"{key}\"");
    let i = line.find(&needle)?;
    let rest = &line[i + needle.len()..];
    let j = rest.find(':')?;
    let after = rest[j + 1..].trim_start();
    let end = after
        .find(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-' || c == '+' || c == 'e'))
        .unwrap_or(after.len());
    after[..end].parse::<f64>().ok()
}

fn read_wav(path: &Path) -> Result<(Vec<f32>, u32)> {
    let mut reader =
        hound::WavReader::open(path).with_context(|| format!("open wav {}", path.display()))?;
    let spec = reader.spec();
    let samples_f32: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Int => reader
            .samples::<i16>()
            .filter_map(|s| s.ok())
            .map(|s| s as f32 / 32768.0)
            .collect(),
        hound::SampleFormat::Float => reader.samples::<f32>().filter_map(|s| s.ok()).collect(),
    };
    let mono: Vec<f32> = if spec.channels > 1 {
        samples_f32
            .chunks_exact(spec.channels as usize)
            .map(|c| c.iter().sum::<f32>() / spec.channels as f32)
            .collect()
    } else {
        samples_f32
    };
    Ok((mono, spec.sample_rate))
}

fn print_params(p: &HmmParams) {
    println!("  state    mu     sigma   log_pi");
    let names = ["Dit  ", "Dah  ", "Intra", "Char ", "Word "];
    for s in 0..NUM_STATES {
        println!(
            "  {}  {:>+6.3}  {:>5.3}  {:>+6.3}",
            names[s], p.mu[s], p.sigma[s], p.log_pi[s]
        );
    }
    println!("  log_a (rows = from, cols = to):");
    for i in 0..NUM_STATES {
        let mut row = String::new();
        for j in 0..NUM_STATES {
            if p.log_a[i][j].is_finite() {
                row.push_str(&format!(" {:>+7.3}", p.log_a[i][j]));
            } else {
                row.push_str("    -inf");
            }
        }
        println!("  {}{}", names[i], row);
    }
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();

    let wpm_filter: BTreeSet<u32> = cli
        .wpm
        .split(',')
        .filter_map(|s| s.trim().parse::<u32>().ok())
        .collect();

    let mut chunks = parse_manifest(&cli.manifest, &cli.corpus_root, &wpm_filter)?;
    if cli.max_chunks > 0 && chunks.len() > cli.max_chunks {
        chunks.truncate(cli.max_chunks);
    }
    println!(
        "Manifest: {} chunks (filter wpm={:?})",
        chunks.len(),
        wpm_filter
    );
    if chunks.is_empty() {
        bail!("no chunks after filter");
    }

    println!("Extracting run-length intervals (parallel)...");
    let t0 = Instant::now();
    let extracted: Vec<(Vec<Run>, f32)> = chunks
        .par_iter()
        .filter_map(|c| {
            let (samples, sr) = read_wav(&c.wav).ok()?;
            let r = ditdah::extract_runs_for_training(&samples, sr, Some(c.wpm)).ok()?;
            // Skip degenerate chunks (almost no detected runs).
            if r.runs.len() < 8 || r.dot_len_samples < 1.0 {
                return None;
            }
            Some((r.runs, r.dot_len_samples))
        })
        .collect();
    let total_runs: usize = extracted.iter().map(|(r, _)| r.len()).sum();
    println!(
        "  → {} usable chunks, {} total intervals ({:.1}s)",
        extracted.len(),
        total_runs,
        t0.elapsed().as_secs_f32()
    );

    let mut params = match &cli.seed_from {
        Some(p) => HmmParams::load(p).map_err(|e| anyhow::anyhow!(e))?,
        None => HmmParams::seed(),
    };
    println!("\nInitial parameters:");
    print_params(&params);

    println!("\nBaum-Welch:");
    let mut prev_ll = f64::NEG_INFINITY;
    let mut history: Vec<(usize, f64, f64)> = Vec::new();
    for it in 0..cli.max_iters {
        let mut acc = EmAccum::new();
        em_e_step(&extracted, &params, &mut acc);
        let n = acc.n_obs.max(1) as f64;
        let mean_ll = acc.log_lik / n;
        let delta = if prev_ll.is_finite() {
            (acc.log_lik - prev_ll) / n
        } else {
            f64::INFINITY
        };
        println!(
            "  iter {:>2}: seqs={} obs={} mean-LL={:.5} delta={:.6e}",
            it, acc.n_seq, acc.n_obs, mean_ll, delta
        );
        history.push((it, mean_ll, delta));
        if delta.is_finite() && delta.abs() < cli.tol && it > 0 {
            println!("  converged (|delta| < {})", cli.tol);
            params = em_m_step(&acc);
            break;
        }
        params = em_m_step(&acc);
        if cli.verbose {
            print_params(&params);
        }
        prev_ll = acc.log_lik;
    }

    println!("\nFinal parameters:");
    print_params(&params);

    if let Some(parent) = cli.out.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    params.save(&cli.out)?;
    println!("\nWrote {}", cli.out.display());

    // Append training history as a comment file alongside the params.
    let mut hist = String::from("iter,mean_ll,delta\n");
    for (it, ll, d) in &history {
        hist.push_str(&format!("{},{:.6},{:.6}\n", it, ll, d));
    }
    let hist_path = cli.out.with_extension("history.csv");
    std::fs::write(&hist_path, hist).ok();
    println!("Wrote {}", hist_path.display());
    Ok(())
}
