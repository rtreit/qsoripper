//! Audio sources: file decoding (via Symphonia) and live capture (via cpal).

use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex as StdMutex;

use anyhow::{anyhow, Context, Result};
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::DecoderOptions;
use symphonia::core::errors::Error as SymError;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

pub struct DecodedAudio {
    /// Mono mix used by the CW decoder.
    pub samples: Vec<f32>,
    /// Original interleaved samples used by playback/preview paths that must
    /// preserve what the operator recorded.
    pub interleaved_samples: Vec<f32>,
    pub sample_rate: u32,
    pub channels: usize,
}

pub struct FilePlayback {
    pub sample_rate: u32,
    pub device_name: String,
    pub duration_s: f32,
    position_frames: Arc<AtomicU64>,
    total_frames: u64,
    finished: Arc<AtomicBool>,
    _stream: cpal::Stream,
}

impl FilePlayback {
    pub fn position_s(&self) -> f32 {
        let frames = self
            .position_frames
            .load(Ordering::Relaxed)
            .min(self.total_frames);
        frames as f32 / self.sample_rate as f32
    }

    pub fn is_finished(&self) -> bool {
        self.finished.load(Ordering::Relaxed)
            || self.position_frames.load(Ordering::Relaxed) >= self.total_frames
    }
}

/// Decode an audio file (mp3/wav/aac/m4a/etc) into a mono f32 PCM buffer.
pub fn decode_file(path: &Path) -> Result<DecodedAudio> {
    match decode_file_with_symphonia(path) {
        Ok(audio) => Ok(audio),
        Err(primary_err) => {
            if let Some(audio) = decode_unfinalized_qsoripper_wav(path)
                .context("decoding unfinalized QsoRipper WAV fallback")?
            {
                return Ok(audio);
            }
            Err(primary_err)
        }
    }
}

