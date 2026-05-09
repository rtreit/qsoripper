use anyhow::{Result, bail};
use rubato::{
    Resampler, SincFixedIn, SincInterpolationParameters, SincInterpolationType, WindowFunction,
};
use rustfft::{FftPlanner, num_complex::Complex};
use std::collections::VecDeque;
use std::io::Write;
// --- DSP Constants ---
const FREQ_MIN_HZ: f32 = 200.0;
const FREQ_MAX_HZ: f32 = 1200.0;
const RESAMPLER_CHUNK_SIZE: usize = 1024;

// --- Decoding Constants ---
const BAD_COPY_MARKER: char = '*';

// --- Element-gate experiment constants ---
/// Default per-element SNR threshold in dB. Elements whose local SNR falls
/// below this are dropped before self-calibration and the per-sample walk.
/// Set the `DITDAH_ELEM_GATE_DB` environment variable to a number to
/// override, or to "off" to disable element gating entirely.
const ELEM_GATE_DEFAULT_DB: f32 = 12.0;
/// Total length (seconds) of the local noise-floor window centered on each
/// candidate on-interval. Off-mask samples inside this window are used to
/// estimate the background noise power.
const ELEM_GATE_NOISE_WINDOW_S: f32 = 1.5;

// --- Variant-stack experiment constants ---
/// Default lambda (weight) for the SNR log-likelihood term added to per-element
/// scores in `soft` and `abstain` stack modes. Override with
/// `DITDAH_STACK_LAMBDA`.
const STACK_SOFT_LAMBDA_DEFAULT: f32 = 1.0;
/// Default mean (dB) of the "real element" SNR Gaussian used by the soft
/// stack. Override with `DITDAH_STACK_SNR_MEAN_DB`. Defaults to the same
/// value as the hard elem-gate threshold so the two variants share a
/// calibration point.
const STACK_SOFT_SNR_MEAN_DB_DEFAULT: f32 = 12.0;
/// Default sigma (dB) of the SNR Gaussians (real and noise priors share it).
/// Override with `DITDAH_STACK_SNR_SIGMA_DB`.
const STACK_SOFT_SNR_SIGMA_DB_DEFAULT: f32 = 4.0;
/// Default abstain threshold (sum of dot/dash + lambda*SNR log-prob) below
/// which a single-element E/T letter is suppressed. Override with
/// `DITDAH_STACK_ABSTAIN`.
const STACK_ABSTAIN_FLOOR_DEFAULT: f32 = -5.0;

// --- BiquadFilter (Unchanged) ---
#[derive(Debug, Clone, Copy)]
pub enum FilterType {
    HighPass,
    LowPass,
}
pub struct BiquadFilter {
    a0: f32,
    a1: f32,
    a2: f32,
    b1: f32,
    b2: f32,
    x1: f32,
    x2: f32,
    y1: f32,
    y2: f32,
}
impl BiquadFilter {
    pub fn new(filter_type: FilterType, cutoff_hz: f32, sample_rate: u32) -> Self {
        let mut filter = Self {
            a0: 1.0,
            a1: 0.0,
            a2: 0.0,
            b1: 0.0,
            b2: 0.0,
            x1: 0.0,
            x2: 0.0,
            y1: 0.0,
            y2: 0.0,
        };
        let c = (std::f32::consts::PI * cutoff_hz / sample_rate as f32).tan();
        let sqrt2 = 2.0f32.sqrt();
        match filter_type {
            FilterType::LowPass => {
                let d = 1.0 / (1.0 + sqrt2 * c + c * c);
                filter.a0 = c * c * d;
                filter.a1 = 2.0 * filter.a0;
                filter.a2 = filter.a0;
                filter.b1 = 2.0 * (c * c - 1.0) * d;
                filter.b2 = (1.0 - sqrt2 * c + c * c) * d;
            }
            FilterType::HighPass => {
                let d = 1.0 / (1.0 + sqrt2 * c + c * c);
                filter.a0 = d;
                filter.a1 = -2.0 * d;
                filter.a2 = d;
                filter.b1 = 2.0 * (c * c - 1.0) * d;
                filter.b2 = (1.0 - sqrt2 * c + c * c) * d;
            }
        }
        filter
    }
    pub fn process(&mut self, input: &mut [f32]) {
        for sample in input.iter_mut() {
            let x0 = *sample;
            let y0 = self.a0 * x0 + self.a1 * self.x1 + self.a2 * self.x2
                - self.b1 * self.y1
                - self.b2 * self.y2;
            self.x2 = self.x1;
            self.x1 = x0;
            self.y2 = self.y1;
            self.y1 = y0;
            *sample = y0;
        }
    }
}

// --- Goertzel Filter (Unchanged) ---
struct Goertzel {
    coeff: f32,
    window: Vec<f32>,
}
impl Goertzel {
    fn new(target_freq: f32, sample_rate: u32, window_size: usize) -> Self {
        let k = 0.5 + (window_size as f32 * target_freq) / sample_rate as f32;
        let omega = (2.0 * std::f32::consts::PI * k) / window_size as f32;
        let coeff = 2.0 * omega.cos();
        let window = (0..window_size)
            .map(|i| {
                0.54 - 0.46 * (2.0 * std::f32::consts::PI * i as f32 / window_size as f32).cos()
            })
            .collect();
        Self { coeff, window }
    }
    fn run(&self, samples: &[f32]) -> f32 {
        let mut q1 = 0.0;
        let mut q2 = 0.0;
        for (i, &sample) in samples.iter().enumerate() {
            let q0 = self.coeff * q1 - q2 + sample * self.window[i];
            q2 = q1;
            q1 = q0;
        }
        q1 * q1 + q2 * q2 - self.coeff * q1 * q2
    }
    fn process_decimated(&self, samples: &[f32], step_size: usize) -> Vec<f32> {
        if samples.len() < self.window.len() {
            return Vec::new();
        }
        samples
            .windows(self.window.len())
            .step_by(step_size)
            .map(|chunk| self.run(chunk))
            .collect()
    }
}
pub struct MorseDecoder {
    resampler: Option<SincFixedIn<f32>>,
    filter_hp: BiquadFilter,
    filter_lp: BiquadFilter,
    input_buffer: Vec<f32>, // Buffer for raw audio before resampling
    audio_buffer: Vec<f32>, // Buffer for resampled, filtered audio
    target_sample_rate: u32,
}

impl MorseDecoder {
    pub fn new(source_sample_rate: u32, target_sample_rate: u32) -> Result<Self> {
        let resampler = if source_sample_rate != target_sample_rate {
            Some(SincFixedIn::new(
                target_sample_rate as f64 / source_sample_rate as f64,
                2.0,
                SincInterpolationParameters {
                    sinc_len: 256,
                    f_cutoff: 0.95,
                    interpolation: SincInterpolationType::Linear,
                    oversampling_factor: 256,
                    window: WindowFunction::BlackmanHarris,
                },
                RESAMPLER_CHUNK_SIZE,
                1,
            )?)
        } else {
            None
        };

        Ok(Self {
            resampler,
            filter_hp: BiquadFilter::new(FilterType::HighPass, FREQ_MIN_HZ, target_sample_rate),
            filter_lp: BiquadFilter::new(FilterType::LowPass, FREQ_MAX_HZ, target_sample_rate),
            input_buffer: Vec::new(),
            audio_buffer: Vec::new(),
            target_sample_rate,
        })
    }

