//! Walk a corpus directory of `*.truth.txt` sidecars + matching audio
//! files, run each through the region-isolated decoder, and produce a
//! per-file pass/CER/ghost summary.
//!
//! This closes the loop between the operator-curated corpus collected via
//! the Visualizer truth workflow (PR #381) and the production region
//! decoder. Without it, every corpus addition required a manual
//! `Get-Content; cargo run; diff` ritual; with it, a single command
//! validates the entire corpus and emits machine-readable results.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Serialize;

use crate::audio;
use crate::region_stream::{decode_region_stream, RegionStreamConfig};

/// Audio file extensions we attempt to pair with a `*.truth.txt` sidecar,
/// in priority order. WAV first because it's lossless and is what the
/// Visualizer Save Truth workflow emits.
const AUDIO_EXTENSIONS: &[&str] = &["wav", "mp3", "m4a", "flac"];

/// One discovered corpus entry: a `truth.txt` sidecar paired with its
/// audio file.
#[derive(Debug, Clone)]
pub struct CorpusEntry {
    /// Display id (basename without `.truth.txt`).
    pub id: String,
    /// Path to the audio file (`.wav` preferred, `.mp3` fallback, …).
    pub audio_path: PathBuf,
    /// Path to the matching `*.truth.txt` sidecar.
    pub truth_path: PathBuf,
    /// Truth text, normalized (whitespace collapsed, no leading/trailing
    /// whitespace).
    pub truth: String,
}

/// Outcome status for one entry. `Skipped` covers cases where we cannot
/// run the decoder (missing audio, unreadable file, etc.); the corpus
/// summary distinguishes skips from real validation failures.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "kind", content = "reason")]
pub enum CorpusValidationStatus {
    Validated,
    Skipped(String),
    Errored(String),
}

/// Per-entry validation result.
#[derive(Debug, Clone, Serialize)]
pub struct CorpusValidation {
    pub id: String,
    pub audio_path: PathBuf,
    pub truth_path: PathBuf,
    pub status: CorpusValidationStatus,
    /// Truth text used for comparison (normalized).
    pub truth: String,
    /// Decoded transcript (normalized).
    pub transcript: String,
    /// Whether the decoded transcript exactly matches the truth.
    pub exact_match: bool,
    /// Levenshtein-based character error rate. `1.0` for total mismatch
    /// and `0.0` for exact match. `None` when the entry was skipped.
    pub char_error_rate: Option<f32>,
    /// Number of decoded characters that don't appear in the truth in
    /// order — a coarse "ghost text" signal.
    pub ghost_chars: usize,
    /// Number of truth characters missing from the decoded transcript.
    pub missing_chars: usize,
    /// Number of decoded regions emitted by the region path.
    pub region_count: usize,
    /// Audio duration in seconds.
    pub duration_s: f32,
}

/// Aggregate summary across an entire corpus run.
#[derive(Debug, Clone, Serialize)]
pub struct CorpusSummary {
    pub total: usize,
    pub validated: usize,
    pub exact_matches: usize,
    pub mismatches: usize,
    pub skipped: usize,
    pub errored: usize,
    pub mean_cer: Option<f32>,
    pub total_ghost_chars: usize,
    pub total_missing_chars: usize,
}

/// Walk `dir` for `*.truth.txt` files, pair each with its preferred
/// audio file, and return the resulting corpus entries.
///
/// `recursive` controls whether sub-directories are descended.
///
/// Entries with no readable matching audio file are still returned with
/// `audio_path` pointing at the highest-priority candidate path that
/// would have been tried — `validate_entry` will report them as
/// `Skipped`.
pub fn discover_corpus(dir: &Path, recursive: bool) -> Result<Vec<CorpusEntry>> {
    let root = dir.to_path_buf();
    let mut out = Vec::new();
    let mut stack: Vec<PathBuf> = vec![root.clone()];
    while let Some(d) = stack.pop() {
        let read = std::fs::read_dir(&d)
            .with_context(|| format!("reading corpus directory {}", d.display()))?;
        for entry in read.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if recursive {
                    stack.push(path);
                }
                continue;
            }
            if !is_truth_sidecar(&path) {
                continue;
            }
            let id = relative_id(&root, &path);
            let truth = read_truth(&path)?;
            let audio_path = find_audio_for(&path);
            out.push(CorpusEntry {
                id,
                audio_path,
                truth_path: path,
                truth,
            });
        }
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(out)
}