fn decode_file_with_symphonia(path: &Path) -> Result<DecodedAudio> {
    let file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());

    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|s| s.to_str()) {
        hint.with_extension(ext);
    }

    let probed = symphonia::default::get_probe()
        .format(
            &hint,
            mss,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )
        .context("probing audio format")?;
    let mut format = probed.format;

    let track = format
        .default_track()
        .ok_or_else(|| anyhow!("no default audio track"))?;
    let track_id = track.id;
    let codec_params = track.codec_params.clone();

    let mut decoder = symphonia::default::get_codecs()
        .make(&codec_params, &DecoderOptions::default())
        .context("creating decoder")?;

    let sample_rate = codec_params
        .sample_rate
        .ok_or_else(|| anyhow!("unknown sample rate"))?;
    let channels = codec_params
        .channels
        .ok_or_else(|| anyhow!("unknown channel layout"))?
        .count();

    let mut samples: Vec<f32> = Vec::new();
    let mut interleaved_samples: Vec<f32> = Vec::new();
    let mut sample_buf: Option<SampleBuffer<f32>> = None;

    loop {
        let packet = match format.next_packet() {
            Ok(p) => p,
            Err(SymError::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(SymError::ResetRequired) => break,
            Err(e) => return Err(e.into()),
        };
        if packet.track_id() != track_id {
            continue;
        }
        match decoder.decode(&packet) {
            Ok(audio_buf) => {
                if sample_buf.is_none() {
                    let spec = *audio_buf.spec();
                    let duration = audio_buf.capacity() as u64;
                    sample_buf = Some(SampleBuffer::<f32>::new(duration, spec));
                }
                if let Some(buf) = sample_buf.as_mut() {
                    buf.copy_interleaved_ref(audio_buf);
                    let interleaved = buf.samples();
                    interleaved_samples.extend_from_slice(interleaved);
                    if channels == 1 {
                        samples.extend_from_slice(interleaved);
                    } else {
                        for frame in interleaved.chunks_exact(channels) {
                            let avg = frame.iter().copied().sum::<f32>() / channels as f32;
                            samples.push(avg);
                        }
                    }
                }
            }
            Err(SymError::DecodeError(_)) => continue,
            Err(SymError::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(e.into()),
        }
    }

    Ok(DecodedAudio {
        samples,
        interleaved_samples,
        sample_rate,
        channels,
    })
}

fn decode_unfinalized_qsoripper_wav(path: &Path) -> Result<Option<DecodedAudio>> {
    if path.extension().and_then(|s| s.to_str()) != Some("wav") {
        return Ok(None);
    }

    let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    if bytes.len() <= 44 {
        return Ok(None);
    }

    if &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Ok(None);
    }

    let riff_size = u32::from_le_bytes(bytes[4..8].try_into().expect("slice length checked"));
    let mut audio_format = 0_u16;
    let mut channels = 0_u16;
    let mut sample_rate = 0_u32;
    let mut block_align = 0_u16;
    let mut bits_per_sample = 0_u16;
    let mut fmt_size = 0_u32;
    let mut data_start = 0_usize;
    let mut data_size = 0_u32;

    let mut cursor = 12_usize;
    while cursor + 8 <= bytes.len() {
        let id = &bytes[cursor..cursor + 4];
        let size = u32::from_le_bytes(
            bytes[cursor + 4..cursor + 8]
                .try_into()
                .expect("slice length checked"),
        );
        let chunk_start = cursor + 8;
        let chunk_end = chunk_start.saturating_add(size as usize).min(bytes.len());
        if id == b"fmt " {
            if size < 16 || chunk_start + 16 > bytes.len() {
                return Ok(None);
            }
            fmt_size = size;
            audio_format = u16::from_le_bytes(
                bytes[chunk_start..chunk_start + 2]
                    .try_into()
                    .expect("slice length checked"),
            );
            channels = u16::from_le_bytes(
                bytes[chunk_start + 2..chunk_start + 4]
                    .try_into()
                    .expect("slice length checked"),
            );
            sample_rate = u32::from_le_bytes(
                bytes[chunk_start + 4..chunk_start + 8]
                    .try_into()
                    .expect("slice length checked"),
            );
            block_align = u16::from_le_bytes(
                bytes[chunk_start + 12..chunk_start + 14]
                    .try_into()
                    .expect("slice length checked"),
            );
            bits_per_sample = u16::from_le_bytes(
                bytes[chunk_start + 14..chunk_start + 16]
                    .try_into()
                    .expect("slice length checked"),
            );
            if audio_format == 0xFFFE && size >= 40 && chunk_start + 40 <= bytes.len() {
                audio_format = u16::from_le_bytes(
                    bytes[chunk_start + 24..chunk_start + 26]
                        .try_into()
                        .expect("slice length checked"),
                );
            }
        } else if id == b"data" {
            data_start = chunk_start;
            data_size = size;
            break;
        }

        cursor = chunk_end + (size as usize & 1);
    }

    let looks_unfinalized = riff_size == 0 && data_size == 0;
    if !looks_unfinalized {
        return Ok(None);
    }
    if fmt_size < 16
        || channels == 0
        || block_align == 0
        || data_start == 0
        || !matches!((audio_format, bits_per_sample), (1, 16) | (3, 32))
    {
        return Err(anyhow!(
            "unsupported unfinalized WAV format: fmt_size={fmt_size}, format={audio_format}, channels={channels}, bits={bits_per_sample}"
        ));
    }

    let channels = channels as usize;
    let data = &bytes[data_start..];
    let mut samples = Vec::with_capacity(data.len() / block_align as usize);
    let mut interleaved_samples = Vec::with_capacity(data.len() / (bits_per_sample as usize / 8));
    for frame in data.chunks_exact(block_align as usize) {
        let mut sum = 0.0_f32;
        for sample in frame
            .chunks_exact(bits_per_sample as usize / 8)
            .take(channels)
        {
            let value = match (audio_format, bits_per_sample) {
                (1, 16) => {
                    let pcm = i16::from_le_bytes([sample[0], sample[1]]);
                    f32::from(pcm) / f32::from(i16::MAX)
                }
                (3, 32) => f32::from_le_bytes([sample[0], sample[1], sample[2], sample[3]]),
                _ => unreachable!("format checked above"),
            };
            interleaved_samples.push(value);
            sum += value;
        }
        samples.push(sum / channels as f32);
    }

    Ok(Some(DecodedAudio {
        samples,
        interleaved_samples,
        sample_rate,
        channels,
    }))
}

pub fn play_output_file(path: &Path) -> Result<FilePlayback> {
    let audio = decode_file(path)?;
    if audio.samples.is_empty() {
        return Err(anyhow!("decoded audio was empty"));
    }

    let host = cpal::default_host();
    let device = host
        .default_output_device()
        .ok_or_else(|| anyhow!("no default output device"))?;
    let device_name = device.name().unwrap_or_else(|_| "<unknown>".to_string());
    let supported = device.default_output_config().context("output config")?;
    let sample_format = supported.sample_format();
    let stream_config: cpal::StreamConfig = supported.into();
    let output_rate = stream_config.sample_rate.0;
    let channels = stream_config.channels as usize;

    let mono = if audio.sample_rate == output_rate {
        audio.samples
    } else {
        resample_linear(&audio.samples, audio.sample_rate, output_rate)
    };

    let total_frames = mono.len() as u64;
    let duration_s = total_frames as f32 / output_rate as f32;
    let samples = Arc::new(mono);
    let position_frames = Arc::new(AtomicU64::new(0));
    let finished = Arc::new(AtomicBool::new(false));

    let err_fn = |e| eprintln!("cpal output stream error: {e}");
    let stream = match sample_format {
        cpal::SampleFormat::F32 => {
            let samples = Arc::clone(&samples);
            let position_frames = Arc::clone(&position_frames);
            let finished = Arc::clone(&finished);
            device.build_output_stream(
                &stream_config,
                move |data: &mut [f32], _| {
                    fill_output(
                        data,
                        channels,
                        &samples,
                        &position_frames,
                        &finished,
                        |sample| sample,
                    );
                },
                err_fn,
                None,
            )?
        }
        cpal::SampleFormat::I16 => {
            let samples = Arc::clone(&samples);
            let position_frames = Arc::clone(&position_frames);
            let finished = Arc::clone(&finished);
            device.build_output_stream(
                &stream_config,
                move |data: &mut [i16], _| {
                    fill_output(
                        data,
                        channels,
                        &samples,
                        &position_frames,
                        &finished,
                        |sample| (sample.clamp(-1.0, 1.0) * i16::MAX as f32) as i16,
                    );
                },
                err_fn,
                None,
            )?
        }
        cpal::SampleFormat::U16 => {
            let samples = Arc::clone(&samples);
            let position_frames = Arc::clone(&position_frames);
            let finished = Arc::clone(&finished);
            device.build_output_stream(
                &stream_config,
                move |data: &mut [u16], _| {
                    fill_output(
                        data,
                        channels,
                        &samples,
                        &position_frames,
                        &finished,
                        |sample| ((sample.clamp(-1.0, 1.0) * 0.5 + 0.5) * u16::MAX as f32) as u16,
                    );
                },
                err_fn,
                None,
            )?
        }
        other => return Err(anyhow!("unsupported output sample format: {other:?}")),
    };
    stream.play().context("starting output stream")?;

    Ok(FilePlayback {
        sample_rate: output_rate,
        device_name,
        duration_s,
        position_frames,
        total_frames,
        finished,
        _stream: stream,
    })
}

