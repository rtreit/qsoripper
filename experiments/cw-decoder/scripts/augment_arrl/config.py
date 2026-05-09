"""Configuration constants and helpers for the ARRL augmentation pipeline.

All paths anchor to the repo root so the scripts work from any CWD.
"""

from __future__ import annotations

import os
import zlib
from dataclasses import dataclass, field
from pathlib import Path

# ---------------------------------------------------------------------------
# Paths
# ---------------------------------------------------------------------------

SCRIPT_DIR = Path(__file__).resolve().parent
# .../experiments/cw-decoder/scripts/augment_arrl -> repo root is 4 levels up.
REPO_ROOT = SCRIPT_DIR.parents[3]

# Source ARRL corpus lives in a sibling worktree (read-only). It can also be
# overridden via the ARRL_CORPUS_ROOT env var or CLI flag for portability.
DEFAULT_SOURCE_CORPUS_ROOT = Path(
    os.environ.get(
        "ARRL_CORPUS_ROOT",
        r"C:\Users\randy\Git\qsoripper-experiments\arrl-corpus-fast\data\cw-samples\arrl-archive",
    )
)
DEFAULT_SOURCE_MANIFEST = DEFAULT_SOURCE_CORPUS_ROOT / "manifest.jsonl"

# Output corpus (gitignored).
AUGMENTED_ROOT = REPO_ROOT / "data" / "cw-samples" / "arrl-augmented"
AUGMENTED_MANIFEST_PATH = SCRIPT_DIR / "..." / "arrl_augmented_manifest.jsonl"
AUGMENTED_MANIFEST_PATH = (SCRIPT_DIR.parent / "arrl_augmented_manifest.jsonl").resolve()
SAMPLE_MANIFEST_PATH = (SCRIPT_DIR.parent / "arrl_augmented_sample_manifest.jsonl").resolve()
DISTRIBUTION_PLOT_PATH = (SCRIPT_DIR.parent / "augment_distribution_check.png").resolve()
CER_PLOT_PATH = (SCRIPT_DIR.parent / "augment_cer_vs_snr.png").resolve()

# Real OTA bench samples for distribution comparison.
DEFAULT_REAL_OTA_DIR = Path(r"C:\Users\randy\Git\qsoripper\data\cw-samples\training-set-a")

# Decoder binary (shared release build from the viterbi worktree).
DEFAULT_DECODER_BIN = Path(
    r"C:\Users\randy\Git\qsoripper-experiments\viterbi\experiments\cw-decoder\target\release\cw-decoder.exe"
)


# ---------------------------------------------------------------------------
# Augmentation knobs
# ---------------------------------------------------------------------------

#: Number of augmented variants generated per pristine chunk.
AUG_VARIANTS_PER_CHUNK: int = 30

#: Working sample rate. ARRL chunks are 8 kHz mono; we keep that to stay cheap.
DEFAULT_SR: int = 8000

#: Carrier frequency that the synthesis assumes the source CW sits on.
DEFAULT_CARRIER_HZ: float = 700.0

#: SNR ladder (dB). One entry sampled per variant.
SNR_LADDER_DB: tuple[float, ...] = (0.0, 5.0, 10.0, 15.0, 20.0, 30.0)

#: Watterson channel CCIR/ITU-R F.1487 profiles.
#: (label, tap_delay_seconds, doppler_spread_hz)
WATTERSON_PROFILES: dict[str, tuple[float, float]] = {
    "good":     (0.5e-3, 0.1),
    "moderate": (1.0e-3, 0.5),
    "poor":     (2.0e-3, 1.0),
    "off":      (0.0,    0.0),  # bypass
}

#: Probability of each *optional* impairment being applied per variant.
#: (Always-applied impairments aren't listed: AWGN, pitch-shift, WPM scaling,
#: WPM drift, per-element timing jitter.)
OPTIONAL_PROBS: dict[str, float] = {
    "watterson":      0.65,   # most variants get a channel
    "qsb":            0.55,
    "pink_noise":     0.45,
    "vfo_chirp":      0.30,
    "pitch_drift":    0.30,
    "agc_pumping":    0.10,
    "qrm":            0.30,
    "birdies":        0.05,
    "impulse":        0.01,
    "farnsworth":     0.20,   # apply Farnsworth-style ratio expansion
}

#: Rough-fist timing-jitter sigma classes.
JITTER_CLASSES: dict[str, float] = {
    "paddle":  0.05,
    "amateur": 0.10,
    "rough":   0.20,
}


@dataclass(frozen=True)
class ImpairmentConfig:
    """Sample-and-hold parameter bundle drawn for a single variant."""

    chunk_id: str
    augment_seed: int
    seed_u32: int

    # Always-applied
    snr_db: float
    pitch_shift_hz: float                # ±50 Hz from 700 Hz
    wpm_scale: float                     # [0.7, 1.5]
    wpm_drift_eps: float                 # [0.0, 0.10]
    wpm_drift_freq_hz: float             # [0.01, 0.1]
    jitter_class: str                    # paddle/amateur/rough
    jitter_sigma: float

    # Optional
    watterson_profile: str               # good/moderate/poor/off
    qsb_freq_hz: float | None            # 0.1..2.0 Hz; None => off
    qsb_depth: float | None              # 0.2..0.8
    pink_noise_db: float | None          # noise level (dB below carrier)
    vfo_chirp_hz: float | None           # ±5..10 Hz transient amplitude
    pitch_drift_hz_per_min: float | None # ±20 Hz/min
    agc_enabled: bool
    qrm_enabled: bool
    qrm_partner_chunk_id: str | None
    qrm_partner_offset_hz: float | None
    qrm_partner_snr_db: float | None
    birdies: tuple[float, ...]           # offsets (Hz) to drop in
    impulse_rate_hz: float | None        # poisson rate
    farnsworth_ratio: float | None       # 1.0..3.0

    # Composition
    applied: tuple[str, ...] = field(default_factory=tuple)


def variant_seed(chunk_id: str, augment_seed: int) -> int:
    """Deterministic 32-bit seed from ``(chunk_id, augment_seed)``.

    Used both to drive ``numpy.random.default_rng`` and to namespace every
    Watterson-tap / noise-vector in a render so ``(chunk_id, augment_seed)``
    is reproducible byte-for-byte.
    """

    key = f"{chunk_id}|{augment_seed}".encode("utf-8")
    return zlib.crc32(key) & 0xFFFFFFFF


def variant_basename(chunk_id: str, augment_seed: int) -> str:
    """Filename stem for a rendered variant (no extension)."""

    return f"{chunk_id}_aug{augment_seed:03d}"