    /// Processes a chunk of audio. Buffers input to meet the resampler's requirements.
    pub fn process(&mut self, chunk: &[f32]) -> Result<()> {
        if let Some(resampler) = &mut self.resampler {
            // Add new audio to our input buffer
            self.input_buffer.extend_from_slice(chunk);

            // Process full chunks from the buffer
            while self.input_buffer.len() >= RESAMPLER_CHUNK_SIZE {
                let waves_in = &[&self.input_buffer[..RESAMPLER_CHUNK_SIZE]];
                let mut resampled = resampler.process(waves_in, None)?;
                self.input_buffer.drain(..RESAMPLER_CHUNK_SIZE);

                let mut processed_chunk = resampled.remove(0);
                self.filter_hp.process(&mut processed_chunk);
                self.filter_lp.process(&mut processed_chunk);
                self.audio_buffer.extend(processed_chunk);
            }
        } else {
            // No resampling, just filter and add to the main buffer
            let mut processed_chunk = chunk.to_vec();
            self.filter_hp.process(&mut processed_chunk);
            self.filter_lp.process(&mut processed_chunk);
            self.audio_buffer.extend(processed_chunk);
        }
        Ok(())
    }

    /// Finalizes decoding. Processes any remaining buffered audio and decodes the full signal.
    pub fn finalize(&mut self) -> Result<String> {
        // --- Flush remaining audio from the input buffer ---
        if let Some(resampler) = &mut self.resampler {
            if !self.input_buffer.is_empty() {
                // Pad the remaining buffer to the required chunk size if needed
                while self.input_buffer.len() < RESAMPLER_CHUNK_SIZE {
                    self.input_buffer.push(0.0);
                }
                let waves_in = &[self.input_buffer.as_slice()];
                let mut resampled = resampler.process(waves_in, None)?;
                self.input_buffer.clear();

                let mut processed_chunk = resampled.remove(0);
                self.filter_hp.process(&mut processed_chunk);
                self.filter_lp.process(&mut processed_chunk);
                self.audio_buffer.extend(processed_chunk);
            }
        }

        if self.audio_buffer.is_empty() {
            bail!("Audio buffer is empty, cannot process.");
        }

        // --- The rest of the decoding pipeline is unchanged ---
        let pitch = self.detect_pitch_stft()?;
        log::info!("Estimated pitch: {pitch:.2} Hz");

        let goertzel_window_size = (self.target_sample_rate as f32 * 0.025) as usize;
        let step_size = (goertzel_window_size / 4).max(1);
        let goertzel_filter = Goertzel::new(pitch, self.target_sample_rate, goertzel_window_size);
        let raw_power = goertzel_filter.process_decimated(&self.audio_buffer, step_size);
        let power_signal_rate = self.target_sample_rate as f32 / step_size as f32;

        let smooth_window = (power_signal_rate * 0.02).round() as usize;
        let smoothed_power = moving_average(&raw_power, smooth_window.max(1));
        if smoothed_power.is_empty() {
            bail!("No power signal after processing");
        }

        let (best_wpm, best_threshold) =
            self.find_best_params(&smoothed_power, power_signal_rate)?;
        log::info!("Best fit: WPM = {best_wpm:.1}, Threshold = {best_threshold:.4e}");

        if log::log_enabled!(log::Level::Trace) {
            trace_signal(&smoothed_power, best_threshold, best_wpm)?;
            log::trace!("Wrote signal trace to signal_trace.txt");
        }

        let text =
            self.decode_with_params(&smoothed_power, best_wpm, best_threshold, power_signal_rate);
        Ok(text)
    }

    /// Like `finalize`, but returns the (text, wpm, threshold) actually used so
    /// callers can pin parameters for subsequent decodes (streaming use case).
    /// If `pin_wpm` is `Some`, skips the WPM grid search and uses that WPM. If
    /// `pin_threshold` is `Some`, skips threshold re-fitting too.
    pub fn finalize_with_params(
        &mut self,
        pin_wpm: Option<f32>,
        pin_threshold: Option<f32>,
    ) -> Result<(String, f32, f32)> {
        if let Some(resampler) = &mut self.resampler {
            if !self.input_buffer.is_empty() {
                while self.input_buffer.len() < RESAMPLER_CHUNK_SIZE {
                    self.input_buffer.push(0.0);
                }
                let waves_in = &[self.input_buffer.as_slice()];
                let mut resampled = resampler.process(waves_in, None)?;
                self.input_buffer.clear();
                let mut processed_chunk = resampled.remove(0);
                self.filter_hp.process(&mut processed_chunk);
                self.filter_lp.process(&mut processed_chunk);
                self.audio_buffer.extend(processed_chunk);
            }
        }

        if self.audio_buffer.is_empty() {
            bail!("Audio buffer is empty, cannot process.");
        }

        let pitch = self.detect_pitch_stft()?;
        let goertzel_window_size = (self.target_sample_rate as f32 * 0.025) as usize;
        let step_size = (goertzel_window_size / 4).max(1);
        let goertzel_filter = Goertzel::new(pitch, self.target_sample_rate, goertzel_window_size);
        let raw_power = goertzel_filter.process_decimated(&self.audio_buffer, step_size);
        let power_signal_rate = self.target_sample_rate as f32 / step_size as f32;
        let smooth_window = (power_signal_rate * 0.02).round() as usize;
        let smoothed_power = moving_average(&raw_power, smooth_window.max(1));
        if smoothed_power.is_empty() {
            bail!("No power signal after processing");
        }

        let (wpm, threshold) = match (pin_wpm, pin_threshold) {
            (Some(w), Some(t)) => (w, t),
            (Some(w), None) => {
                let mut sorted = smoothed_power.clone();
                sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
                let n = sorted.len();
                let p25 = sorted[n / 4];
                let p75 = sorted[(3 * n) / 4];
                let iqr = p75 - p25;
                (w, p25 + iqr * 0.50)
            }
            (None, Some(t)) => {
                let mut best_wpm = 20.0;
                let mut best_cost = f32::INFINITY;
                for wpm_int in 5..=40 {
                    let w = wpm_int as f32;
                    let cost = self.calculate_cost(&smoothed_power, w, t, power_signal_rate);
                    if cost < best_cost {
                        best_cost = cost;
                        best_wpm = w;
                    }
                }
                (best_wpm, t)
            }
            (None, None) => self.find_best_params(&smoothed_power, power_signal_rate)?,
        };

        let wpm_is_authoritative = pin_wpm.is_some();
        let text = self.decode_with_params_inner(
            &smoothed_power,
            wpm,
            threshold,
            power_signal_rate,
            wpm_is_authoritative,
        );
        Ok((text, wpm, threshold))
    }

    // --- The complex analysis functions below are unchanged ---
    fn detect_pitch_stft(&self) -> Result<f32> {
        let fft_size = 4096;
        let step_size = fft_size / 4;
        let mut planner = FftPlanner::new();
        let fft = planner.plan_fft_forward(fft_size);
        let window: Vec<f32> = (0..fft_size)
            .map(|i| 0.54 - 0.46 * (2.0 * std::f32::consts::PI * i as f32 / fft_size as f32).cos())
            .collect();
        let mut spectrum_sum = vec![0.0; fft_size / 2];
        let mut count = 0;
        for chunk in self.audio_buffer.windows(fft_size).step_by(step_size) {
            let mut buffer: Vec<Complex<f32>> = chunk
                .iter()
                .zip(window.iter())
                .map(|(s, w)| Complex::new(s * w, 0.0))
                .collect();
            fft.process(&mut buffer);
            for (i, v) in buffer.iter().take(fft_size / 2).enumerate() {
                spectrum_sum[i] += v.norm_sqr();
            }
            count += 1;
        }
        if count == 0 {
            bail!("Not enough audio data for pitch detection");
        }
        let df = self.target_sample_rate as f32 / fft_size as f32;
        let (max_idx, max_power) =
            spectrum_sum
                .iter()
                .enumerate()
                .fold((0, 0.0), |(max_i, max_p), (i, &p)| {
                    let freq = i as f32 * df;
                    if (FREQ_MIN_HZ..=FREQ_MAX_HZ).contains(&freq) && p > max_p {
                        (i, p)
                    } else {
                        (max_i, max_p)
                    }
                });
        if max_power == 0.0 {
            bail!("Could not find a dominant frequency in the specified range.");
        }
        Ok(max_idx as f32 * df)
    }