/// Controllable playback handle used by `decode-and-play`. The audio
/// callback is the master clock; everything else (decoder pump, GUI
/// scrubber, region trim) reads from this handle. Position is reported
/// in *input-rate frames* even though the cpal stream may run at a
/// different output rate, so a caller can map a played frame index 1:1
/// onto the original input sample buffer.
pub struct ControllablePlayback {
    /// Sample rate of the original input buffer (the value the caller
    /// passed to `play_samples_with_control`).
    pub input_rate: u32,
    /// Sample rate of the cpal output stream (may differ from input
    /// when the device's mixer rate forces a resample).
    pub output_rate: u32,
    pub device_name: String,
    /// Duration of the input buffer, in seconds.
    pub duration_s: f32,

    output_position_frames: Arc<AtomicU64>,
    total_output_frames: u64,
    paused: Arc<AtomicBool>,
    finished: Arc<AtomicBool>,
    /// `u64::MAX` = no pending seek. Otherwise the audio callback will
    /// jump its read cursor to this output-rate frame on the next pull.
    seek_target: Arc<AtomicU64>,
    /// Bumped every time a seek is acknowledged by the callback so the
    /// pump can detect "I need to reset the decoder" without racing.
    seek_epoch: Arc<AtomicU64>,
    _stream: cpal::Stream,
}

impl ControllablePlayback {
    /// Current playback position expressed in *input-rate frames*.
    pub fn position_input_frames(&self) -> u64 {
        let out_pos = self
            .output_position_frames
            .load(Ordering::Relaxed)
            .min(self.total_output_frames);
        // out_pos / output_rate = seconds; * input_rate = input frames.
        ((out_pos as u128 * self.input_rate as u128) / self.output_rate as u128) as u64
    }

    pub fn position_seconds(&self) -> f32 {
        let out_pos = self
            .output_position_frames
            .load(Ordering::Relaxed)
            .min(self.total_output_frames);
        out_pos as f32 / self.output_rate as f32
    }

    pub fn is_finished(&self) -> bool {
        self.finished.load(Ordering::Relaxed)
            || self.output_position_frames.load(Ordering::Relaxed) >= self.total_output_frames
    }

    pub fn is_paused(&self) -> bool {
        self.paused.load(Ordering::Relaxed)
    }

    pub fn pause(&self) {
        self.paused.store(true, Ordering::Relaxed);
    }

    pub fn resume(&self) {
        self.paused.store(false, Ordering::Relaxed);
    }

    /// Seek to a time in seconds, relative to the start of the input
    /// buffer (which already accounts for any region trim applied by
    /// the caller). Returns the seek epoch that the pump should wait
    /// to observe before resuming decoder feeds.
    pub fn seek_to_seconds(&self, seconds: f32) -> u64 {
        let clamped = seconds.max(0.0);
        let out_target = ((clamped * self.output_rate as f32) as u64).min(self.total_output_frames);
        // Use SeqCst so the pump observes the target write before it
        // observes the epoch bump that signals the seek landed.
        self.seek_target.store(out_target, Ordering::SeqCst);
        // Don't bump epoch here — the callback bumps it after applying.
        self.seek_epoch.load(Ordering::Relaxed)
    }

    pub fn seek_epoch(&self) -> u64 {
        self.seek_epoch.load(Ordering::Relaxed)
    }
}

