"""Audio-domain impairments used by the ARRL augmentation pipeline.

Each function is **pure**: it takes a NumPy array (and possibly a deterministic
RNG / parameters) and returns a new array. Determinism is enforced by always
using the caller-supplied ``numpy.random.Generator``.

The signal flow is:

1. Source CW (real, 8 kHz, carrier ~700 Hz) is demodulated to complex baseband.
2. Timing impairments warp the *baseband* time axis (per-element jitter, WPM
   scaling, WPM drift, Farnsworth-style gap stretching).
3. Pitch impairments are applied at the upconvert step (pitch shift, slow
   pitch drift, per-element VFO chirp).
4. The HF channel (Watterson model, QSB) multiplies the upconverted signal.
5. Noise and interference (AWGN, pink, impulses, QRM, birdies, AGC pumping)
   are added at the final real-audio stage.

References:
    - Watterson, C.C. et al., "Experimental Confirmation of an HF Channel
      Model", IEEE Trans. Comm. Tech. (1970).
    - ITU-R Recommendation F.1487 (2000) — HF channel simulator profiles.
    - MIL-STD-188-110B App. C — channel models for HF data modems.
    - Jakes, "Microwave Mobile Communications" (1974) — sum-of-sinusoids
      Doppler simulation.
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import Sequence

import numpy as np
from scipy import signal


# ---------------------------------------------------------------------------
# Demod / remod
# ---------------------------------------------------------------------------

def to_baseband(x: np.ndarray, sr: int, fc: float) -> np.ndarray:
    """Return the analytic complex baseband at carrier ``fc`` (low-passed)."""

    n = np.arange(x.size, dtype=np.float64)
    lo = np.exp(-1j * 2.0 * np.pi * fc * n / sr)
    bb = x.astype(np.float64) * lo
    # Low-pass to keep the keying envelope (~150 Hz BW is plenty for CW).
    sos = signal.butter(4, 200.0 / (sr / 2.0), btype="low", output="sos")
    return signal.sosfiltfilt(sos, bb)


def from_baseband(bb: np.ndarray, sr: int, fc: float, phase_hz: np.ndarray | None = None) -> np.ndarray:
    """Upconvert complex baseband to a real signal at carrier ``fc``.

    ``phase_hz`` (optional, length-N) is an instantaneous-frequency offset
    (Hz) added to the carrier sample-by-sample — used for pitch drift and
    VFO chirp.
    """

    n = np.arange(bb.size, dtype=np.float64)
    if phase_hz is None:
        phase = 2.0 * np.pi * fc * n / sr
    else:
        # Instantaneous phase = ∫ 2π·f(t) dt
        inst_freq = fc + phase_hz
        phase = 2.0 * np.pi * np.cumsum(inst_freq) / sr
    return np.real(bb * np.exp(1j * phase))


# ---------------------------------------------------------------------------
# Timing
# ---------------------------------------------------------------------------

def time_warp(x: np.ndarray, time_map: np.ndarray) -> np.ndarray:
    """Resample ``x`` according to ``time_map`` (output_sample_index -> source_index).

    Both real and complex arrays are supported.
    """

    src_idx = np.arange(x.size, dtype=np.float64)
    if np.iscomplexobj(x):
        re = np.interp(time_map, src_idx, x.real, left=0.0, right=0.0)
        im = np.interp(time_map, src_idx, x.imag, left=0.0, right=0.0)
        return re + 1j * im
    return np.interp(time_map, src_idx, x, left=0.0, right=0.0)


def wpm_scale_map(n_in: int, scale: float) -> np.ndarray:
    """Time map for a uniform WPM scale (faster speech = smaller scale).

    ``scale > 1`` => slower (longer audio).
    """

    n_out = max(1, int(round(n_in * scale)))
    return np.linspace(0.0, n_in - 1, n_out)


def wpm_drift_map(n_in: int, sr: int, eps: float, freq_hz: float) -> np.ndarray:
    """Per-sample sinusoidal time-warp implementing WPM(t) = WPM₀ * (1 + ε sin)."""

    if eps <= 0.0:
        return np.arange(n_in, dtype=np.float64)
    t = np.arange(n_in, dtype=np.float64) / sr
    # WPM scales inversely with element duration. dt'/dt = 1/(1+ε sin).
    # Cumulative time-map: τ(t) = ∫ 1/(1+ε sin(2π f t)) dt.
    inv = 1.0 / (1.0 + eps * np.sin(2.0 * np.pi * freq_hz * t))
    tau = np.cumsum(inv) / sr
    # Now we want output sample k -> source time s such that τ(s)=k/sr_out.
    # Source samples = tau * sr (numerical integration of inv).
    n_out = int(round(tau[-1] * sr))
    if n_out < 8:
        return np.arange(n_in, dtype=np.float64)
    out_t = np.linspace(0.0, tau[-1], n_out)
    return np.interp(out_t, tau, np.arange(n_in, dtype=np.float64))


def per_element_jitter_map(
    envelope: np.ndarray,
    sr: int,
    sigma_rel: float,
    rng: np.random.Generator,
    nominal_dit_s: float,
) -> np.ndarray:
    """Per-element timing jitter (rough fist).

    Detects rising / falling edges by hysteresis on the magnitude envelope,
    perturbs each edge time by ``LogNormal(μ=0, σ=sigma_rel)`` of the nominal
    dit length, and returns a piecewise-linear time-warp map.
    """

    if sigma_rel <= 0.0:
        return np.arange(envelope.size, dtype=np.float64)
    env = envelope - envelope.min()
    peak = max(env.max(), 1e-9)
    # Vectorized hysteresis: smooth via running max-of-thresholds approach.
    above_high = env > 0.55 * peak
    above_low = env > 0.30 * peak
    # Hysteresis state: +1 for rising-into-high, -1 for falling-out-of-low.
    state = np.zeros(env.size, dtype=np.int8)
    state[above_high] = 1
    state[~above_low] = -1
    # Forward-fill zeros so each sample inherits the most recent decisive
    # state. Trick: replace 0 with NaN, then `np.maximum.accumulate` over
    # `state.cumsum`-style fill — simplest is a manual scan but only over
    # the *non-zero* indices, which is much shorter than the full envelope.
    nz_idx = np.flatnonzero(state)
    if nz_idx.size == 0:
        return np.arange(envelope.size, dtype=np.float64)
    nz_state = state[nz_idx]
    # An "edge" is where the decisive state flips between -1 and +1.
    flip_mask = np.concatenate(([True], nz_state[1:] != nz_state[:-1]))
    edge_indices = nz_idx[flip_mask]
    edges_arr = np.concatenate(([0.0], edge_indices.astype(np.float64), [envelope.size - 1]))
    if edges_arr.size <= 2:
        return np.arange(envelope.size, dtype=np.float64)
    # Multiplicative LogNormal jitter on each interior edge time.
    nominal_samples = max(8.0, nominal_dit_s * sr)
    jitter = rng.lognormal(mean=0.0, sigma=sigma_rel, size=edges_arr.size - 2)
    shifts = (jitter - 1.0) * nominal_samples * 0.5
    perturbed = edges_arr.copy()
    perturbed[1:-1] = edges_arr[1:-1] + shifts
    # Keep monotonic.
    perturbed = np.maximum.accumulate(perturbed)
    perturbed = np.clip(perturbed, 0.0, envelope.size - 1)
    if perturbed[-1] <= perturbed[0]:
        return np.arange(envelope.size, dtype=np.float64)
    n_out = int(round(perturbed[-1])) + 1
    out_axis = np.arange(n_out, dtype=np.float64)
    return np.interp(out_axis, perturbed, edges_arr)


def farnsworth_stretch_map(
    envelope: np.ndarray,
    ratio: float,
    threshold_frac: float = 0.30,
) -> np.ndarray:
    """Stretch low-envelope (gap) regions by ``ratio`` (Farnsworth padding).

    ``ratio == 1`` is a no-op. ``ratio == 2`` doubles every detected gap.
    """

    if ratio <= 1.0:
        return np.arange(envelope.size, dtype=np.float64)
    env = np.abs(envelope)
    thr = threshold_frac * env.max()
    is_gap = env < thr
    # Local stretching factor per source sample.
    factor = np.where(is_gap, ratio, 1.0)
    # Cumulative output sample index per source sample.
    cum = np.cumsum(factor)
    n_out = int(round(cum[-1]))
    if n_out < 8:
        return np.arange(envelope.size, dtype=np.float64)
    out_axis = np.linspace(1.0, cum[-1], n_out)
    return np.interp(out_axis, cum, np.arange(envelope.size, dtype=np.float64))


# ---------------------------------------------------------------------------
# Pitch / VFO
# ---------------------------------------------------------------------------

def pitch_drift_curve(n: int, sr: int, hz_per_min: float) -> np.ndarray:
    """Slow linear ± drift (Hz) over the chunk."""

    t = np.arange(n, dtype=np.float64) / sr
    return hz_per_min * t / 60.0


def vfo_chirp_curve(envelope: np.ndarray, sr: int, amp_hz: float, decay_ms: float = 15.0) -> np.ndarray:
    """Per-element VFO chirp: a decaying ±amp_hz transient at each rising edge.

    Real cheap rigs exhibit a brief frequency excursion as the VFO recovers
    from the keying transient.
    """

    if amp_hz == 0.0:
        return np.zeros(envelope.size, dtype=np.float64)
    env = np.abs(envelope)
    peak = max(env.max(), 1e-9)
    above = env > 0.5 * peak
    # Rising edges = transitions False -> True.
    edges = np.where(np.logical_and(above[1:], np.logical_not(above[:-1])))[0] + 1
    out = np.zeros(envelope.size, dtype=np.float64)
    if edges.size == 0:
        return out
    decay_n = max(1, int(decay_ms * 1e-3 * sr))
    template = np.exp(-np.arange(decay_n, dtype=np.float64) / (decay_n / 3.0))
    for e in edges:
        end = min(envelope.size, e + decay_n)
        out[e:end] += amp_hz * template[: end - e]
    return out


# ---------------------------------------------------------------------------
# HF channel
# ---------------------------------------------------------------------------

def watterson_channel(
    bb: np.ndarray,
    sr: int,
    tau_s: float,
    doppler_hz: float,
    rng: np.random.Generator,
    n_components: int = 16,
) -> np.ndarray:
    """Apply a 2-tap Watterson channel to complex baseband ``bb``.

    Each tap is a Rayleigh-faded path with Gaussian Doppler spread of
    ``doppler_hz`` (1-σ). Implemented via a sum-of-sinusoids
    approximation (Jakes-style).

    Performance trick: the tap process is band-limited to ``doppler_hz``
    (≤1 Hz for the worst CCIR profile), so we synthesize the complex
    Gaussian fading sequence at a coarse 100 Hz internal rate and linearly
    interpolate up to ``sr``. This keeps Watterson at ~5 ms per chunk
    instead of >1 s.

    A ``tau_s == 0`` profile bypasses channel entirely.
    """

    if tau_s <= 0.0 and doppler_hz <= 0.0:
        return bb
    n = bb.size
    # Coarse rate: 50× the worst doppler, with a floor at 50 Hz so even the
    # "good" profile (f_d=0.1 Hz) renders enough samples.
    coarse_sr = max(50.0, 50.0 * max(doppler_hz, 0.1))
    n_coarse = max(8, int(round(n / sr * coarse_sr)) + 4)
    t_c = np.arange(n_coarse, dtype=np.float64) / coarse_sr
    t_full = np.arange(n, dtype=np.float64) / sr

    def tap() -> np.ndarray:
        freqs = rng.normal(0.0, max(doppler_hz, 1e-6), size=n_components)
        phases = rng.uniform(0.0, 2.0 * np.pi, size=n_components)
        amps = (rng.normal(size=n_components) + 1j * rng.normal(size=n_components)) / np.sqrt(2.0 * n_components)
        ph = 2.0 * np.pi * np.outer(t_c, freqs) + phases[None, :]
        h_coarse = (amps[None, :] * np.exp(1j * ph)).sum(axis=1)
        # Linear interp up to sr (separate real/imag).
        h_re = np.interp(t_full, t_c, h_coarse.real)
        h_im = np.interp(t_full, t_c, h_coarse.imag)
        return h_re + 1j * h_im

    h0 = tap()
    h1 = tap()
    delay = max(0, int(round(tau_s * sr)))
    delayed = np.concatenate([np.zeros(delay, dtype=bb.dtype), bb[: n - delay]])
    out = h0 * bb + h1 * delayed
    p_in = float(np.mean(np.abs(bb) ** 2)) + 1e-12
    p_out = float(np.mean(np.abs(out) ** 2)) + 1e-12
    return out * np.sqrt(p_in / p_out)


def qsb_envelope(n: int, sr: int, freq_hz: float, depth: float) -> np.ndarray:
    """Slow fading multiplier A(t) = 1 - depth/2 + (depth/2) cos(2π f t).

    Range stays in [1-depth, 1] which avoids polarity flips while still
    producing the audible "QSB" amplitude swell.
    """

    t = np.arange(n, dtype=np.float64) / sr
    return 1.0 - depth / 2.0 + (depth / 2.0) * np.cos(2.0 * np.pi * freq_hz * t)


# ---------------------------------------------------------------------------
# Noise / interference
# ---------------------------------------------------------------------------

def add_awgn(x: np.ndarray, snr_db: float, rng: np.random.Generator) -> np.ndarray:
    sig_p = float(np.mean(x ** 2)) + 1e-12
    noise_p = sig_p / (10.0 ** (snr_db / 10.0))
    n = rng.standard_normal(x.size) * np.sqrt(noise_p)
    return x + n


def add_pink_noise(x: np.ndarray, level_db: float, rng: np.random.Generator) -> np.ndarray:
    """Add 1/f noise at ``level_db`` below the signal RMS."""

    n = x.size
    white = rng.standard_normal(n)
    # Voss-McCartney via FFT: shape spectrum by 1/sqrt(f).
    spec = np.fft.rfft(white)
    f = np.arange(spec.size, dtype=np.float64)
    f[0] = 1.0
    spec /= np.sqrt(f)
    pink = np.fft.irfft(spec, n=n)
    pink *= np.std(white) / (np.std(pink) + 1e-12)
    sig_p = float(np.mean(x ** 2)) + 1e-12
    noise_p = sig_p / (10.0 ** (level_db / 10.0))
    pink *= np.sqrt(noise_p / (np.mean(pink ** 2) + 1e-12))
    return x + pink


def add_impulses(
    x: np.ndarray,
    sr: int,
    rate_hz: float,
    rng: np.random.Generator,
    amplitude: float = 1.5,
) -> np.ndarray:
    """Atmospheric / static-crash style impulsive noise (Poisson process)."""

    if rate_hz <= 0.0:
        return x
    duration = x.size / sr
    n_impulses = rng.poisson(rate_hz * duration)
    if n_impulses == 0:
        return x
    out = x.copy()
    sig_peak = float(np.max(np.abs(x))) + 1e-9
    for _ in range(int(n_impulses)):
        pos = int(rng.integers(0, x.size))
        width = int(rng.integers(2, 8))
        sign = 1.0 if rng.random() < 0.5 else -1.0
        end = min(x.size, pos + width)
        out[pos:end] += sign * amplitude * sig_peak * np.exp(
            -np.arange(end - pos, dtype=np.float64) / 2.0
        )
    return out


def add_birdies(
    x: np.ndarray,
    sr: int,
    offsets_hz: Sequence[float],
    carrier_hz: float,
    level_db: float = -10.0,
) -> np.ndarray:
    """Add narrow CW tones (receiver birdies) at ``carrier_hz + offset``."""

    if not offsets_hz:
        return x
    sig_rms = float(np.sqrt(np.mean(x ** 2))) + 1e-12
    amp = sig_rms * (10.0 ** (level_db / 20.0))
    t = np.arange(x.size, dtype=np.float64) / sr
    out = x.copy()
    for off in offsets_hz:
        out += amp * np.cos(2.0 * np.pi * (carrier_hz + off) * t)
    return out


def add_qrm(
    x: np.ndarray,
    qrm_signal: np.ndarray,
    snr_ratio_db: float,
) -> np.ndarray:
    """Sum a second CW signal at the requested S/QRM ratio.

    ``qrm_signal`` is assumed already pre-shifted to the desired ΔPitch.
    """

    n = min(x.size, qrm_signal.size)
    sig_p = float(np.mean(x[:n] ** 2)) + 1e-12
    qrm_p = float(np.mean(qrm_signal[:n] ** 2)) + 1e-12
    target_qrm_p = sig_p / (10.0 ** (snr_ratio_db / 10.0))
    g = np.sqrt(target_qrm_p / qrm_p)
    out = x.copy()
    out[:n] = out[:n] + g * qrm_signal[:n]
    return out


def agc_pump(x: np.ndarray, sr: int, attack_ms: float = 10.0, release_ms: float = 200.0,
             threshold: float = 0.30) -> np.ndarray:
    """Soft-knee one-pole AGC simulating receiver gain pumping.

    Vectorized via ``scipy.signal.lfilter`` for the attack/release envelope.
    We use a single-coefficient release filter and gate the attack with the
    instantaneous absolute value, which matches the audible behavior of
    a fast-attack / slow-release receiver AGC closely enough for training.
    """

    a_release = np.exp(-1.0 / (release_ms * 1e-3 * sr))
    abs_x = np.abs(x)
    # Release-only one-pole (slow decay), then take elementwise max with
    # the instantaneous abs (fast attack ≈ 1 sample).
    env_release = signal.lfilter([1.0 - a_release], [1.0, -a_release], abs_x)
    a_attack = np.exp(-1.0 / (attack_ms * 1e-3 * sr))
    env = np.maximum(env_release, signal.lfilter([1.0 - a_attack], [1.0, -a_attack], abs_x))
    gain = np.where(env > threshold, threshold / (env + 1e-9), 1.0)
    return x * gain


def estimate_envelope_stats_loop(env: np.ndarray) -> tuple[np.ndarray, np.ndarray]:
    """Vectorized rising / falling edge detection used by stats."""

    norm = env / max(env.max(), 1e-9)
    above_high = norm > 0.55
    above_low = norm > 0.30
    state = np.zeros(env.size, dtype=np.int8)
    state[above_high] = 1
    state[~above_low] = -1
    nz_idx = np.flatnonzero(state)
    if nz_idx.size == 0:
        return np.array([]), np.array([])
    nz_state = state[nz_idx]
    flip_mask = np.concatenate(([True], nz_state[1:] != nz_state[:-1]))
    edge_indices = nz_idx[flip_mask]
    edge_states = nz_state[flip_mask]
    starts = edge_indices[edge_states == 1]
    stops = edge_indices[edge_states == -1]
    n = min(starts.size, stops.size)
    return starts[:n], stops[:n]


# ---------------------------------------------------------------------------
# Utilities
# ---------------------------------------------------------------------------

@dataclass
class EnvelopeStats:
    """Crude envelope-domain stats used by the distribution-check plot."""

    element_durations_s: np.ndarray
    gap_durations_s: np.ndarray
    dominant_pitch_hz: float
    snr_db_estimate: float


def estimate_envelope_stats(x: np.ndarray, sr: int) -> EnvelopeStats:
    """Run a coarse on/off detection + Goertzel-ish pitch estimate."""

    if x.size == 0:
        return EnvelopeStats(np.array([]), np.array([]), 0.0, 0.0)
    # Envelope via |hilbert| (fall back to abs if signal is too short).
    if x.size < 64:
        env = np.abs(x)
    else:
        env = np.abs(signal.hilbert(x))
    # Smooth ~5 ms.
    win = max(1, int(sr * 0.005))
    env = np.convolve(env, np.ones(win) / win, mode="same")
    if env.max() <= 0:
        return EnvelopeStats(np.array([]), np.array([]), 0.0, 0.0)
    starts_a, stops_a = estimate_envelope_stats_loop(env)
    elements = (stops_a - starts_a) / sr
    gaps = (starts_a[1:] - stops_a[:-1]) / sr if starts_a.size >= 2 else np.array([])
    elements = elements[elements > 0]
    gaps = gaps[gaps > 0]

    # Dominant pitch via FFT.
    spec = np.abs(np.fft.rfft(x * np.hanning(x.size)))
    freqs = np.fft.rfftfreq(x.size, d=1.0 / sr)
    band = (freqs >= 300) & (freqs <= 1200)
    if not band.any():
        dom = 0.0
    else:
        dom = float(freqs[band][int(np.argmax(spec[band]))])

    # Crude SNR: ratio of in-band peak spectral energy to off-band median.
    in_band = spec[band].mean() if band.any() else 0.0
    off_band = np.median(spec[~band]) if (~band).any() else 1e-9
    snr_est = 20.0 * np.log10((in_band + 1e-9) / (off_band + 1e-9))
    return EnvelopeStats(elements, gaps, dom, snr_est)