    fn find_best_params(&self, power_signal: &[f32], power_signal_rate: f32) -> Result<(f32, f32)> {
        if power_signal.is_empty() {
            bail!("Power signal is empty");
        }
        let mut sorted_power: Vec<f32> =
            power_signal.iter().cloned().filter(|&p| p > 0.0).collect();
        if sorted_power.len() < 10 {
            bail!("Not enough signal to determine parameters");
        }
        sorted_power.sort_unstable_by(|a, b| a.partial_cmp(b).unwrap());
        let p25 = sorted_power[(sorted_power.len() as f32 * 0.25) as usize];
        let p75 = sorted_power[(sorted_power.len() as f32 * 0.75) as usize];
        let iqr = p75 - p25;
        let threshold_candidates = [p25 + iqr * 0.25, p25 + iqr * 0.50, p25 + iqr * 0.75];
        let mut best_cost = f32::MAX;
        let mut best_wpm = 20.0;
        let mut best_threshold = threshold_candidates[1];
        for &threshold in &threshold_candidates {
            for wpm_int in 5..=40 {
                let wpm = wpm_int as f32;
                let cost = self.calculate_cost(power_signal, wpm, threshold, power_signal_rate);
                if cost < best_cost {
                    best_cost = cost;
                    best_wpm = wpm;
                    best_threshold = threshold;
                }
            }
        }
        Ok((best_wpm, best_threshold))
    }

    fn calculate_cost(
        &self,
        power_signal: &[f32],
        wpm: f32,
        threshold: f32,
        power_signal_rate: f32,
    ) -> f32 {
        let (on_intervals, off_intervals) = get_raw_intervals(power_signal, threshold);
        if on_intervals.len() < 3 || off_intervals.len() < 3 {
            return f32::MAX;
        }
        let dot_len_samples = (1200.0 / wpm / 1000.0) * power_signal_rate;
        if dot_len_samples < 1.0 {
            return f32::MAX;
        }
        let on_norm: Vec<f32> = on_intervals
            .iter()
            .map(|&s| s as f32 / dot_len_samples)
            .collect();
        let off_norm: Vec<f32> = off_intervals
            .iter()
            .map(|&s| s as f32 / dot_len_samples)
            .collect();
        let mut short_elements: Vec<f32> = on_norm
            .iter()
            .chain(off_norm.iter())
            .cloned()
            .filter(|&l| l < 2.0)
            .collect();
        if short_elements.is_empty() {
            return f32::MAX;
        }
        short_elements.sort_unstable_by(|a, b| a.partial_cmp(b).unwrap());
        let median_dot_len = short_elements[short_elements.len() / 2];
        if median_dot_len < 0.25 {
            return f32::MAX;
        }
        let cost_on: f32 = on_norm
            .iter()
            .map(|&len| {
                (len / median_dot_len - 1.0)
                    .powi(2)
                    .min((len / median_dot_len - 3.0).powi(2))
            })
            .sum();
        let cost_off: f32 = off_norm
            .iter()
            .map(|&len| {
                (len / median_dot_len - 1.0)
                    .powi(2)
                    .min((len / median_dot_len - 3.0).powi(2))
                    .min((len / median_dot_len - 7.0).powi(2))
            })
            .sum();
        (cost_on / on_intervals.len() as f32) + (cost_off / off_intervals.len() as f32)
    }

    fn decode_with_params(
        &self,
        power_signal: &[f32],
        wpm: f32,
        threshold: f32,
        power_signal_rate: f32,
    ) -> String {
        self.decode_with_params_inner(power_signal, wpm, threshold, power_signal_rate, false)
    }

    /// Internal decoder body. When `wpm_is_authoritative` is true, the dot length
    /// is taken from `wpm` (theoretical timing) instead of the median-element-length
    /// self-calibration. The self-calibration heuristic is robust on clean studio
    /// audio but breaks on real-world live signals where the element-length
    /// distribution gets distorted by noise, fading, or partial elements.
    fn decode_with_params_inner(
        &self,
        power_signal: &[f32],
        wpm: f32,
        threshold: f32,
        power_signal_rate: f32,
        wpm_is_authoritative: bool,
    ) -> String {
        if power_signal.is_empty() {
            return String::new();
        }

        // STACK EXPERIMENT (vit-elem-stack):
        // The `DITDAH_STACK` env var selects one of four variants that
        // combine the round-1 viterbi soft decoder with the round-2
        // per-element SNR gate in different ways. See `StackMode` for the
        // catalog. The default mode (`StackMode::Default`) preserves the
        // pre-experiment behavior (hard mask at `DITDAH_ELEM_GATE_DB` +
        // viterbi soft decode), so unset env yields the elem-gate baseline.
        let stack_mode = stack_mode_from_env();
        let stack_params = StackParams::from_env();

        // ELEM-GATE EXPERIMENT (stacked on top of the viterbi soft decoder):
        // Per-element confidence gating before the soft decode pass.
        //
        // Live OTA CW produces "ghost character" cascades (E/T/I/M/S) when the
        // SNR drops within an active region. Window-level pitch-power gating
        // can't separate ghosts from legit weak elements because they share
        // the same pitch. Here we instead score every detected on-interval
        // against the *local* envelope noise floor (samples NOT inside any
        // detected on-interval, in a window centered on the element). Drop
        // intervals whose SNR falls below `DITDAH_ELEM_GATE_DB` (default 12.0).
        //
        // Dropped on-intervals get zeroed in a working copy of `power_signal`
        // BEFORE the soft decoder runs, which causes the surrounding
        // off-intervals to merge naturally — turning a spurious "letter
        // break" into a longer letter or word gap.
        let apply_hard_mask = matches!(stack_mode, StackMode::Default | StackMode::Hard);
        let gate_db = if apply_hard_mask {
            elem_gate_threshold_db()
        } else {
            None
        };
        let working_signal: std::borrow::Cow<'_, [f32]> = if let Some(t_db) = gate_db {
            let on_with_pos = get_raw_on_intervals_with_positions(power_signal, threshold);
            if on_with_pos.is_empty() {
                std::borrow::Cow::Borrowed(power_signal)
            } else {
                let snrs = compute_element_snrs_db(
                    power_signal,
                    &on_with_pos,
                    power_signal_rate,
                    ELEM_GATE_NOISE_WINDOW_S,
                );
                let mut dropped = 0usize;
                let mut masked = power_signal.to_vec();
                for ((start, len), snr) in on_with_pos.iter().zip(snrs.iter()) {
                    if !snr.is_finite() || *snr < t_db {
                        let end = (start + len).min(masked.len());
                        for s in &mut masked[*start..end] {
                            *s = 0.0;
                        }
                        dropped += 1;
                    }
                }
                log::debug!(
                    "elem-gate: dropped {dropped}/{} on-intervals (threshold={t_db:.1} dB)",
                    on_with_pos.len()
                );
                std::borrow::Cow::Owned(masked)
            }
        } else {
            std::borrow::Cow::Borrowed(power_signal)
        };
        let power_signal: &[f32] = &working_signal;