/// Open the default output device and start playing pre-decoded mono
/// samples. The returned handle exposes pause/resume/seek and reports
/// position in input-rate frames so callers can drive a streaming
/// decoder in lockstep with what is audible.
pub fn play_samples_with_control(
    samples: Vec<f32>,
    input_rate: u32,
) -> Result<ControllablePlayback> {
    play_interleaved_samples_with_control(samples, input_rate, 1)
}

pub fn play_interleaved_samples_with_control(
    samples: Vec<f32>,
    input_rate: u32,
    input_channels: usize,
) -> Result<ControllablePlayback> {
    if samples.is_empty() {
        return Err(anyhow!("input sample buffer was empty"));
    }
    if input_rate == 0 {
        return Err(anyhow!("input sample rate must be non-zero"));
    }
    if input_channels == 0 || samples.len() % input_channels != 0 {
        return Err(anyhow!(
            "interleaved sample buffer length {} is not aligned to {input_channels} channel(s)",
            samples.len()
        ));
    }

    let host = cpal::default_host();
    let device = host
        .default_output_device()
        .ok_or_else(|| anyhow!("no default output device"))?;
    let device_name = device.name().unwrap_or_else(|_| "<unknown>".to_string());
    let supported = device.default_output_config().context("output config")?;
    let sample_format = supported.sample_format();
    let stream_config: cpal::StreamConfig = supported.into();
    let output_rate = stream_config.sample_rate.0;
    let channels = stream_config.channels as usize;

    let samples_for_output: Vec<f32> = if input_rate == output_rate {
        samples
    } else {
        resample_linear_interleaved(&samples, input_channels, input_rate, output_rate)
    };

    let total_output_frames = (samples_for_output.len() / input_channels) as u64;
    let duration_s = total_output_frames as f32 / output_rate as f32;
    let buf = Arc::new(samples_for_output);
    let output_position_frames = Arc::new(AtomicU64::new(0));
    let paused = Arc::new(AtomicBool::new(false));
    let finished = Arc::new(AtomicBool::new(false));
    // u64::MAX sentinel = no pending seek.
    let seek_target = Arc::new(AtomicU64::new(u64::MAX));
    let seek_epoch = Arc::new(AtomicU64::new(0));

    let err_fn = |e| eprintln!("cpal output stream error: {e}");
    let stream = match sample_format {
        cpal::SampleFormat::F32 => {
            let buf = Arc::clone(&buf);
            let pos = Arc::clone(&output_position_frames);
            let paused = Arc::clone(&paused);
            let finished = Arc::clone(&finished);
            let seek_target = Arc::clone(&seek_target);
            let seek_epoch = Arc::clone(&seek_epoch);
            device.build_output_stream(
                &stream_config,
                move |data: &mut [f32], _| {
                    fill_controlled(
                        data,
                        channels,
                        &buf,
                        input_channels,
                        &pos,
                        &paused,
                        &finished,
                        &seek_target,
                        &seek_epoch,
                        |s| s,
                    );
                },
                err_fn,
                None,
            )?
        }
        cpal::SampleFormat::I16 => {
            let buf = Arc::clone(&buf);
            let pos = Arc::clone(&output_position_frames);
            let paused = Arc::clone(&paused);
            let finished = Arc::clone(&finished);
            let seek_target = Arc::clone(&seek_target);
            let seek_epoch = Arc::clone(&seek_epoch);
            device.build_output_stream(
                &stream_config,
                move |data: &mut [i16], _| {
                    fill_controlled(
                        data,
                        channels,
                        &buf,
                        input_channels,
                        &pos,
                        &paused,
                        &finished,
                        &seek_target,
                        &seek_epoch,
                        |s| (s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16,
                    );
                },
                err_fn,
                None,
            )?
        }
        cpal::SampleFormat::U16 => {
            let buf = Arc::clone(&buf);
            let pos = Arc::clone(&output_position_frames);
            let paused = Arc::clone(&paused);
            let finished = Arc::clone(&finished);
            let seek_target = Arc::clone(&seek_target);
            let seek_epoch = Arc::clone(&seek_epoch);
            device.build_output_stream(
                &stream_config,
                move |data: &mut [u16], _| {
                    fill_controlled(
                        data,
                        channels,
                        &buf,
                        input_channels,
                        &pos,
                        &paused,
                        &finished,
                        &seek_target,
                        &seek_epoch,
                        |s| ((s.clamp(-1.0, 1.0) * 0.5 + 0.5) * u16::MAX as f32) as u16,
                    );
                },
                err_fn,
                None,
            )?
        }
        other => return Err(anyhow!("unsupported output sample format: {other:?}")),
    };
    stream.play().context("starting output stream")?;

    Ok(ControllablePlayback {
        input_rate,
        output_rate,
        device_name,
        duration_s,
        output_position_frames,
        total_output_frames,
        paused,
        finished,
        seek_target,
        seek_epoch,
        _stream: stream,
    })
}

#[allow(clippy::too_many_arguments)]
fn fill_controlled<T, F>(
    data: &mut [T],
    channels: usize,
    samples: &Arc<Vec<f32>>,
    input_channels: usize,
    position_frames: &Arc<AtomicU64>,
    paused: &Arc<AtomicBool>,
    finished: &Arc<AtomicBool>,
    seek_target: &Arc<AtomicU64>,
    seek_epoch: &Arc<AtomicU64>,
    mut convert: F,
) where
    T: cpal::Sample,
    F: FnMut(f32) -> T,
{
    // Apply any pending seek before reading the position. This lets the
    // pump observe the new position the next tick after the audio has
    // physically jumped.
    let pending = seek_target.swap(u64::MAX, Ordering::SeqCst);
    if pending != u64::MAX {
        position_frames.store(pending, Ordering::SeqCst);
        seek_epoch.fetch_add(1, Ordering::SeqCst);
        finished.store(false, Ordering::Relaxed);
    }

    let total = samples.len() / input_channels;
    let mut frame_index = position_frames.load(Ordering::Relaxed) as usize;
    let is_paused = paused.load(Ordering::Relaxed);

    for frame in data.chunks_mut(channels) {
        if !is_paused && frame_index >= total {
            finished.store(true, Ordering::Relaxed);
        }
        for (out_channel, out) in frame.iter_mut().enumerate() {
            let sample = if is_paused || frame_index >= total {
                0.0
            } else {
                sample_for_output_channel(
                    samples,
                    frame_index,
                    input_channels,
                    out_channel,
                    channels,
                )
            };
            *out = convert(sample);
        }
        if !is_paused && frame_index < total {
            frame_index += 1;
        }
    }

    if !is_paused {
        position_frames.store(frame_index.min(total) as u64, Ordering::Relaxed);
        if frame_index >= total {
            finished.store(true, Ordering::Relaxed);
        }
    }
}

fn sample_for_output_channel(
    samples: &[f32],
    frame_index: usize,
    input_channels: usize,
    output_channel: usize,
    output_channels: usize,
) -> f32 {
    let base = frame_index * input_channels;
    if input_channels == output_channels || input_channels == 1 {
        samples[base + output_channel.min(input_channels - 1)]
    } else if output_channels == 1 {
        samples[base..base + input_channels]
            .iter()
            .copied()
            .sum::<f32>()
            / input_channels as f32
    } else {
        samples[base + output_channel.min(input_channels - 1)]
    }
}

fn fill_output<T, F>(
    data: &mut [T],
    channels: usize,
    samples: &Arc<Vec<f32>>,
    position_frames: &Arc<AtomicU64>,
    finished: &Arc<AtomicBool>,
    mut convert: F,
) where
    F: FnMut(f32) -> T,
{
    let mut frame_index = position_frames.load(Ordering::Relaxed) as usize;
    let total_frames = samples.len();

    for frame in data.chunks_mut(channels) {
        let sample = if frame_index < total_frames {
            let value = samples[frame_index];
            frame_index += 1;
            value
        } else {
            finished.store(true, Ordering::Relaxed);
            0.0
        };

        for out in frame {
            *out = convert(sample);
        }
    }

    position_frames.store(frame_index.min(total_frames) as u64, Ordering::Relaxed);
    if frame_index >= total_frames {
        finished.store(true, Ordering::Relaxed);
    }
}

fn resample_linear(samples: &[f32], input_rate: u32, output_rate: u32) -> Vec<f32> {
    if samples.is_empty() || input_rate == output_rate {
        return samples.to_vec();
    }

    let out_len = ((samples.len() as f64) * output_rate as f64 / input_rate as f64)
        .round()
        .max(1.0) as usize;
    let mut out = Vec::with_capacity(out_len);
    let last = samples.len() - 1;

    for index in 0..out_len {
        let source_pos = index as f64 * input_rate as f64 / output_rate as f64;
        let left = source_pos.floor() as usize;
        let right = (left + 1).min(last);
        let frac = (source_pos - left as f64) as f32;
        let left_sample = samples[left];
        let right_sample = samples[right];
        out.push(left_sample + (right_sample - left_sample) * frac);
    }

    out
}

fn resample_linear_interleaved(
    samples: &[f32],
    channels: usize,
    input_rate: u32,
    output_rate: u32,
) -> Vec<f32> {
    if samples.is_empty() || input_rate == output_rate {
        return samples.to_vec();
    }

    let input_frames = samples.len() / channels;
    let out_frames = ((input_frames as f64) * output_rate as f64 / input_rate as f64)
        .round()
        .max(1.0) as usize;
    let mut out = Vec::with_capacity(out_frames * channels);
    let last_frame = input_frames - 1;

    for frame_index in 0..out_frames {
        let source_pos = frame_index as f64 * input_rate as f64 / output_rate as f64;
        let left = (source_pos.floor() as usize).min(last_frame);
        let right = (left + 1).min(last_frame);
        let frac = (source_pos - left as f64) as f32;
        let left_base = left * channels;
        let right_base = right * channels;
        for channel in 0..channels {
            let left_sample = samples[left_base + channel];
            let right_sample = samples[right_base + channel];
            out.push(left_sample + (right_sample - left_sample) * frac);
        }
    }

    out
}

// --- Live capture --------------------------------------------------------

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use parking_lot::Mutex;
use std::sync::Arc;

pub struct LiveCapture {
    pub sample_rate: u32,
    pub device_name: String,
    /// Rolling ring buffer of the most recent N seconds of mono f32 samples.
    pub buffer: Arc<Mutex<RingBuffer>>,
    _stream: cpal::Stream,
    recorder: Option<RecorderHandle>,
    record_path: Option<PathBuf>,
}

/// Shared, lockable WAV recorder handle. Wrapped in an alias so we don't
/// drag the full `Option<Arc<Mutex<Option<WavWriter<BufWriter<File>>>>>>`
/// soup through every signature.
pub(crate) type RecorderHandle = Arc<StdMutex<Option<hound::WavWriter<std::io::BufWriter<File>>>>>;

impl LiveCapture {
    /// Path the recording is being written to (if any).
    pub fn record_path(&self) -> Option<&Path> {
        self.record_path.as_deref()
    }

    /// Flushes and closes the recording. Idempotent. Returns the WAV path on
    /// first close, `None` otherwise. Called automatically on drop.
    pub fn finalize_recording(&self) -> Option<PathBuf> {
        let recorder = self.recorder.as_ref()?;
        let mut guard = recorder.lock().ok()?;
        let writer = guard.take()?;
        // best-effort flush+close; ignore IO errors
        let _ = writer.finalize();
        self.record_path.clone()
    }
}

impl Drop for LiveCapture {
    fn drop(&mut self) {
        let _ = self.finalize_recording();
    }
}

pub struct RingBuffer {
    capacity: usize,
    data: Vec<f32>,
    /// Total samples ever written; useful for "have we got fresh data" checks.
    pub written: u64,
}

impl RingBuffer {
    fn new(capacity: usize) -> Self {
        Self {
            capacity,
            data: Vec::with_capacity(capacity),
            written: 0,
        }
    }

    fn push_slice(&mut self, samples: &[f32]) {
        self.written = self.written.saturating_add(samples.len() as u64);
        if samples.len() >= self.capacity {
            let start = samples.len() - self.capacity;
            self.data.clear();
            self.data.extend_from_slice(&samples[start..]);
            return;
        }
        let total = self.data.len() + samples.len();
        if total > self.capacity {
            let drop = total - self.capacity;
            self.data.drain(..drop);
        }
        self.data.extend_from_slice(samples);
    }

    pub fn snapshot(&self) -> Vec<f32> {
        self.data.clone()
    }

    pub fn len(&self) -> usize {
        self.data.len()
    }

    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }
}

