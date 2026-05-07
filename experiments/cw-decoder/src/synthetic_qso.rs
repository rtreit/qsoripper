//! Synthetic QSO generator for decoder debugging.
//!
//! Produces deterministic ragchew and contest-style exchanges with realistic
//! turn-taking, static gaps, QSB fades, and light in-band QRM. The generated
//! audio is intentionally "radio-like" but controlled enough to use as a
//! repeatable validation corpus for the region-isolated decoder baseline.

use std::f32::consts::TAU;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Serialize;

use crate::region_stream::decode_region_stream;
use crate::region_streamer::RegionStreamerConfig;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SyntheticQsoKind {
    Ragchew,
    Contest,
}

impl SyntheticQsoKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ragchew => "ragchew",
            Self::Contest => "contest",
        }
    }
}

#[derive(Debug, Clone)]
pub struct SyntheticQsoExample {
    pub id: String,
    pub kind: SyntheticQsoKind,
    pub sample_rate: u32,
    pub samples: Vec<f32>,
    pub truth: String,
    pub bursts: Vec<SyntheticQsoBurst>,
    pub impairments: SyntheticQsoImpairments,
}

#[derive(Debug, Clone, Serialize)]
pub struct SyntheticQsoBurst {
    pub text: String,
    pub wpm: f32,
    pub pitch_hz: f32,
    pub amp: f32,
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct SyntheticQsoImpairments {
    pub static_amp: f32,
    pub qsb_depth: f32,
    pub qsb_rate_hz: f32,
    pub qrm_hz: Option<f32>,
    pub qrm_amp: f32,
}

#[derive(Debug, Clone, Serialize)]
pub struct SyntheticQsoValidation {
    pub id: String,
    pub kind: SyntheticQsoKind,
    pub duration_s: f32,
    pub truth: String,
    pub transcript: String,
    pub exact_match: bool,
    pub region_count: usize,
    pub wav_path: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SyntheticQsoManifest {
    pub id: String,
    pub kind: SyntheticQsoKind,
    pub sample_rate: u32,
    pub duration_s: f32,
    pub truth: String,
    pub bursts: Vec<SyntheticQsoBurst>,
    pub impairments: SyntheticQsoImpairments,
    pub validation: SyntheticQsoValidation,
    pub wav_path: Option<PathBuf>,
    pub truth_path: Option<PathBuf>,
}

pub fn generate_qso_suite(
    sample_rate: u32,
    ragchew_count: usize,
    contest_count: usize,
) -> Vec<SyntheticQsoExample> {
    let mut examples = Vec::with_capacity(ragchew_count + contest_count);
    for i in 0..ragchew_count {
        examples.push(generate_ragchew(i, sample_rate));
    }
    for i in 0..contest_count {
        examples.push(generate_contest(i, sample_rate));
    }
    examples
}

pub fn validate_example(
    example: &SyntheticQsoExample,
    _decode_every_ms: u64,
) -> SyntheticQsoValidation {
    let cfg = RegionStreamerConfig::default();
    let decoded = decode_region_stream(&example.samples, example.sample_rate, &cfg.region);
    let transcript = normalize_transcript(&decoded.text);
    let truth = normalize_transcript(&example.truth);
    SyntheticQsoValidation {
        id: example.id.clone(),
        kind: example.kind,
        duration_s: example.samples.len() as f32 / example.sample_rate as f32,
        exact_match: transcript == truth,
        region_count: decoded.regions.len(),
        wav_path: None,
        truth,
        transcript,
    }
}

pub fn write_example_files(
    example: &SyntheticQsoExample,
    validation: &SyntheticQsoValidation,
    output_dir: &Path,
) -> Result<SyntheticQsoManifest> {
    std::fs::create_dir_all(output_dir)
        .with_context(|| format!("creating output directory {}", output_dir.display()))?;
    let wav_path = output_dir.join(format!("{}.wav", example.id));
    let truth_path = output_dir.join(format!("{}.truth.txt", example.id));
    write_wav(&wav_path, example.sample_rate, &example.samples)?;
    std::fs::write(&truth_path, example.truth.as_bytes())
        .with_context(|| format!("writing truth {}", truth_path.display()))?;

    let mut validation = validation.clone();
    validation.wav_path = Some(wav_path.clone());
    Ok(SyntheticQsoManifest {
        id: example.id.clone(),
        kind: example.kind,
        sample_rate: example.sample_rate,
        duration_s: example.samples.len() as f32 / example.sample_rate as f32,
        truth: example.truth.clone(),
        bursts: example.bursts.clone(),
        impairments: example.impairments,
        validation,
        wav_path: Some(wav_path),
        truth_path: Some(truth_path),
    })
}

pub fn write_manifest(output_dir: &Path, manifests: &[SyntheticQsoManifest]) -> Result<PathBuf> {
    let path = output_dir.join("manifest.ndjson");
    let mut lines = String::new();
    for manifest in manifests {
        lines.push_str(&serde_json::to_string(manifest).context("serializing manifest row")?);
        lines.push('\n');
    }
    std::fs::write(&path, lines).with_context(|| format!("writing manifest {}", path.display()))?;
    Ok(path)
}

fn generate_ragchew(index: usize, sample_rate: u32) -> SyntheticQsoExample {
    let scripts: &[&[&str]] = &[
        &[
            "CQ CQ DE W7N W7N K",
            "W7N DE KC7AVA GM RANDY WA RST 579 QTH SEATTLE K",
            "KC7AVA DE W7N FB RANDY NAME LEE QTH PORTLAND RIG K3 ANT DIPOLE K",
            "W7N DE KC7AVA FB LEE RIG TS590 ANT VERTICAL WX RAIN 73 SK",
        ],
        &[
            "CQ CQ CQ DE K1ABC K1ABC K",
            "K1ABC DE KC7AVA GA OM RST 589 NAME RANDY QTH WA K",
            "KC7AVA DE K1ABC RR RANDY NAME TOM QTH MA POWER 100W K",
            "K1ABC DE KC7AVA FB TOM SOLID COPY TNX QSO 73 SK",
        ],
        &[
            "CQ DE N0CALL N0CALL K",
            "N0CALL DE KC7AVA GE UR 569 IN WA NAME RANDY K",
            "KC7AVA DE N0CALL RR RANDY NAME SUE QTH CO RIG IC7300 K",
            "N0CALL DE KC7AVA FB SUE QSB BUT COPY OK 73 SK",
        ],
        &[
            "CQ CQ DE VE3XYZ VE3XYZ K",
            "VE3XYZ DE KC7AVA GM UR 599 WA NAME RANDY K",
            "KC7AVA DE VE3XYZ TNX RANDY NAME AL QTH ON ANT LOOP K",
            "VE3XYZ DE KC7AVA FB AL NICE SIG 73 SK",
        ],
        &[
            "CQ CQ DE W5DX W5DX K",
            "W5DX DE KC7AVA GA RST 579 NAME RANDY QTH WA K",
            "KC7AVA DE W5DX RR NAME JO QTH TX TEMP 72 K",
            "W5DX DE KC7AVA FB JO HR RAIN AND WIND TNX 73 SK",
        ],
        &[
            "CQ DE K7RAD K7RAD K",
            "K7RAD DE KC7AVA GM RST 589 NAME RANDY WA K",
            "KC7AVA DE K7RAD RR RANDY NAME MIKE QTH AZ RIG 7300 K",
            "K7RAD DE KC7AVA FB MIKE GREAT COPY 73 SK",
        ],
    ];
    let script = scripts[index % scripts.len()];
    let impairments = SyntheticQsoImpairments {
        static_amp: 0.014 + (index % 3) as f32 * 0.004,
        qsb_depth: 0.18 + (index % 4) as f32 * 0.06,
        qsb_rate_hz: 0.18 + (index % 5) as f32 * 0.05,
        qrm_hz: (index % 2 == 0).then_some(820.0 + index as f32 * 15.0),
        qrm_amp: if index % 2 == 0 { 0.020 } else { 0.0 },
    };
    let bursts = script
        .iter()
        .enumerate()
        .map(|(turn, text)| SyntheticQsoBurst {
            text: (*text).to_string(),
            wpm: 19.0 + ((index + turn) % 4) as f32,
            pitch_hz: 640.0 + ((index + turn) % 5) as f32 * 35.0,
            amp: 0.52 - (turn % 2) as f32 * 0.04,
        })
        .collect::<Vec<_>>();
    build_example(
        format!("ragchew-{:02}", index + 1),
        SyntheticQsoKind::Ragchew,
        sample_rate,
        bursts,
        impairments,
        0xA11C_E000 + index as u64,
    )
}

fn generate_contest(index: usize, sample_rate: u32) -> SyntheticQsoExample {
    let scripts: &[&[&str]] = &[
        &[
            "CQ TEST W7N W7N",
            "KC7AVA",
            "KC7AVA 5NN WA",
            "W7N 5NN OR TU",
        ],
        &[
            "CQ FD K1ABC K1ABC",
            "KC7AVA",
            "KC7AVA 5NN 1D WA",
            "K1ABC 5NN 2A EMA TU",
        ],
        &["TEST N0CALL", "KC7AVA", "KC7AVA 5NN CO", "N0CALL 5NN WA TU"],
        &[
            "CQ WPX VE3XYZ",
            "KC7AVA",
            "KC7AVA 5NN 104",
            "VE3XYZ 5NN 052 TU",
        ],
        &[
            "CQ NA W5DX W5DX",
            "KC7AVA",
            "KC7AVA 5NN TX",
            "W5DX 5NN WA TU",
        ],
        &[
            "CQ TEST K7RAD",
            "KC7AVA",
            "KC7AVA 5NN AZ",
            "K7RAD 5NN WA TU",
        ],
    ];
    let script = scripts[index % scripts.len()];
    let impairments = SyntheticQsoImpairments {
        static_amp: 0.018 + (index % 4) as f32 * 0.003,
        qsb_depth: 0.12 + (index % 3) as f32 * 0.05,
        qsb_rate_hz: 0.28 + (index % 4) as f32 * 0.07,
        qrm_hz: (index % 3 != 1).then_some(560.0 + index as f32 * 40.0),
        qrm_amp: if index % 3 != 1 { 0.018 } else { 0.0 },
    };
    let bursts = script
        .iter()
        .enumerate()
        .map(|(turn, text)| SyntheticQsoBurst {
            text: (*text).to_string(),
            wpm: 24.0 + ((index + turn) % 5) as f32,
            pitch_hz: 660.0 + ((index + turn) % 6) as f32 * 30.0,
            amp: 0.54 - (turn % 2) as f32 * 0.03,
        })
        .collect::<Vec<_>>();
    build_example(
        format!("contest-{:02}", index + 1),
        SyntheticQsoKind::Contest,
        sample_rate,
        bursts,
        impairments,
        0xC047_E570 + index as u64,
    )
}

fn build_example(
    id: String,
    kind: SyntheticQsoKind,
    sample_rate: u32,
    bursts: Vec<SyntheticQsoBurst>,
    impairments: SyntheticQsoImpairments,
    seed: u64,
) -> SyntheticQsoExample {
    let mut rng = Lcg::new(seed);
    let mut samples = static_noise(sample_rate, 0.65, impairments.static_amp, &mut rng);
    let mut truth_parts = Vec::with_capacity(bursts.len());
    for (idx, burst) in bursts.iter().enumerate() {
        let mut audio = synth_morse(sample_rate, burst, &mut rng);
        apply_qsb(
            &mut audio,
            sample_rate,
            impairments.qsb_depth,
            impairments.qsb_rate_hz,
            0.42,
            idx as f32 * 0.7,
        );
        samples.extend(audio);
        truth_parts.push(canonical_text(&burst.text));
        if idx + 1 < bursts.len() {
            let gap_s = 0.82 + (idx % 3) as f32 * 0.18 + rng.next_unit() * 0.08;
            samples.extend(static_noise(
                sample_rate,
                gap_s,
                impairments.static_amp,
                &mut rng,
            ));
        }
    }
    samples.extend(static_noise(
        sample_rate,
        0.8,
        impairments.static_amp,
        &mut rng,
    ));
    if let Some(qrm_hz) = impairments.qrm_hz {
        add_qrm_tone(&mut samples, sample_rate, qrm_hz, impairments.qrm_amp);
    }
    clamp_samples(&mut samples);
    SyntheticQsoExample {
        id,
        kind,
        sample_rate,
        samples,
        truth: truth_parts.join(" "),
        bursts,
        impairments,
    }
}

fn synth_morse(sample_rate: u32, burst: &SyntheticQsoBurst, rng: &mut Lcg) -> Vec<f32> {
    let dot_s = 1.2 / burst.wpm;
    let mut out = Vec::new();
    let mut phase_sample = 0usize;
    let canonical = canonical_text(&burst.text);
    let mut words = canonical.split_whitespace().peekable();
    while let Some(word) = words.next() {
        let mut chars = word.chars().peekable();
        while let Some(ch) = chars.next() {
            let Some(code) = morse_for_char(ch) else {
                continue;
            };
            let mut elements = code.chars().peekable();
            while let Some(element) = elements.next() {
                let units = if element == '.' { 1.0 } else { 3.0 };
                let duration = dot_s * units * timing_jitter(rng, 0.035);
                push_tone(
                    &mut out,
                    sample_rate,
                    burst.pitch_hz,
                    duration,
                    burst.amp,
                    &mut phase_sample,
                );
                if elements.peek().is_some() {
                    push_silence(
                        &mut out,
                        sample_rate,
                        dot_s * timing_jitter(rng, 0.03),
                        &mut phase_sample,
                    );
                }
            }
            if chars.peek().is_some() {
                push_silence(
                    &mut out,
                    sample_rate,
                    dot_s * 3.0 * timing_jitter(rng, 0.03),
                    &mut phase_sample,
                );
            }
        }
        if words.peek().is_some() {
            push_silence(
                &mut out,
                sample_rate,
                dot_s * 7.0 * timing_jitter(rng, 0.025),
                &mut phase_sample,
            );
        }
    }
    out
}

fn push_tone(
    out: &mut Vec<f32>,
    sample_rate: u32,
    pitch_hz: f32,
    seconds: f32,
    amp: f32,
    phase_sample: &mut usize,
) {
    let n = (seconds * sample_rate as f32).round().max(1.0) as usize;
    let ramp_n = ((sample_rate as f32) * 0.004).round() as usize;
    for i in 0..n {
        let rise = if ramp_n > 0 && i < ramp_n {
            0.5 * (1.0 - ((std::f32::consts::PI * i as f32) / ramp_n as f32).cos())
        } else {
            1.0
        };
        let fall = if ramp_n > 0 && i + ramp_n > n {
            let remaining = (n - i) as f32;
            0.5 * (1.0 - ((std::f32::consts::PI * remaining) / ramp_n as f32).cos())
        } else {
            1.0
        };
        let t = *phase_sample as f32 / sample_rate as f32;
        out.push((TAU * pitch_hz * t).sin() * amp * rise.min(fall));
        *phase_sample += 1;
    }
}

fn push_silence(out: &mut Vec<f32>, sample_rate: u32, seconds: f32, phase_sample: &mut usize) {
    let n = (seconds * sample_rate as f32).round().max(0.0) as usize;
    out.resize(out.len() + n, 0.0);
    *phase_sample += n;
}

fn apply_qsb(
    samples: &mut [f32],
    sample_rate: u32,
    depth: f32,
    rate_hz: f32,
    floor: f32,
    phase: f32,
) {
    for (i, sample) in samples.iter_mut().enumerate() {
        let t = i as f32 / sample_rate as f32;
        let fade = floor + (1.0 - floor) * (0.5 + 0.5 * (TAU * rate_hz * t + phase).sin());
        *sample *= 1.0 - depth + depth * fade;
    }
}

fn add_qrm_tone(samples: &mut [f32], sample_rate: u32, freq_hz: f32, amp: f32) {
    for (i, sample) in samples.iter_mut().enumerate() {
        let t = i as f32 / sample_rate as f32;
        *sample += (TAU * freq_hz * t).sin() * amp;
    }
}

fn static_noise(sample_rate: u32, seconds: f32, amp: f32, rng: &mut Lcg) -> Vec<f32> {
    let n = (seconds * sample_rate as f32).round() as usize;
    (0..n).map(|_| rng.next_bipolar() * amp).collect()
}

fn timing_jitter(rng: &mut Lcg, width: f32) -> f32 {
    (1.0 + rng.next_bipolar() * width).max(0.2)
}

fn clamp_samples(samples: &mut [f32]) {
    for sample in samples {
        *sample = sample.clamp(-0.98, 0.98);
    }
}

fn write_wav(path: &Path, sample_rate: u32, samples: &[f32]) -> Result<()> {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer = hound::WavWriter::create(path, spec)
        .with_context(|| format!("creating WAV {}", path.display()))?;
    for sample in samples {
        writer
            .write_sample((sample * i16::MAX as f32) as i16)
            .context("writing sample")?;
    }
    writer.finalize().context("finalising WAV")?;
    Ok(())
}

fn normalize_transcript(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn canonical_text(text: &str) -> String {
    text.chars()
        .map(|ch| ch.to_ascii_uppercase())
        .filter(|ch| ch.is_ascii_alphanumeric() || ch.is_ascii_whitespace())
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn morse_for_char(c: char) -> Option<&'static str> {
    match c.to_ascii_uppercase() {
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

#[derive(Debug, Clone, Copy)]
struct Lcg {
    state: u64,
}

impl Lcg {
    fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    fn next_unit(&mut self) -> f32 {
        self.state = self
            .state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        ((self.state >> 33) as u32) as f32 / u32::MAX as f32
    }

    fn next_bipolar(&mut self) -> f32 {
        self.next_unit() * 2.0 - 1.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suite_contains_requested_ragchew_and_contest_examples() {
        let examples = generate_qso_suite(12_000, 6, 6);
        assert_eq!(
            examples
                .iter()
                .filter(|e| e.kind == SyntheticQsoKind::Ragchew)
                .count(),
            6
        );
        assert_eq!(
            examples
                .iter()
                .filter(|e| e.kind == SyntheticQsoKind::Contest)
                .count(),
            6
        );
        assert!(examples.iter().all(|e| e.truth.contains("KC7AVA")));
    }

    #[test]
    fn first_contest_example_validates_exact_copy() {
        let example = generate_qso_suite(12_000, 0, 1).remove(0);
        let validation = validate_example(&example, 500);
        assert!(
            validation.exact_match,
            "expected exact copy for {}, truth={:?}, transcript={:?}",
            example.id, validation.truth, validation.transcript
        );
    }

    #[test]
    fn final_over_k_after_cq_is_not_read_as_s() {
        let sample_rate = 12_000;
        let example = build_example(
            "final-k-cq".to_string(),
            SyntheticQsoKind::Contest,
            sample_rate,
            vec![SyntheticQsoBurst {
                text: "CQ CQ DE W7N W7N K".to_string(),
                wpm: 22.0,
                pitch_hz: 700.0,
                amp: 0.54,
            }],
            SyntheticQsoImpairments {
                static_amp: 0.018,
                qsb_depth: 0.22,
                qsb_rate_hz: 0.33,
                qrm_hz: Some(830.0),
                qrm_amp: 0.018,
            },
            0xF1A1_0ACE,
        );
        let validation = validate_example(&example, 500);
        assert_eq!(
            validation.transcript, validation.truth,
            "final over marker must remain K, truth={:?}, transcript={:?}",
            validation.truth, validation.transcript
        );
    }

    #[test]
    fn isolated_final_over_k_region_is_not_read_as_s() {
        let sample_rate = 12_000;
        let example = build_example(
            "isolated-final-k".to_string(),
            SyntheticQsoKind::Contest,
            sample_rate,
            vec![
                SyntheticQsoBurst {
                    text: "CQ CQ DE W7N W7N".to_string(),
                    wpm: 22.0,
                    pitch_hz: 700.0,
                    amp: 0.54,
                },
                SyntheticQsoBurst {
                    text: "K".to_string(),
                    wpm: 22.0,
                    pitch_hz: 700.0,
                    amp: 0.54,
                },
            ],
            SyntheticQsoImpairments {
                static_amp: 0.018,
                qsb_depth: 0.22,
                qsb_rate_hz: 0.33,
                qrm_hz: Some(830.0),
                qrm_amp: 0.018,
            },
            0xF1A1_0ACF,
        );
        let validation = validate_example(&example, 500);
        assert_eq!(
            validation.transcript, validation.truth,
            "isolated final over marker must remain K, truth={:?}, transcript={:?}",
            validation.truth, validation.transcript
        );
    }
}