        // Collect interleaved on/off intervals for the whole signal.
        let raw_intervals = get_interleaved_intervals(power_signal, threshold);
        if raw_intervals.is_empty() {
            return String::new();
        }
        let on_intervals: Vec<usize> = raw_intervals
            .iter()
            .filter_map(|&(o, l)| if o { Some(l) } else { None })
            .collect();
        if on_intervals.is_empty() {
            return String::new();
        }

        let actual_dot_len = if wpm_is_authoritative && wpm > 0.0 {
            // Theoretical dot length in power-signal samples for the pinned WPM.
            (1200.0 / wpm / 1000.0) * power_signal_rate
        } else {
            // Self-calibrate: detect if we have mixed dots/dashes or all same type
            let mut sorted_lengths = on_intervals.clone();
            sorted_lengths.sort_unstable();

            let min_len = sorted_lengths[0] as f32;
            let max_len = sorted_lengths[sorted_lengths.len() - 1] as f32;
            let length_ratio = max_len / min_len;

            if length_ratio > 2.0 {
                // Mixed signal: use shortest elements as dots
                let shortest_half = &sorted_lengths[0..=(sorted_lengths.len() / 2)];
                shortest_half[shortest_half.len() / 2] as f32
            } else {
                // All similar lengths: Use a simple heuristic based on absolute length
                // This is more robust than relying on potentially inaccurate WPM estimates
                let median_len = sorted_lengths[sorted_lengths.len() / 2] as f32;

                // Based on actual observed values:
                // - EEEE (dots): median ~10 power signal samples
                // - TTTT (dashes): median ~29 power signal samples
                let breakpoint = 18.0;

                if median_len > breakpoint {
                    median_len / 3.0
                } else {
                    median_len
                }
            }
        };

        log::debug!(
            "Self-calibration: WPM={wpm:.1} (authoritative={wpm_is_authoritative}), actual_dot_len={actual_dot_len:.1} samples"
        );

        let debounce_samples = ((actual_dot_len * 0.30).round() as usize).max(1);
        log::debug!("Debounce threshold: {debounce_samples} samples");

        // Merge sub-debounce blips into the surrounding runs; this is where
        // most short noise-induced state flips disappear before the soft
        // classifier ever sees them.
        let intervals = merge_short_blips(&raw_intervals, debounce_samples);
        if intervals.is_empty() {
            return String::new();
        }

        // Soft / abstain stack modes: compute per-on-interval SNR (dB) on the
        // unmasked signal and pass it through to the soft decoder so it can
        // add a likelihood term that smoothly penalizes low-SNR elements
        // (instead of the hard threshold cliff). The merge step is monotone
        // in cumulative sample position, so we can derive on-interval start
        // positions directly from the post-merge interval list.
        let soft_snrs: Option<Vec<f32>> =
            if matches!(stack_mode, StackMode::Soft | StackMode::Abstain) {
                let mut on_with_pos: Vec<(usize, usize)> = Vec::new();
                let mut pos = 0usize;
                for &(is_on, len) in &intervals {
                    if is_on {
                        on_with_pos.push((pos, len));
                    }
                    pos += len;
                }
                if on_with_pos.is_empty() {
                    None
                } else {
                    Some(compute_element_snrs_db(
                        power_signal,
                        &on_with_pos,
                        power_signal_rate,
                        ELEM_GATE_NOISE_WINDOW_S,
                    ))
                }
            } else {
                None
            };

        soft_decode_intervals(
            &intervals,
            actual_dot_len,
            soft_snrs.as_deref(),
            stack_mode,
            &stack_params,
        )
    }
}

// --- Soft / Viterbi-style decoder ---

#[derive(Debug, Clone, Copy)]
struct ElementScore {
    log_p_dot: f32,
    log_p_dash: f32,
}

#[derive(Debug, Clone, Copy)]
struct GapScore {
    log_p_elem: f32,
    log_p_letter: f32,
    log_p_word: f32,
}

/// Per-element noise-rejection floor (log-prob). An on-interval whose best
/// dot/dash log-prob is below this is treated as a noise-only burst and
/// dropped (not used to extend the current letter).
const ELEMENT_NOISE_FLOOR: f32 = -3.0;

/// Extra log-prob floor required to *emit* a single-element letter (E or T).
/// If the lone element's best log-prob is below this, the letter is
/// suppressed even if a clean letter-gap surrounds it. This is the headline
/// ghost-suppression knob.
const SINGLE_ELEMENT_EMIT_FLOOR: f32 = -0.5;

/// Log-prior penalty added to the letter-break / word-break scores when the
/// letter currently being built is a single element. Encourages the gap
/// classifier to merge ambiguous gaps into the next element rather than
/// emit an E/T ghost.
const SINGLE_ELEMENT_GAP_PENALTY: f32 = 0.6;

/// Sigma for on-interval scoring, in dot-length units. Equal across the dot
/// and dash classes so the dot/dash decision boundary stays at the canonical
/// 2*dot_len, matching the old hard-threshold semantics. The smooth scores
/// still let us reject obvious noise (large |z|) and apply confidence
/// floors at letter-emit time.
const SIGMA_ON_DOTS: f32 = 0.55;
/// Sigma for off-interval scoring, in dot-length units. Equal across the
/// elem-gap, letter-gap, and word-gap classes so the canonical 2x/5x
/// boundaries are preserved while still scoring borderline gaps softly.
const SIGMA_OFF_DOTS: f32 = 0.85;

fn gaussian_log_prob(value: f32, mean: f32, sigma: f32) -> f32 {
    let s = sigma.max(0.5);
    let z = (value - mean) / s;
    -0.5 * z * z
}

fn score_on_interval(len: f32, dot_len: f32) -> ElementScore {
    let dash_len = 3.0 * dot_len;
    let sigma = SIGMA_ON_DOTS * dot_len;
    ElementScore {
        log_p_dot: gaussian_log_prob(len, dot_len, sigma),
        log_p_dash: gaussian_log_prob(len, dash_len, sigma),
    }
}

fn score_off_interval(len: f32, dot_len: f32) -> GapScore {
    let sigma = SIGMA_OFF_DOTS * dot_len;
    GapScore {
        log_p_elem: gaussian_log_prob(len, dot_len, sigma),
        log_p_letter: gaussian_log_prob(len, 3.0 * dot_len, sigma),
        log_p_word: gaussian_log_prob(len, 7.0 * dot_len, sigma),
    }
}

/// Merge sub-debounce intervals into the surrounding runs. A short blip is
/// absorbed into the preceding interval (lengthening it), and the next
/// same-type interval is then concatenated, eliminating spurious state flips
/// caused by noise glitches.
fn merge_short_blips(intervals: &[(bool, usize)], debounce: usize) -> Vec<(bool, usize)> {
    let mut out: Vec<(bool, usize)> = Vec::with_capacity(intervals.len());
    for &(is_on, len) in intervals {
        if let Some(last) = out.last_mut() {
            if last.0 == is_on {
                // A previous merge left a same-type predecessor; concatenate.
                last.1 += len;
                continue;
            }
        }
        if len < debounce && !out.is_empty() {
            // Absorb the blip into the previous (opposite-type) interval. The
            // next interval (which will share the previous interval's type)
            // will then be concatenated by the branch above.
            out.last_mut().unwrap().1 += len;
            continue;
        }
        out.push((is_on, len));
    }
    out
}