/// Find an input device whose name contains `query` (case-insensitive). When
/// `query` is `None`, the host default input is used.
pub fn open_input(query: Option<&str>, window_seconds: f32) -> Result<LiveCapture> {
    open_input_with_recording(query, window_seconds, None)
}

/// Same as [`open_input`] but additionally writes the captured interleaved
/// channels to a 32-bit float WAV file at the device's native sample rate.
/// The decoder still receives a mono mix, but the recording preserves the
/// source stream for labeling and preview.
pub fn open_input_with_recording(
    query: Option<&str>,
    window_seconds: f32,
    record_to: Option<&Path>,
) -> Result<LiveCapture> {
    let host = cpal::default_host();

    let device = if let Some(q) = query {
        let q_lower = q.to_lowercase();
        host.input_devices()?
            .find(|d| {
                d.name()
                    .map(|n| n.to_lowercase().contains(&q_lower))
                    .unwrap_or(false)
            })
            .ok_or_else(|| anyhow!("no input device matching {q:?}"))?
    } else {
        host.default_input_device()
            .ok_or_else(|| anyhow!("no default input device"))?
    };

    let device_name = device.name().unwrap_or_else(|_| "<unknown>".to_string());
    let config = device.default_input_config().context("input config")?;
    let sample_rate = config.sample_rate().0;
    let channels = config.channels() as usize;
    let capacity = ((sample_rate as f32 * window_seconds) as usize).max(1);
    let buffer = Arc::new(Mutex::new(RingBuffer::new(capacity)));

    let (recorder, record_path) = if let Some(path) = record_to {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).ok();
            }
        }
        let spec = hound::WavSpec {
            channels: channels as u16,
            sample_rate,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        };
        let writer = hound::WavWriter::create(path, spec)
            .with_context(|| format!("creating WAV file {}", path.display()))?;
        (
            Some(Arc::new(StdMutex::new(Some(writer)))),
            Some(path.to_path_buf()),
        )
    } else {
        (None, None)
    };

    let err_fn = |e| eprintln!("cpal stream error: {e}");
    let buffer_cb = Arc::clone(&buffer);
    let recorder_cb = recorder.clone();

    let stream = match config.sample_format() {
        cpal::SampleFormat::F32 => device.build_input_stream(
            &config.into(),
            move |data: &[f32], _| {
                push_mono(&buffer_cb, data, channels, recorder_cb.as_ref());
            },
            err_fn,
            None,
        )?,
        cpal::SampleFormat::I16 => device.build_input_stream(
            &config.into(),
            move |data: &[i16], _| {
                let f: Vec<f32> = data.iter().map(|s| *s as f32 / i16::MAX as f32).collect();
                push_mono(&buffer_cb, &f, channels, recorder_cb.as_ref());
            },
            err_fn,
            None,
        )?,
        cpal::SampleFormat::U16 => device.build_input_stream(
            &config.into(),
            move |data: &[u16], _| {
                let f: Vec<f32> = data
                    .iter()
                    .map(|s| (*s as f32 - 32768.0) / 32768.0)
                    .collect();
                push_mono(&buffer_cb, &f, channels, recorder_cb.as_ref());
            },
            err_fn,
            None,
        )?,
        other => return Err(anyhow!("unsupported sample format: {other:?}")),
    };
    stream.play().context("starting input stream")?;

    Ok(LiveCapture {
        sample_rate,
        device_name,
        buffer,
        _stream: stream,
        recorder,
        record_path,
    })
}

