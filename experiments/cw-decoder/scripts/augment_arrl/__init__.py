"""ARRL CW corpus augmentation pipeline.

Generates K deterministic noisy/impaired variants per pristine ARRL chunk so
downstream training has realistic HF-channel coverage (Watterson model, QRM,
QSB, AGC pumping, rough-fist timing jitter, ...).

Re-running the same ``(chunk_id, augment_seed)`` produces a bit-identical WAV.
"""

from .config import AUG_VARIANTS_PER_CHUNK, DEFAULT_SR, ImpairmentConfig
from .render import render_variant, RenderResult

__all__ = [
    "AUG_VARIANTS_PER_CHUNK",
    "DEFAULT_SR",
    "ImpairmentConfig",
    "render_variant",
    "RenderResult",
]