/// Soft per-element classification + symbol-length-prior gap decoder.
///
/// This replaces the hard threshold-based classifier. For each on-interval we
/// score it as dot vs dash under per-class Gaussians (with per-class sigmas
/// proportional to the class mean). Intervals whose best class log-prob is
/// below `ELEMENT_NOISE_FLOOR` are dropped (treated as noise that survived
/// merge_short_blips). For each off-interval we score elem-gap / letter-gap /
/// word-gap and pick the best score after adding a single-element-letter
/// penalty to the letter/word-break scores. Finally, when emitting a
/// completed letter we require a per-element confidence floor for the
/// special case of single-element letters (E and T) — the dominant source
/// of ghost characters in real OTA noise bursts.
///
/// `snrs_per_on` (when `Some`) holds one SNR-in-dB per on-interval, and
/// `mode`/`params` together control whether the SNR is folded into the
/// element score (soft/abstain stacks) and whether single-element E/T
/// letters get an extra abstain check (abstain stack only).
fn soft_decode_intervals(
    intervals: &[(bool, usize)],
    dot_len: f32,
    snrs_per_on: Option<&[f32]>,
    mode: StackMode,
    params: &StackParams,
) -> String {
    if dot_len <= 0.0 || intervals.is_empty() {
        return String::new();
    }

    let mut result = String::new();
    let mut letter = String::new();
    // Track the worst-case (minimum) log-prob across the elements that built
    // up `letter`. Used for single-element ghost suppression at flush time.
    let mut letter_min_conf = f32::INFINITY;
    // Track the *summed* per-element log-prob for abstain-mode evaluation.
    // This includes the SNR term so abstain is sensitive to *both* duration
    // and channel-quality confidence.
    let mut letter_score_sum: f32 = 0.0;
    // SNR cursor (parallel to on-interval index, NOT raw interval index).
    let mut on_idx: usize = 0;
    let use_snr = matches!(mode, StackMode::Soft | StackMode::Abstain);
    let abstain_active = matches!(mode, StackMode::Abstain);

    let flush = |letter: &mut String,
                 letter_min_conf: &mut f32,
                 letter_score_sum: &mut f32,
                 result: &mut String,
                 force_keep: bool| {
        if letter.is_empty() {
            *letter_min_conf = f32::INFINITY;
            *letter_score_sum = 0.0;
            return;
        }
        let is_single = letter.chars().count() == 1;
        let suppressed_min =
            !force_keep && is_single && *letter_min_conf < SINGLE_ELEMENT_EMIT_FLOOR;
        // Abstain: drop a single-element E/T whose summed log-prob (duration
        // + SNR) falls below the abstain floor. Multi-element letters are
        // unaffected — abstain targets the ghost-prone E/T case only.
        let suppressed_abstain =
            !force_keep && abstain_active && is_single && *letter_score_sum < params.abstain_floor;
        if !(suppressed_min || suppressed_abstain) {
            if let Some(c) = morse_to_char(letter) {
                result.push(c);
            } else {
                result.push(BAD_COPY_MARKER);
            }
        }
        letter.clear();
        *letter_min_conf = f32::INFINITY;
        *letter_score_sum = 0.0;
    };

    for &(is_on, len) in intervals {
        let l = len as f32;
        if is_on {
            let s = score_on_interval(l, dot_len);
            // Optional SNR likelihood term (soft / abstain stack modes).
            let snr_log_lr = if use_snr {
                let snr = snrs_per_on.and_then(|v| v.get(on_idx).copied());
                on_idx += 1;
                snr.map(|db| params.snr_log_lr(db)).unwrap_or(0.0)
            } else {
                0.0
            };
            let lambda = params.lambda;
            let log_p_dot_eff = s.log_p_dot + lambda * snr_log_lr;
            let log_p_dash_eff = s.log_p_dash + lambda * snr_log_lr;
            let best = log_p_dot_eff.max(log_p_dash_eff);
            if best < ELEMENT_NOISE_FLOOR {
                // Implausible as either dot or dash — treat as noise. Don't
                // extend the current letter; leave the surrounding gaps to
                // decide its boundary on the next pass.
                continue;
            }
            if log_p_dot_eff >= log_p_dash_eff {
                letter.push('.');
                letter_min_conf = letter_min_conf.min(log_p_dot_eff);
                letter_score_sum += log_p_dot_eff;
            } else {
                letter.push('-');
                letter_min_conf = letter_min_conf.min(log_p_dash_eff);
                letter_score_sum += log_p_dash_eff;
            }
        } else {
            let g = score_off_interval(l, dot_len);
            let single = letter.chars().count() == 1;
            let prior_break = if single {
                -SINGLE_ELEMENT_GAP_PENALTY
            } else {
                0.0
            };
            let s_elem = g.log_p_elem;
            let s_letter = g.log_p_letter + prior_break;
            let s_word = g.log_p_word + prior_break;

            // Pick the best decision (continue letter / break letter / word).
            if s_word >= s_letter && s_word >= s_elem {
                flush(
                    &mut letter,
                    &mut letter_min_conf,
                    &mut letter_score_sum,
                    &mut result,
                    false,
                );
                if !result.ends_with(' ') && !result.is_empty() {
                    result.push(' ');
                }
            } else if s_letter >= s_elem {
                flush(
                    &mut letter,
                    &mut letter_min_conf,
                    &mut letter_score_sum,
                    &mut result,
                    false,
                );
            }
            // Otherwise s_elem wins — keep extending the current letter.
        }
    }

    // Flush trailing letter. Apply the same single-element suppression to the
    // tail so a noise-spike at the end of the buffer doesn't add a ghost E/T.
    flush(
        &mut letter,
        &mut letter_min_conf,
        &mut letter_score_sum,
        &mut result,
        false,
    );

    result.trim().to_string()
}

fn get_interleaved_intervals(power_signal: &[f32], threshold: f32) -> Vec<(bool, usize)> {
    let mut out = Vec::new();
    if power_signal.is_empty() {
        return out;
    }
    let mut current_len: usize = 0;
    let mut is_on = power_signal[0] > threshold;
    for &p in power_signal {
        if (p > threshold) == is_on {
            current_len += 1;
        } else {
            out.push((is_on, current_len));
            is_on = !is_on;
            current_len = 1;
        }
    }
    out.push((is_on, current_len));
    out
}

// --- Helper Functions ---
fn get_raw_intervals(power_signal: &[f32], threshold: f32) -> (Vec<usize>, Vec<usize>) {
    let mut on = Vec::new();
    let mut off = Vec::new();
    if power_signal.is_empty() {
        return (on, off);
    }

    let mut current_len = 0;
    let mut is_on = power_signal[0] > threshold;
    for &p in power_signal {
        if (p > threshold) == is_on {
            current_len += 1;
        } else {
            if is_on {
                on.push(current_len);
            } else {
                off.push(current_len);
            }
            is_on = !is_on;
            current_len = 1;
        }
    }
    if is_on {
        on.push(current_len);
    } else {
        off.push(current_len);
    }
    (on, off)
}

/// Like `get_raw_intervals`, but returns `(start_index, length)` pairs for
/// the on-intervals only. Used by the element-gate experiment so we can
/// compute per-element SNR against the local noise floor.
fn get_raw_on_intervals_with_positions(
    power_signal: &[f32],
    threshold: f32,
) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    if power_signal.is_empty() {
        return out;
    }
    let mut current_len = 0usize;
    let mut current_start = 0usize;
    let mut is_on = power_signal[0] > threshold;
    for (i, &p) in power_signal.iter().enumerate() {
        let now_on = p > threshold;
        if now_on == is_on {
            current_len += 1;
        } else {
            if is_on {
                out.push((current_start, current_len));
            }
            is_on = now_on;
            current_start = i;
            current_len = 1;
        }
    }
    if is_on {
        out.push((current_start, current_len));
    }
    out
}