fn push_mono(
    buf: &Arc<Mutex<RingBuffer>>,
    data: &[f32],
    channels: usize,
    recorder: Option<&RecorderHandle>,
) {
    // Compute the mono buffer once for the decoder while recording preserves
    // the original interleaved channel samples.
    let mono: Vec<f32> = if channels == 1 {
        data.to_vec()
    } else {
        let mut m = Vec::with_capacity(data.len() / channels);
        for frame in data.chunks_exact(channels) {
            let avg = frame.iter().copied().sum::<f32>() / channels as f32;
            m.push(avg);
        }
        m
    };
    {
        let mut lock = buf.lock();
        lock.push_slice(&mono);
    }
    if let Some(rec) = recorder {
        if let Ok(mut guard) = rec.lock() {
            if let Some(w) = guard.as_mut() {
                for s in data {
                    let _ = w.write_sample(s.clamp(-1.0, 1.0));
                }
            }
        }
    }
}

pub fn list_input_devices() -> Result<Vec<String>> {
    let host = cpal::default_host();
    let mut names = Vec::new();
    for d in host.input_devices()? {
        if let Ok(n) = d.name() {
            names.push(n);
        }
    }
    Ok(names)
}

/// Enumerate output (playback) devices. On WASAPI (Windows), each of these
/// can be opened in *loopback* mode via `open_loopback_with_recording` to
/// capture whatever is currently playing without any third-party software.
pub fn list_output_devices() -> Result<Vec<String>> {
    let host = cpal::default_host();
    let mut names = Vec::new();
    for d in host.output_devices()? {
        if let Ok(n) = d.name() {
            names.push(n);
        }
    }
    Ok(names)
}

