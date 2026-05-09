"""Per-variant render orchestrator.

Given a source ARRL chunk + ``augment_seed``, draw a deterministic impairment
parameter set, apply it, and return the rendered audio (plus metadata for the
augmentation manifest).

This module is the only one that touches the filesystem (loading WAVs).
"""

from __future__ import annotations

from dataclasses import asdict, dataclass
from pathlib import Path
from typing import Any

import numpy as np
import soundfile as sf

from .config import (
    AUG_VARIANTS_PER_CHUNK,
    DEFAULT_CARRIER_HZ,
    DEFAULT_SR,
    JITTER_CLASSES,
    OPTIONAL_PROBS,
    SNR_LADDER_DB,
    WATTERSON_PROFILES,
    ImpairmentConfig,
    variant_seed,
)
from . import impairments as imp


# ---------------------------------------------------------------------------
# Parameter sampling
# ---------------------------------------------------------------------------

def _sample_config(
    chunk_id: str,
    augment_seed: int,
    *,
    qrm_partner_chunk_id: str | None = None,
    duration_s: float = 30.0,
) -> ImpairmentConfig:
    seed = variant_seed(chunk_id, augment_seed)
    rng = np.random.default_rng(seed)
    applied: list[str] = ["awgn", "wpm_scale", "pitch_shift", "jitter", "wpm_drift"]

    snr_db = float(rng.choice(SNR_LADDER_DB))
    pitch_shift = float(rng.uniform(-50.0, 50.0))
    wpm_scale = float(rng.uniform(0.7, 1.5))
    eps = float(rng.uniform(0.0, 0.10))
    f_drift = float(rng.uniform(0.01, 0.10))
    jitter_class = str(rng.choice(list(JITTER_CLASSES.keys())))
    jitter_sigma = JITTER_CLASSES[jitter_class]

    def maybe(name: str) -> bool:
        hit = bool(rng.random() < OPTIONAL_PROBS[name])
        if hit:
            applied.append(name)
        return hit

    watterson_profile = "off"
    if maybe("watterson"):
        labels = [k for k in WATTERSON_PROFILES if k != "off"]
        watterson_profile = str(rng.choice(labels))

    qsb_freq = qsb_depth = None
    if maybe("qsb"):
        qsb_freq = float(rng.uniform(0.1, 2.0))
        qsb_depth = float(rng.uniform(0.2, 0.8))

    pink_db = None
    if maybe("pink_noise"):
        pink_db = float(rng.uniform(5.0, 25.0))  # dB above noise floor

    vfo_chirp = None
    if maybe("vfo_chirp"):
        vfo_chirp = float(rng.uniform(5.0, 10.0)) * (1.0 if rng.random() < 0.5 else -1.0)

    pitch_drift = None
    if maybe("pitch_drift"):
        pitch_drift = float(rng.uniform(-20.0, 20.0))

    agc_enabled = maybe("agc_pumping")

    qrm_enabled = qrm_partner_chunk_id is not None and maybe("qrm")
    qrm_offset = qrm_snr = None
    if qrm_enabled:
        qrm_offset = float(rng.uniform(-100.0, 100.0))
        qrm_snr = float(rng.uniform(-6.0, 6.0))

    birdies: tuple[float, ...] = ()
    if maybe("birdies"):
        n = int(rng.integers(1, 3))
        birdies = tuple(
            float(rng.choice([-1.0, 1.0]) * rng.uniform(50.0, 200.0)) for _ in range(n)
        )

    impulse_rate = None
    if maybe("impulse"):
        impulse_rate = float(rng.uniform(0.5, 3.0))  # impulses / s

    farnsworth_ratio = None
    if maybe("farnsworth"):
        farnsworth_ratio = float(rng.uniform(1.2, 3.0))

    return ImpairmentConfig(
        chunk_id=chunk_id,
        augment_seed=augment_seed,
        seed_u32=seed,
        snr_db=snr_db,
        pitch_shift_hz=pitch_shift,
        wpm_scale=wpm_scale,
        wpm_drift_eps=eps,
        wpm_drift_freq_hz=f_drift,
        jitter_class=jitter_class,
        jitter_sigma=jitter_sigma,
        watterson_profile=watterson_profile,
        qsb_freq_hz=qsb_freq,
        qsb_depth=qsb_depth,
        pink_noise_db=pink_db,
        vfo_chirp_hz=vfo_chirp,
        pitch_drift_hz_per_min=pitch_drift,
        agc_enabled=agc_enabled,
        qrm_enabled=qrm_enabled,
        qrm_partner_chunk_id=qrm_partner_chunk_id if qrm_enabled else None,
        qrm_partner_offset_hz=qrm_offset,
        qrm_partner_snr_db=qrm_snr,
        birdies=birdies,
        impulse_rate_hz=impulse_rate,
        farnsworth_ratio=farnsworth_ratio,
        applied=tuple(applied),
    )