/// Decode `entry.audio_path` through the region-isolated decoder using
/// `cfg`, then compare against `entry.truth`.
pub fn validate_entry(entry: &CorpusEntry, cfg: &RegionStreamConfig) -> CorpusValidation {
    if !entry.audio_path.exists() {
        return skipped(entry, "audio file missing");
    }

    let decoded = match audio::decode_file(&entry.audio_path) {
        Ok(audio) => audio,
        Err(err) => return errored(entry, &format!("decode_file failed: {err}")),
    };
    let duration_s = if decoded.sample_rate > 0 {
        decoded.samples.len() as f32 / decoded.sample_rate as f32
    } else {
        0.0
    };

    let region = decode_region_stream(&decoded.samples, decoded.sample_rate, cfg);
    let transcript = normalize_transcript(&region.text);
    let truth = normalize_transcript(&entry.truth);
    let exact_match = transcript == truth;
    let cer = char_error_rate(&truth, &transcript);
    let (ghost_chars, missing_chars) = ghost_and_missing_chars(&truth, &transcript);

    CorpusValidation {
        id: entry.id.clone(),
        audio_path: entry.audio_path.clone(),
        truth_path: entry.truth_path.clone(),
        status: CorpusValidationStatus::Validated,
        truth,
        transcript,
        exact_match,
        char_error_rate: Some(cer),
        ghost_chars,
        missing_chars,
        region_count: region.regions.len(),
        duration_s,
    }
}

/// Compute the aggregate summary across a set of per-entry results.
pub fn summarize(validations: &[CorpusValidation]) -> CorpusSummary {
    let mut total_cer = 0.0_f32;
    let mut cer_count = 0usize;
    let mut exact_matches = 0;
    let mut mismatches = 0;
    let mut skipped = 0;
    let mut errored = 0;
    let mut total_ghost = 0;
    let mut total_missing = 0;
    for v in validations {
        match &v.status {
            CorpusValidationStatus::Validated => {
                if let Some(cer) = v.char_error_rate {
                    total_cer += cer;
                    cer_count += 1;
                }
                if v.exact_match {
                    exact_matches += 1;
                } else {
                    mismatches += 1;
                }
                total_ghost += v.ghost_chars;
                total_missing += v.missing_chars;
            }
            CorpusValidationStatus::Skipped(_) => skipped += 1,
            CorpusValidationStatus::Errored(_) => errored += 1,
        }
    }
    let mean_cer = if cer_count > 0 {
        Some(total_cer / cer_count as f32)
    } else {
        None
    };
    CorpusSummary {
        total: validations.len(),
        validated: exact_matches + mismatches,
        exact_matches,
        mismatches,
        skipped,
        errored,
        mean_cer,
        total_ghost_chars: total_ghost,
        total_missing_chars: total_missing,
    }
}

fn is_truth_sidecar(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
        return false;
    };
    name.to_ascii_lowercase().ends_with(".truth.txt")
}

fn truth_basename(path: &Path) -> String {
    let name = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or_default();
    let lower = name.to_ascii_lowercase();
    if let Some(stripped) = lower.strip_suffix(".truth.txt") {
        return name[..stripped.len()].to_string();
    }
    name.to_string()
}

/// Build a stable, human-readable id for a discovered truth sidecar by
/// joining its parent directory (relative to the discovery root) with
/// the truth basename. Always uses `/` separators so output is stable
/// across Windows and Linux.
fn relative_id(root: &Path, truth_path: &Path) -> String {
    let basename = truth_basename(truth_path);
    let parent = truth_path.parent();
    let rel = parent.and_then(|p| p.strip_prefix(root).ok());
    match rel {
        Some(rel) if !rel.as_os_str().is_empty() => {
            let parts: Vec<String> = rel
                .components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect();
            if parts.is_empty() {
                basename
            } else {
                format!("{}/{}", parts.join("/"), basename)
            }
        }
        _ => basename,
    }
}