/// Read the user-tunable per-element SNR gate from the `DITDAH_ELEM_GATE_DB`
/// environment variable. Returns `None` when the gate is explicitly disabled
/// ("off"/"none"/"disabled"), and `Some(default)` when the variable is unset
/// or unparsable.
fn elem_gate_threshold_db() -> Option<f32> {
    match std::env::var("DITDAH_ELEM_GATE_DB") {
        Err(_) => Some(ELEM_GATE_DEFAULT_DB),
        Ok(raw) => {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                return Some(ELEM_GATE_DEFAULT_DB);
            }
            if matches!(
                trimmed.to_ascii_lowercase().as_str(),
                "off" | "none" | "disabled" | "0off" | "false"
            ) {
                return None;
            }
            match trimmed.parse::<f32>() {
                Ok(v) if v.is_finite() => Some(v),
                _ => Some(ELEM_GATE_DEFAULT_DB),
            }
        }
    }
}

/// Stack-variant selector. The `DITDAH_STACK` env var picks one of the four
/// variants benched in this experiment. Unset (or unrecognized) preserves
/// the pre-experiment default behaviour (which is itself the round-2
/// "elem-gate" stack atop the round-1 viterbi soft decoder).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StackMode {
    /// Unset env: keep pre-experiment behaviour = hard elem-gate + viterbi.
    Default,
    /// `viterbi` / `viterbi-only` / `off`: viterbi soft decoder only,
    /// no elem-gate masking and no SNR likelihood. Baseline for the stack.
    ViterbiOnly,
    /// `hard`: hard elem-gate mask before viterbi (same as `Default`, but
    /// kept as an explicit name for the bench script).
    Hard,
    /// `soft`: no hard mask; instead fold an SNR Gaussian log-likelihood
    /// term into the per-element score before the dot/dash + gap walk.
    Soft,
    /// `abstain`: like `soft`, plus suppress single-element E/T letters
    /// whose summed score falls below the abstain threshold.
    Abstain,
}

fn stack_mode_from_env() -> StackMode {
    match std::env::var("DITDAH_STACK") {
        Err(_) => StackMode::Default,
        Ok(raw) => match raw.trim().to_ascii_lowercase().as_str() {
            "" | "default" => StackMode::Default,
            "viterbi" | "viterbi-only" | "vit" | "off" | "none" => StackMode::ViterbiOnly,
            "hard" | "hard-stack" | "elem-gate" => StackMode::Hard,
            "soft" | "soft-stack" => StackMode::Soft,
            "abstain" | "abstaining" | "abstaining-stack" | "abstain-stack" => StackMode::Abstain,
            _ => StackMode::Default,
        },
    }
}

/// Tunables for the `soft` and `abstain` stack variants.
#[derive(Debug, Clone, Copy)]
pub(crate) struct StackParams {
    pub lambda: f32,
    pub snr_mean_db: f32,
    pub snr_sigma_db: f32,
    pub abstain_floor: f32,
}

impl StackParams {
    pub(crate) fn from_env() -> Self {
        Self {
            lambda: env_f32_or("DITDAH_STACK_LAMBDA", STACK_SOFT_LAMBDA_DEFAULT),
            snr_mean_db: env_f32_or("DITDAH_STACK_SNR_MEAN_DB", STACK_SOFT_SNR_MEAN_DB_DEFAULT),
            snr_sigma_db: env_f32_or("DITDAH_STACK_SNR_SIGMA_DB", STACK_SOFT_SNR_SIGMA_DB_DEFAULT),
            abstain_floor: env_f32_or("DITDAH_STACK_ABSTAIN", STACK_ABSTAIN_FLOOR_DEFAULT),
        }
    }

    /// Log-likelihood ratio of "real element" vs "noise" given an observed
    /// SNR in dB. Both priors are Gaussian in dB with a shared sigma; the
    /// real-element prior is centered at `snr_mean_db`, the noise prior at
    /// 0 dB. The result is linear in the observed SNR:
    ///
    /// ```text
    /// LLR(snr) = (snr_mean / sigma^2) * (snr - snr_mean / 2)
    /// ```
    ///
    /// Positive when `snr > snr_mean / 2`, negative below — a soft sigmoid
    /// ramp around `snr_mean / 2` instead of the hard step at `snr_mean`
    /// that the elem-gate uses.
    pub(crate) fn snr_log_lr(&self, snr_db: f32) -> f32 {
        if !snr_db.is_finite() {
            // +inf SNR (no noise samples in window): fully favor "real".
            // -inf SNR (degenerate interval): fully favor "noise".
            return if snr_db > 0.0 { 5.0 } else { -5.0 };
        }
        let sigma = self.snr_sigma_db.max(0.5);
        let mean = self.snr_mean_db;
        (mean / (sigma * sigma)) * (snr_db - mean * 0.5)
    }
}

fn env_f32_or(name: &str, default: f32) -> f32 {
    match std::env::var(name) {
        Err(_) => default,
        Ok(raw) => raw
            .trim()
            .parse::<f32>()
            .ok()
            .filter(|v| v.is_finite())
            .unwrap_or(default),
    }
}

/// For each on-interval, compute SNR (dB) against the local off-mask noise
/// floor. The element power is `mean(power_signal[i]^2)` over the interval.
/// The noise floor is the 20th percentile of `power_signal[i]^2` over a
/// window of `noise_window_s` seconds centered on the interval, restricted
/// to samples that are NOT inside any detected on-interval (so we never
/// score a ghost against another ghost — only against true background).
fn compute_element_snrs_db(
    power_signal: &[f32],
    on_intervals: &[(usize, usize)],
    power_signal_rate: f32,
    noise_window_s: f32,
) -> Vec<f32> {
    let n = power_signal.len();
    let mut snrs = Vec::with_capacity(on_intervals.len());
    if n == 0 || on_intervals.is_empty() {
        return snrs;
    }
    let mut on_mask = vec![false; n];
    for &(start, len) in on_intervals {
        let end = (start + len).min(n);
        for slot in &mut on_mask[start..end] {
            *slot = true;
        }
    }
    let half_window = ((power_signal_rate * noise_window_s * 0.5).round() as usize).max(1);
    let mut bg: Vec<f32> = Vec::new();
    for &(start, len) in on_intervals {
        let end = (start + len).min(n);
        if end <= start {
            snrs.push(f32::NEG_INFINITY);
            continue;
        }
        // Element power: mean of squared envelope over the on-interval.
        let mut esum = 0.0_f64;
        for &v in &power_signal[start..end] {
            let v = v as f64;
            esum += v * v;
        }
        let elem_power = (esum / (end - start) as f64) as f32;

        // Local noise samples = squared envelope over the surrounding window
        // EXCLUDING all on-intervals (including this one).
        bg.clear();
        let lo = start.saturating_sub(half_window);
        let hi = (end + half_window).min(n);
        for i in lo..hi {
            if !on_mask[i] {
                let v = power_signal[i];
                bg.push(v * v);
            }
        }
        let noise = if bg.len() >= 5 {
            // Use nth_element-style partial sort for the 20th percentile.
            let p_idx = bg.len() / 5;
            bg.select_nth_unstable_by(p_idx, |a, b| {
                a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal)
            });
            bg[p_idx]
        } else if !bg.is_empty() {
            // Fall back to mean when too few samples for a stable percentile.
            bg.iter().sum::<f32>() / bg.len() as f32
        } else {
            // No noise samples in the window (long element with no padding).
            // Treat as infinite SNR so we never drop the only thing we see.
            snrs.push(f32::INFINITY);
            continue;
        };
        let denom = noise.max(1e-20);
        let ratio = (elem_power / denom).max(1e-20);
        snrs.push(10.0 * ratio.log10());
    }
    snrs
}