# ---------------------------------------------------------------------------
# I/O
# ---------------------------------------------------------------------------

def _load_chunk(path: Path, target_sr: int) -> tuple[np.ndarray, int]:
    audio, sr = sf.read(str(path), dtype="float32", always_2d=False)
    if audio.ndim == 2:
        audio = audio.mean(axis=1)
    if sr != target_sr:
        # Source corpus is 8 kHz; we don't re-resample inside the hot loop.
        # Fail fast so callers don't silently mix sample rates.
        raise ValueError(f"unexpected sample rate {sr} for {path} (expected {target_sr})")
    return audio.astype(np.float64), sr


# ---------------------------------------------------------------------------
# Render
# ---------------------------------------------------------------------------

@dataclass
class RenderResult:
    audio: np.ndarray
    sr: int
    config: ImpairmentConfig
    src_duration_s: float
    out_duration_s: float

    def manifest_dict(self, *, wav_path: str, src_wav: str, truth: str, wpm: float) -> dict[str, Any]:
        cfg_dict = asdict(self.config)
        return {
            "wav_path": wav_path,
            "src_wav_path": src_wav,
            "text": truth,
            "src_wpm": wpm,
            "augment_seed": self.config.augment_seed,
            "chunk_id": self.config.chunk_id,
            "seed_u32": self.config.seed_u32,
            "src_duration_s": round(self.src_duration_s, 3),
            "duration_s": round(self.out_duration_s, 3),
            "sample_rate": self.sr,
            "snr_db": self.config.snr_db,
            "watterson_profile": self.config.watterson_profile,
            "applied": list(self.config.applied),
            "params": {
                k: v for k, v in cfg_dict.items()
                if k not in {"chunk_id", "augment_seed", "seed_u32", "applied"}
            },
        }