fn read_truth(path: &Path) -> Result<String> {
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("reading truth sidecar {}", path.display()))?;
    Ok(raw.trim().to_string())
}

fn find_audio_for(truth_path: &Path) -> PathBuf {
    let parent = truth_path.parent().unwrap_or(Path::new("."));
    let basename = truth_basename(truth_path);
    // Case-insensitive directory scan so a corpus with `FOO.WAV` or
    // mixed-case extensions works on Linux (filesystem is case-sensitive
    // there) the same way it works on Windows.
    let basename_lower = basename.to_ascii_lowercase();
    if let Ok(read) = std::fs::read_dir(parent) {
        let mut by_ext: std::collections::HashMap<String, PathBuf> =
            std::collections::HashMap::new();
        for entry in read.flatten() {
            let path = entry.path();
            let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
                continue;
            };
            let name_lower = name.to_ascii_lowercase();
            // Skip the truth sidecar itself (its full name ends in
            // `.truth.txt`, which would otherwise stem-match basename).
            if name_lower.ends_with(".truth.txt") {
                continue;
            }
            let Some(dot) = name_lower.rfind('.') else {
                continue;
            };
            let stem_lower = &name_lower[..dot];
            let ext = &name_lower[dot + 1..];
            if stem_lower != basename_lower {
                continue;
            }
            if AUDIO_EXTENSIONS.contains(&ext) {
                by_ext.entry(ext.to_string()).or_insert(path);
            }
        }
        for ext in AUDIO_EXTENSIONS {
            if let Some(p) = by_ext.remove(*ext) {
                return p;
            }
        }
    }
    // Fall back to the WAV-flavored candidate path so the skip message
    // points at the canonical missing file.
    parent.join(format!("{basename}.wav"))
}

fn skipped(entry: &CorpusEntry, reason: &str) -> CorpusValidation {
    CorpusValidation {
        id: entry.id.clone(),
        audio_path: entry.audio_path.clone(),
        truth_path: entry.truth_path.clone(),
        status: CorpusValidationStatus::Skipped(reason.to_string()),
        truth: normalize_transcript(&entry.truth),
        transcript: String::new(),
        exact_match: false,
        char_error_rate: None,
        ghost_chars: 0,
        missing_chars: 0,
        region_count: 0,
        duration_s: 0.0,
    }
}

fn errored(entry: &CorpusEntry, reason: &str) -> CorpusValidation {
    CorpusValidation {
        id: entry.id.clone(),
        audio_path: entry.audio_path.clone(),
        truth_path: entry.truth_path.clone(),
        status: CorpusValidationStatus::Errored(reason.to_string()),
        truth: normalize_transcript(&entry.truth),
        transcript: String::new(),
        exact_match: false,
        char_error_rate: None,
        ghost_chars: 0,
        missing_chars: 0,
        region_count: 0,
        duration_s: 0.0,
    }
}