fn moving_average(data: &[f32], window_size: usize) -> Vec<f32> {
    if window_size <= 1 {
        return data.to_vec();
    }
    let mut smoothed = Vec::with_capacity(data.len());
    let mut sum = 0.0;
    let mut window = VecDeque::with_capacity(window_size);
    for &x in data {
        if window.len() == window_size {
            sum -= window.pop_front().unwrap();
        }
        sum += x;
        window.push_back(x);
        smoothed.push(sum / window.len() as f32);
    }
    smoothed
}

fn trace_signal(signal: &[f32], threshold: f32, wpm: f32) -> std::io::Result<()> {
    let mut file = std::fs::File::create("signal_trace.txt")?;
    writeln!(file, "# WPM: {wpm:.1}, Threshold: {threshold:.4e}")?;
    let max_val = signal.iter().cloned().fold(f32::MIN, f32::max);
    if max_val <= 0.0 {
        return Ok(());
    }

    for &val in signal {
        let bar_len = (val / max_val * 100.0).round() as usize;
        let thresh_pos = (threshold / max_val * 100.0).round() as usize;
        let mut line = vec![' '; 101];
        for item in line.iter_mut().take(bar_len.min(100)) {
            *item = '#';
        }
        if thresh_pos <= 100 {
            line[thresh_pos] = '|';
        }
        writeln!(file, "{}", line.into_iter().collect::<String>())?;
    }
    Ok(())
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
        // ITU punctuation and prosigns (commonly seen in real CW traffic
        // including ARRL code practice transmissions and contest exchanges).
        "-...-" => Some('='),   // BT prosign / equals (ARRL section separator)
        "--..--" => Some(','),  // comma
        ".-.-.-" => Some('.'),  // period
        "..--.." => Some('?'),  // question mark
        "-..-." => Some('/'),   // slash
        ".-.-." => Some('+'),   // AR prosign / plus
        "-....-" => Some('-'),  // hyphen / minus
        ".----." => Some('\''), // apostrophe
        ".-..-." => Some('"'),  // quotation mark
        "---..." => Some(':'),  // colon
        "-.-.-." => Some(';'),  // semicolon
        "-.--." => Some('('),   // open parenthesis (also KN prosign)
        "-.--.-" => Some(')'),  // close parenthesis
        ".-..." => Some('&'),   // ampersand (also AS / wait prosign)
        ".--.-." => Some('@'),  // at sign
        "..--.-" => Some('_'),  // underscore
        "-.-.--" => Some('!'),  // exclamation
        "...-..-" => Some('$'), // dollar sign
        "...-.-" => Some('<'),  // SK / VA end-of-work prosign
        _ => None,
    }
}

#[cfg(test)]
mod morse_to_char_tests {
    use super::morse_to_char;

    #[test]
    fn alphanumeric_round_trip() {
        assert_eq!(morse_to_char(".-"), Some('A'));
        assert_eq!(morse_to_char("-.-"), Some('K'));
        assert_eq!(morse_to_char("-----"), Some('0'));
        assert_eq!(morse_to_char("....."), Some('5'));
    }

    #[test]
    fn itu_punctuation_decodes() {
        assert_eq!(morse_to_char("-...-"), Some('='));
        assert_eq!(morse_to_char("--..--"), Some(','));
        assert_eq!(morse_to_char(".-.-.-"), Some('.'));
        assert_eq!(morse_to_char("..--.."), Some('?'));
        assert_eq!(morse_to_char("-..-."), Some('/'));
        assert_eq!(morse_to_char(".-.-."), Some('+'));
        assert_eq!(morse_to_char("-....-"), Some('-'));
        assert_eq!(morse_to_char(".----."), Some('\''));
        assert_eq!(morse_to_char("...-.-"), Some('<'));
    }

    #[test]
    fn unknown_morse_returns_none() {
        assert_eq!(morse_to_char("........"), None);
        assert_eq!(morse_to_char("-.-..-.-"), None);
        assert_eq!(morse_to_char(""), None);
    }
}

#[cfg(test)]
mod elem_gate_tests {
    use super::*;

    #[test]
    fn snr_is_high_for_clean_signal_against_quiet_floor() {
        // 2s of quiet background (envelope amplitude ~0.01) with a clean
        // dit (amplitude 1.0) inserted at the middle. The element should
        // score well above any reasonable gate threshold.
        let rate = 100.0_f32;
        let mut sig = vec![0.01_f32; 200];
        for s in &mut sig[100..110] {
            *s = 1.0;
        }
        let intervals = vec![(100usize, 10usize)];
        let snrs = compute_element_snrs_db(&sig, &intervals, rate, 1.5);
        assert_eq!(snrs.len(), 1);
        assert!(
            snrs[0] > 30.0,
            "expected clean dit SNR > 30 dB, got {}",
            snrs[0]
        );
    }

    #[test]
    fn snr_drops_low_for_ghost_against_noisy_background() {
        // Background is a noise burst at amp 0.5 (squared = 0.25). A "ghost"
        // element pokes only slightly above (amp 0.6, squared = 0.36).
        // SNR ~= 10*log10(0.36/0.25) ~= 1.58 dB, which must fall below the
        // default gate threshold.
        let rate = 100.0_f32;
        let mut sig = vec![0.5_f32; 300];
        for s in &mut sig[150..160] {
            *s = 0.6;
        }
        let intervals = vec![(150usize, 10usize)];
        let snrs = compute_element_snrs_db(&sig, &intervals, rate, 1.5);
        assert_eq!(snrs.len(), 1);
        assert!(
            snrs[0] < super::ELEM_GATE_DEFAULT_DB,
            "ghost element SNR ({}) should fall below the {} dB default gate",
            snrs[0],
            super::ELEM_GATE_DEFAULT_DB,
        );
        assert!(
            snrs[0] < 3.0,
            "ghost SNR should be only marginal (<3 dB), got {}",
            snrs[0]
        );
    }

    #[test]
    fn noise_floor_excludes_other_on_intervals() {
        // If we did NOT mask out other on-intervals, a sea of equally-loud
        // ghosts would inflate the noise floor and accidentally save each
        // ghost. Verify that two adjacent equal-amplitude ghosts on a quiet
        // background BOTH score very high (because each ghost's noise
        // estimate excludes the other's samples too).
        let rate = 100.0_f32;
        let mut sig = vec![0.01_f32; 300];
        for s in &mut sig[100..110] {
            *s = 1.0;
        }
        for s in &mut sig[200..210] {
            *s = 1.0;
        }
        let intervals = vec![(100usize, 10usize), (200usize, 10usize)];
        let snrs = compute_element_snrs_db(&sig, &intervals, rate, 1.5);
        assert_eq!(snrs.len(), 2);
        for snr in &snrs {
            assert!(*snr > 30.0, "expected high SNR for both, got {}", snr);
        }
    }

    #[test]
    fn raw_on_intervals_with_positions_round_trip() {
        // Construct a signal with three on-intervals at known positions so
        // we can verify the start/length tuples line up with the legacy
        // length-only `get_raw_intervals` output.
        let mut sig = vec![0.0_f32; 100];
        for s in &mut sig[10..15] {
            *s = 1.0;
        }
        for s in &mut sig[30..40] {
            *s = 1.0;
        }
        for s in &mut sig[70..72] {
            *s = 1.0;
        }
        let with_pos = get_raw_on_intervals_with_positions(&sig, 0.5);
        assert_eq!(with_pos, vec![(10, 5), (30, 10), (70, 2)]);
        let (lengths, _) = get_raw_intervals(&sig, 0.5);
        let from_pos: Vec<usize> = with_pos.iter().map(|(_, l)| *l).collect();
        assert_eq!(lengths, from_pos);
    }
}

#[cfg(test)]
mod soft_decoder_tests {
    use super::{merge_short_blips, soft_decode_intervals};