def render_variant(
    src_path: Path,
    chunk_id: str,
    augment_seed: int,
    *,
    target_sr: int = DEFAULT_SR,
    carrier_hz: float = DEFAULT_CARRIER_HZ,
    qrm_partner: tuple[str, Path] | None = None,
) -> RenderResult:
    """Render one variant of ``chunk_id`` deterministically."""

    src_audio, sr = _load_chunk(src_path, target_sr)
    duration_s = src_audio.size / sr
    cfg = _sample_config(
        chunk_id,
        augment_seed,
        qrm_partner_chunk_id=qrm_partner[0] if qrm_partner else None,
        duration_s=duration_s,
    )
    rng = np.random.default_rng(cfg.seed_u32)

    # ---- 1. Demod to baseband (preserve original carrier).
    bb = imp.to_baseband(src_audio, sr, carrier_hz)

    # ---- 2. Timing: nominal-dit length estimated from the source WPM
    #         (we don't know it precisely; use a generic 60ms which spans
    #         15-30 wpm well — only used to scale per-element jitter).
    nominal_dit_s = 60e-3

    if cfg.farnsworth_ratio is not None:
        env = np.abs(bb)
        fmap = imp.farnsworth_stretch_map(env, cfg.farnsworth_ratio)
        bb = imp.time_warp(bb, fmap)

    if cfg.jitter_sigma > 0.0:
        jmap = imp.per_element_jitter_map(np.abs(bb), sr, cfg.jitter_sigma, rng, nominal_dit_s)
        bb = imp.time_warp(bb, jmap)

    if cfg.wpm_drift_eps > 0.0:
        dmap = imp.wpm_drift_map(bb.size, sr, cfg.wpm_drift_eps, cfg.wpm_drift_freq_hz)
        bb = imp.time_warp(bb, dmap)

    if cfg.wpm_scale != 1.0:
        smap = imp.wpm_scale_map(bb.size, cfg.wpm_scale)
        bb = imp.time_warp(bb, smap)

    # ---- 3. HF channel on baseband.
    if cfg.watterson_profile != "off":
        tau, fd = WATTERSON_PROFILES[cfg.watterson_profile]
        bb = imp.watterson_channel(bb, sr, tau, fd, rng)

    if cfg.qsb_freq_hz is not None:
        bb = bb * imp.qsb_envelope(bb.size, sr, cfg.qsb_freq_hz, cfg.qsb_depth or 0.0)

    # ---- 4. Upconvert with pitch shift / drift / VFO chirp.
    new_carrier = carrier_hz + cfg.pitch_shift_hz
    phase_curve = np.zeros(bb.size, dtype=np.float64)
    if cfg.pitch_drift_hz_per_min is not None:
        phase_curve = phase_curve + imp.pitch_drift_curve(bb.size, sr, cfg.pitch_drift_hz_per_min)
    if cfg.vfo_chirp_hz is not None:
        phase_curve = phase_curve + imp.vfo_chirp_curve(np.abs(bb), sr, cfg.vfo_chirp_hz)
    audio = imp.from_baseband(bb, sr, new_carrier, phase_hz=phase_curve)

    # ---- 5. QRM: render partner deterministically and mix in.
    if cfg.qrm_enabled and qrm_partner is not None:
        partner_id, partner_path = qrm_partner
        partner_audio, _ = _load_chunk(partner_path, target_sr)
        # Pad / clip partner to current length, then frequency-shift it.
        n = audio.size
        if partner_audio.size < n:
            reps = int(np.ceil(n / partner_audio.size))
            partner_audio = np.tile(partner_audio, reps)[:n]
        else:
            partner_audio = partner_audio[:n]
        # Shift QRM up/down by Δpitch by demod / remod.
        p_bb = imp.to_baseband(partner_audio, sr, carrier_hz)
        qrm_audio = imp.from_baseband(
            p_bb, sr, carrier_hz + (cfg.qrm_partner_offset_hz or 0.0)
        )
        audio = imp.add_qrm(audio, qrm_audio, cfg.qrm_partner_snr_db or 0.0)

    # ---- 6. Noise, birdies, impulses.
    audio = imp.add_awgn(audio, cfg.snr_db, rng)
    if cfg.pink_noise_db is not None:
        audio = imp.add_pink_noise(audio, cfg.pink_noise_db, rng)
    if cfg.impulse_rate_hz is not None:
        audio = imp.add_impulses(audio, sr, cfg.impulse_rate_hz, rng)
    if cfg.birdies:
        audio = imp.add_birdies(audio, sr, cfg.birdies, new_carrier)

    # ---- 7. Receiver AGC pumping.
    if cfg.agc_enabled:
        audio = imp.agc_pump(audio, sr)

    # ---- 8. Final scaling / clip-safe export at int16-equivalent peak.
    peak = float(np.max(np.abs(audio)))
    if peak > 0.99:
        audio = audio * (0.99 / peak)
    audio = audio.astype(np.float32)

    return RenderResult(
        audio=audio,
        sr=sr,
        config=cfg,
        src_duration_s=duration_s,
        out_duration_s=audio.size / sr,
    )


def write_wav(path: Path, audio: np.ndarray, sr: int) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    sf.write(str(path), audio, sr, subtype="PCM_16")