fn normalize_transcript(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn char_error_rate(reference: &str, hypothesis: &str) -> f32 {
    let r: Vec<char> = reference.chars().collect();
    let h: Vec<char> = hypothesis.chars().collect();
    if r.is_empty() {
        return if h.is_empty() { 0.0 } else { 1.0 };
    }
    let mut prev: Vec<usize> = (0..=h.len()).collect();
    let mut cur = vec![0usize; h.len() + 1];
    for (i, rc) in r.iter().enumerate() {
        cur[0] = i + 1;
        for (j, hc) in h.iter().enumerate() {
            let cost = if rc == hc { 0 } else { 1 };
            cur[j + 1] = (prev[j + 1] + 1).min(cur[j] + 1).min(prev[j] + cost);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[h.len()] as f32 / r.len() as f32
}

/// Decompose the diff between truth and decoded transcript into "ghost
/// chars" (chars in decode beyond what truth contains) and "missing
/// chars" (chars in truth that decode dropped), as a multiset count
/// difference. Whitespace is ignored.
///
/// For each non-whitespace character `c`:
///   `ghost   += max(0, count_in_decoded(c) - count_in_truth(c))`
///   `missing += max(0, count_in_truth(c)   - count_in_decoded(c))`
///
/// This is a coarse, position-insensitive signal that complements the
/// precise CER number — useful for surfacing "the decoder invented N
/// characters" / "the decoder dropped N characters" without being
/// distorted by a swap of identical characters or pure reordering.
fn ghost_and_missing_chars(truth: &str, decoded: &str) -> (usize, usize) {
    use std::collections::HashMap;
    fn counts(s: &str) -> HashMap<char, usize> {
        let mut m = HashMap::new();
        for c in s.chars().filter(|c| !c.is_whitespace()) {
            *m.entry(c).or_insert(0) += 1;
        }
        m
    }
    let truth_counts = counts(truth);
    let decoded_counts = counts(decoded);
    let mut ghost = 0usize;
    let mut missing = 0usize;
    for (c, dn) in &decoded_counts {
        let tn = *truth_counts.get(c).unwrap_or(&0);
        if *dn > tn {
            ghost += dn - tn;
        }
    }
    for (c, tn) in &truth_counts {
        let dn = *decoded_counts.get(c).unwrap_or(&0);
        if *tn > dn {
            missing += tn - dn;
        }
    }
    (ghost, missing)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_truth(dir: &Path, name: &str, text: &str) -> PathBuf {
        let path = dir.join(format!("{name}.truth.txt"));
        let mut f = std::fs::File::create(&path).expect("create truth");
        f.write_all(text.as_bytes()).expect("write truth");
        path
    }

    #[test]
    fn discover_corpus_walks_truth_sidecars() {
        let tmp = tempdir();
        let _t1 = write_truth(tmp.path(), "alpha", "ALPHA TEST");
        let _t2 = write_truth(tmp.path(), "bravo", "BRAVO TEST");
        // Non-truth file should be ignored.
        std::fs::write(tmp.path().join("readme.txt"), b"not truth").unwrap();
        let entries = discover_corpus(tmp.path(), false).expect("discover");
        let ids: Vec<&str> = entries.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids, vec!["alpha", "bravo"]);
        assert_eq!(entries[0].truth, "ALPHA TEST");
    }

    #[test]
    fn discover_corpus_recurses_when_requested() {
        let tmp = tempdir();
        let sub = tmp.path().join("subdir");
        std::fs::create_dir(&sub).unwrap();
        let _ = write_truth(tmp.path(), "top", "TOP");
        let _ = write_truth(&sub, "deep", "DEEP");

        let flat = discover_corpus(tmp.path(), false).expect("discover");
        assert_eq!(
            flat.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(),
            vec!["top"]
        );

        let recursive = discover_corpus(tmp.path(), true).expect("discover");
        let ids: Vec<&str> = recursive.iter().map(|e| e.id.as_str()).collect();
        assert!(ids.contains(&"top"));
        // Subdirectory entries are namespaced with their relative path so
        // duplicate basenames across folders don't collide on a single id.
        assert!(ids.contains(&"subdir/deep"));
    }

    #[test]
    fn discover_corpus_disambiguates_duplicate_basenames() {
        let tmp = tempdir();
        let sub_a = tmp.path().join("set-a");
        let sub_b = tmp.path().join("set-b");
        std::fs::create_dir(&sub_a).unwrap();
        std::fs::create_dir(&sub_b).unwrap();
        let _ = write_truth(&sub_a, "shared", "FROM A");
        let _ = write_truth(&sub_b, "shared", "FROM B");

        let entries = discover_corpus(tmp.path(), true).expect("discover");
        let ids: Vec<&str> = entries.iter().map(|e| e.id.as_str()).collect();
        assert!(ids.contains(&"set-a/shared"));
        assert!(ids.contains(&"set-b/shared"));
        assert_eq!(ids.len(), 2);
    }

    #[test]
    fn validate_entry_with_missing_audio_is_skipped() {
        let tmp = tempdir();
        let truth_path = write_truth(tmp.path(), "ghost", "MISSING");
        let entries = discover_corpus(tmp.path(), false).expect("discover");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].truth_path, truth_path);
        let v = validate_entry(&entries[0], &RegionStreamConfig::default());
        assert!(matches!(v.status, CorpusValidationStatus::Skipped(_)));
        assert!(!v.exact_match);
        assert_eq!(v.char_error_rate, None);
    }

    #[test]
    fn char_error_rate_handles_exact_and_total_mismatch() {
        assert_eq!(char_error_rate("HELLO", "HELLO"), 0.0);
        assert!((char_error_rate("HELLO", "WORLD") - 0.8).abs() < 1e-6);
        assert_eq!(char_error_rate("", ""), 0.0);
        assert_eq!(char_error_rate("", "EXTRA"), 1.0);
    }

    #[test]
    fn ghost_and_missing_chars_basic() {
        let (g, m) = ghost_and_missing_chars("ABC", "ABCXYZ");
        assert_eq!(g, 3);
        assert_eq!(m, 0);
        let (g2, m2) = ghost_and_missing_chars("ABCDE", "AC");
        assert_eq!(g2, 0);
        assert_eq!(m2, 3);
    }

    #[test]
    fn ghost_and_missing_chars_uses_multiset_counts() {
        // Repeated character in truth must not be silently masked just
        // because the same character appears once in the decoded text.
        // Multiset semantics catch under-counts and over-counts that the
        // original `HashSet`-based diagnostic would have hidden.
        let (g_under, m_under) = ghost_and_missing_chars("AAA", "A");
        assert_eq!(
            (g_under, m_under),
            (0, 2),
            "decoder dropped two A's; missing must be 2"
        );

        let (g_over, m_over) = ghost_and_missing_chars("A", "AAA");
        assert_eq!(
            (g_over, m_over),
            (2, 0),
            "decoder invented two extra A's; ghost must be 2"
        );

        // Pure reordering is not noise: same multiset = no ghost / missing.
        let (g_perm, m_perm) = ghost_and_missing_chars("ABC", "CBA");
        assert_eq!((g_perm, m_perm), (0, 0));

        // Whitespace is ignored on both sides.
        let (g_ws, m_ws) = ghost_and_missing_chars("HELLO WORLD", "HELLOWORLD");
        assert_eq!((g_ws, m_ws), (0, 0));
    }

    #[test]
    fn summarize_aggregates_cer_and_status_counts() {
        let mk_validated = |id: &str, exact: bool, cer: f32, ghost: usize| CorpusValidation {
            id: id.to_string(),
            audio_path: PathBuf::from(format!("{id}.wav")),
            truth_path: PathBuf::from(format!("{id}.truth.txt")),
            status: CorpusValidationStatus::Validated,
            truth: "REF".to_string(),
            transcript: "DEC".to_string(),
            exact_match: exact,
            char_error_rate: Some(cer),
            ghost_chars: ghost,
            missing_chars: 0,
            region_count: 1,
            duration_s: 1.0,
        };
        let mk_skip = |id: &str| CorpusValidation {
            id: id.to_string(),
            audio_path: PathBuf::from(format!("{id}.wav")),
            truth_path: PathBuf::from(format!("{id}.truth.txt")),
            status: CorpusValidationStatus::Skipped("missing".into()),
            truth: "REF".to_string(),
            transcript: String::new(),
            exact_match: false,
            char_error_rate: None,
            ghost_chars: 0,
            missing_chars: 0,
            region_count: 0,
            duration_s: 0.0,
        };
        let validations = vec![
            mk_validated("a", true, 0.0, 0),
            mk_validated("b", false, 0.5, 4),
            mk_skip("c"),
        ];
        let s = summarize(&validations);
        assert_eq!(s.total, 3);
        assert_eq!(s.validated, 2);
        assert_eq!(s.exact_matches, 1);
        assert_eq!(s.mismatches, 1);
        assert_eq!(s.skipped, 1);
        assert_eq!(s.total_ghost_chars, 4);
        assert!((s.mean_cer.unwrap() - 0.25).abs() < 1e-6);
    }

    /// Tiny in-test scratch dir helper. Avoids pulling in `tempfile`.
    fn tempdir() -> ScratchDir {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let base = std::env::temp_dir();
        let id = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let counter = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = base.join(format!("cw-corpus-{id}-{}-{counter}", std::process::id()));
        std::fs::create_dir_all(&path).expect("create scratch dir");
        ScratchDir { path }
    }

    struct ScratchDir {
        path: PathBuf,
    }

    impl ScratchDir {
        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for ScratchDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }
}