    /// Build interleaved (is_on, len) intervals for a clean morse pattern.
    /// `pattern` is a string of `.` (dot), `-` (dash), `|` (letter gap),
    /// and ` ` (word gap). Element gaps are inserted automatically between
    /// elements that belong to the same letter.
    fn build_intervals(pattern: &str, dot_len: usize) -> Vec<(bool, usize)> {
        let dash = 3 * dot_len;
        let letter_gap = 3 * dot_len;
        let word_gap = 7 * dot_len;
        let mut out: Vec<(bool, usize)> = Vec::new();
        let mut prev_was_element = false;
        for ch in pattern.chars() {
            match ch {
                '.' | '-' => {
                    if prev_was_element {
                        out.push((false, dot_len));
                    }
                    let on_len = if ch == '.' { dot_len } else { dash };
                    out.push((true, on_len));
                    prev_was_element = true;
                }
                '|' => {
                    out.push((false, letter_gap));
                    prev_was_element = false;
                }
                ' ' => {
                    out.push((false, word_gap));
                    prev_was_element = false;
                }
                _ => panic!("bad pattern char: {ch}"),
            }
        }
        out
    }

    #[test]
    fn clean_dots_decode_to_eeee() {
        let dot_len = 30usize;
        let intervals = build_intervals(".|.|.|.", dot_len);
        let text = soft_decode_intervals(&intervals, dot_len as f32);
        assert_eq!(text, "EEEE");
    }

    #[test]
    fn clean_dashes_decode_to_tttt() {
        let dot_len = 30usize;
        let intervals = build_intervals("-|-|-|-", dot_len);
        let text = soft_decode_intervals(&intervals, dot_len as f32);
        assert_eq!(text, "TTTT");
    }

    #[test]
    fn clean_word_decodes() {
        let dot_len = 30usize;
        // "CQ DE" = -.-. --.- / -.. .
        let intervals = build_intervals("-.-.|--.- -..|.", dot_len);
        let text = soft_decode_intervals(&intervals, dot_len as f32);
        assert_eq!(text, "CQ DE");
    }

    #[test]
    fn ghost_single_element_between_letters_is_suppressed() {
        // Real word "AN" = .-|-. with a noise spike of ~0.4*dot_len in the
        // middle of the letter gap. The noise spike survives interval
        // collection (it's above the merge_short_blips debounce) but is too
        // short to score as a confident dot, so the single-element ghost it
        // would have produced should be dropped by the emit-floor check.
        let dot_len: usize = 40;
        let spike = (dot_len as f32 * 0.40) as usize; // noise blip
        let intervals = vec![
            (true, dot_len),      // .
            (false, dot_len),     // elem gap
            (true, 3 * dot_len),  // -
            (false, 3 * dot_len), // letter gap
            (true, spike),        // <-- noise spike; should be suppressed
            (false, 3 * dot_len), // letter gap
            (true, 3 * dot_len),  // -
            (false, dot_len),     // elem gap
            (true, dot_len),      // .
        ];
        let text = soft_decode_intervals(&intervals, dot_len as f32);
        // Without ghost suppression we'd see "AEN"; with it we get "AN".
        assert_eq!(text, "AN", "got {text}");
    }

    #[test]
    fn merge_collapses_blip_inside_long_on() {
        // A 1-sample dropout in the middle of a long key-down should be
        // absorbed so the dash is not split into two dots.
        let intervals = vec![(true, 100usize), (false, 1), (true, 100)];
        let merged = merge_short_blips(&intervals, 10);
        assert_eq!(merged, vec![(true, 201)]);
    }

    #[test]
    fn merge_collapses_blip_inside_long_off() {
        let intervals = vec![(false, 100usize), (true, 1), (false, 100)];
        let merged = merge_short_blips(&intervals, 10);
        assert_eq!(merged, vec![(false, 201)]);
    }
}

#[cfg(test)]
mod end_to_end_synth_tests {
    use crate::decode_samples;

    fn morse_for(ch: char) -> Option<&'static str> {
        Some(match ch {
            'A' => ".-",
            'C' => "-.-.",
            'D' => "-..",
            'E' => ".",
            'Q' => "--.-",
            'W' => ".--",
            '1' => ".----",
            _ => return None,
        })
    }

    /// Render a morse text to in-memory samples at the given WPM and pitch,
    /// optionally adding deterministic pseudo-random noise scaled by
    /// `noise_amp` (relative to the +/-1.0 signal envelope). Returns
    /// (samples, sample_rate).
    fn synth_samples(text: &str, wpm: f32, pitch_hz: f32, noise_amp: f32) -> (Vec<f32>, u32) {
        use std::f32::consts::PI;
        let sample_rate: u32 = 12000;
        let dot_s = 1.2 / wpm;
        let dash_s = 3.0 * dot_s;
        let elem_gap_s = dot_s;
        let letter_gap_s = 3.0 * dot_s;
        let word_gap_s = 7.0 * dot_s;
        let mut samples: Vec<f32> = Vec::new();
        let lead_silence = (0.5 * sample_rate as f32) as usize;
        samples.extend(std::iter::repeat_n(0.0, lead_silence));

        let mut phase = 0.0f32;
        let phase_step = 2.0 * PI * pitch_hz / sample_rate as f32;
        let emit_tone = |samples: &mut Vec<f32>, dur_s: f32, phase: &mut f32| {
            let n = (dur_s * sample_rate as f32) as usize;
            for _ in 0..n {
                samples.push(0.5 * phase.sin());
                *phase += phase_step;
            }
        };
        let emit_silence = |samples: &mut Vec<f32>, dur_s: f32| {
            let n = (dur_s * sample_rate as f32) as usize;
            samples.extend(std::iter::repeat_n(0.0, n));
        };

        let words: Vec<&str> = text.split_whitespace().collect();
        for (wi, word) in words.iter().enumerate() {
            let chars: Vec<char> = word.chars().collect();
            for (ci, ch) in chars.iter().enumerate() {
                let morse = morse_for(ch.to_ascii_uppercase()).expect("unsupported char");
                for (ei, el) in morse.chars().enumerate() {
                    if ei > 0 {
                        emit_silence(&mut samples, elem_gap_s);
                    }
                    let dur = if el == '.' { dot_s } else { dash_s };
                    emit_tone(&mut samples, dur, &mut phase);
                }
                if ci + 1 < chars.len() {
                    emit_silence(&mut samples, letter_gap_s);
                }
            }
            if wi + 1 < words.len() {
                emit_silence(&mut samples, word_gap_s);
            }
        }
        emit_silence(&mut samples, 0.5);

        if noise_amp > 0.0 {
            // Cheap deterministic xorshift32 PRNG (no extra dependency).
            let mut state: u32 = 0xC0FFEE_u32;
            for s in samples.iter_mut() {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                let f = (state as f32 / u32::MAX as f32) * 2.0 - 1.0;
                *s += noise_amp * f;
            }
        }

        (samples, sample_rate)
    }

    #[test]
    fn synth_cq_de_w1aw_at_18wpm_with_low_noise() {
        let (samples, sr) = synth_samples("CQ DE W1AW", 18.0, 700.0, 0.05);
        let decoded = decode_samples(&samples, sr).unwrap_or_default();
        let normalized: String = decoded
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == ' ')
            .collect::<String>()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        assert!(
            normalized.contains("CQ") && normalized.contains("DE") && normalized.contains("W1AW"),
            "expected CQ DE W1AW in {normalized:?}"
        );
        let single_char_words = normalized
            .split_whitespace()
            .filter(|w| w.chars().count() == 1)
            .count();
        assert!(
            single_char_words <= 1,
            "too many ghost single-char words in {normalized:?}"
        );
    }
}