/// WASAPI loopback capture: opens an *output* device but reads from it as
/// an input stream. cpal handles the WASAPI loopback flag automatically
/// when `build_input_stream` is called on a device returned from
/// `output_devices()`. Mirrors the API of `open_input_with_recording` so
/// callers can swap the two without any other changes.
pub fn open_loopback_with_recording(
    query: Option<&str>,
    window_seconds: f32,
    record_to: Option<&Path>,
) -> Result<LiveCapture> {
    let host = cpal::default_host();

    let device = if let Some(q) = query {
        let q_lower = q.to_lowercase();
        host.output_devices()?
            .find(|d| {
                d.name()
                    .map(|n| n.to_lowercase().contains(&q_lower))
                    .unwrap_or(false)
            })
            .ok_or_else(|| anyhow!("no output (loopback) device matching {q:?}"))?
    } else {
        host.default_output_device()
            .ok_or_else(|| anyhow!("no default output device for loopback"))?
    };

    // For loopback we must use the *output* config so the format matches
    // what's actually playing. WASAPI then hands us those frames as an
    // input stream.
    let config = device.default_output_config().context("output config")?;
    let device_name = format!(
        "{} (loopback)",
        device.name().unwrap_or_else(|_| "<unknown>".to_string())
    );
    let sample_rate = config.sample_rate().0;
    let channels = config.channels() as usize;
    let capacity = ((sample_rate as f32 * window_seconds) as usize).max(1);
    let buffer = Arc::new(Mutex::new(RingBuffer::new(capacity)));

    let (recorder, record_path) = if let Some(path) = record_to {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).ok();
            }
        }
        let spec = hound::WavSpec {
            channels: channels as u16,
            sample_rate,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        };
        let writer = hound::WavWriter::create(path, spec)
            .with_context(|| format!("creating WAV file {}", path.display()))?;
        (
            Some(Arc::new(StdMutex::new(Some(writer)))),
            Some(path.to_path_buf()),
        )
    } else {
        (None, None)
    };

    let err_fn = |e| eprintln!("cpal loopback stream error: {e}");
    let buffer_cb = Arc::clone(&buffer);
    let recorder_cb = recorder.clone();

    let stream = match config.sample_format() {
        cpal::SampleFormat::F32 => device.build_input_stream(
            &config.into(),
            move |data: &[f32], _| {
                push_mono(&buffer_cb, data, channels, recorder_cb.as_ref());
            },
            err_fn,
            None,
        )?,
        cpal::SampleFormat::I16 => device.build_input_stream(
            &config.into(),
            move |data: &[i16], _| {
                let f: Vec<f32> = data.iter().map(|s| *s as f32 / i16::MAX as f32).collect();
                push_mono(&buffer_cb, &f, channels, recorder_cb.as_ref());
            },
            err_fn,
            None,
        )?,
        cpal::SampleFormat::U16 => device.build_input_stream(
            &config.into(),
            move |data: &[u16], _| {
                let f: Vec<f32> = data
                    .iter()
                    .map(|s| (*s as f32 - 32768.0) / 32768.0)
                    .collect();
                push_mono(&buffer_cb, &f, channels, recorder_cb.as_ref());
            },
            err_fn,
            None,
        )?,
        other => return Err(anyhow!("unsupported loopback sample format: {other:?}")),
    };
    stream.play().context("starting loopback stream")?;

    Ok(LiveCapture {
        sample_rate,
        device_name,
        buffer,
        _stream: stream,
        recorder,
        record_path,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_wav_path(name: &str) -> PathBuf {
        let unique = format!(
            "{}-{}-{name}.wav",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system time should be after unix epoch")
                .as_nanos()
        );
        std::env::temp_dir().join(unique)
    }

    #[test]
    fn decode_file_recovers_unfinalized_qsoripper_pcm_wav() {
        let path = temp_wav_path("unfinalized");
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"RIFF");
        bytes.extend_from_slice(&0u32.to_le_bytes());
        bytes.extend_from_slice(b"WAVE");
        bytes.extend_from_slice(b"fmt ");
        bytes.extend_from_slice(&16u32.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&48_000u32.to_le_bytes());
        bytes.extend_from_slice(&96_000u32.to_le_bytes());
        bytes.extend_from_slice(&2u16.to_le_bytes());
        bytes.extend_from_slice(&16u16.to_le_bytes());
        bytes.extend_from_slice(b"data");
        bytes.extend_from_slice(&0u32.to_le_bytes());
        bytes.extend_from_slice(&0i16.to_le_bytes());
        bytes.extend_from_slice(&i16::MAX.to_le_bytes());
        bytes.extend_from_slice(&i16::MIN.to_le_bytes());
        std::fs::write(&path, bytes).expect("write test wav");

        let audio = decode_file(&path).expect("decode unfinalized wav");
        let _ = std::fs::remove_file(&path);

        assert_eq!(audio.sample_rate, 48_000);
        assert_eq!(audio.samples.len(), 3);
        assert_eq!(audio.samples[0], 0.0);
        assert!((audio.samples[1] - 1.0).abs() < f32::EPSILON);
        assert!((audio.samples[2] + 1.0).abs() < 0.0001);
    }

    #[test]
    fn decode_file_recovers_unfinalized_extensible_float_stereo_wav() {
        let path = temp_wav_path("unfinalized-float-stereo");
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"RIFF");
        bytes.extend_from_slice(&0u32.to_le_bytes());
        bytes.extend_from_slice(b"WAVE");
        bytes.extend_from_slice(b"fmt ");
        bytes.extend_from_slice(&40u32.to_le_bytes());
        bytes.extend_from_slice(&0xFFFEu16.to_le_bytes());
        bytes.extend_from_slice(&2u16.to_le_bytes());
        bytes.extend_from_slice(&48_000u32.to_le_bytes());
        bytes.extend_from_slice(&384_000u32.to_le_bytes());
        bytes.extend_from_slice(&8u16.to_le_bytes());
        bytes.extend_from_slice(&32u16.to_le_bytes());
        bytes.extend_from_slice(&22u16.to_le_bytes());
        bytes.extend_from_slice(&32u16.to_le_bytes());
        bytes.extend_from_slice(&3u32.to_le_bytes());
        bytes.extend_from_slice(&[
            0x03, 0x00, 0x00, 0x00, 0x00, 0x00, 0x10, 0x00, 0x80, 0x00, 0x00, 0xAA, 0x00, 0x38,
            0x9B, 0x71,
        ]);
        bytes.extend_from_slice(b"data");
        bytes.extend_from_slice(&0u32.to_le_bytes());
        for sample in [0.25_f32, -0.25, 0.5, -0.5] {
            bytes.extend_from_slice(&sample.to_le_bytes());
        }
        std::fs::write(&path, bytes).expect("write test wav");

        let audio = decode_file(&path).expect("decode unfinalized float wav");
        let _ = std::fs::remove_file(&path);

        assert_eq!(audio.sample_rate, 48_000);
        assert_eq!(audio.channels, 2);
        assert_eq!(audio.interleaved_samples, [0.25, -0.25, 0.5, -0.5]);
        assert_eq!(audio.samples, [0.0, 0.0]);
    }
}
