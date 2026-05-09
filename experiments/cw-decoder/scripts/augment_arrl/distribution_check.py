"""Distribution-check report.

Renders four-panel histograms of:
  - element duration
  - inter-element gap duration
  - dominant pitch
  - mean SNR estimate

for the augmented corpus, overlaid on the same stats from the 6 real OTA
bench samples.

Usage::

    py -m augment_arrl.distribution_check \\
        --manifest <path> --real-dir <path> --out <png>
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

import matplotlib

matplotlib.use("Agg")
import matplotlib.pyplot as plt
import numpy as np
import soundfile as sf

if __package__ in (None, ""):
    sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
    from augment_arrl import config as cfg_mod  # type: ignore
    from augment_arrl.impairments import estimate_envelope_stats  # type: ignore
else:
    from . import config as cfg_mod
    from .impairments import estimate_envelope_stats


def _stats_for(wav_path: Path) -> dict[str, float | np.ndarray]:
    audio, sr = sf.read(str(wav_path), dtype="float32", always_2d=False)
    if audio.ndim == 2:
        audio = audio.mean(axis=1)
    if sr != 8000:
        # Cheap downsample via slicing to stay numpy-only.
        step = sr // 8000
        audio = audio[::step]
        sr = sr // step
    s = estimate_envelope_stats(audio.astype(np.float64), sr)
    return {
        "elements": s.element_durations_s,
        "gaps": s.gap_durations_s,
        "pitch": s.dominant_pitch_hz,
        "snr": s.snr_db_estimate,
    }


def _aggregate(paths: list[Path], cap: int = 200) -> dict[str, np.ndarray]:
    elements: list[np.ndarray] = []
    gaps: list[np.ndarray] = []
    pitches: list[float] = []
    snrs: list[float] = []
    for p in paths[:cap]:
        try:
            s = _stats_for(p)
        except Exception as e:
            print(f"warn: {p.name}: {e}", file=sys.stderr)
            continue
        elements.append(s["elements"])
        gaps.append(s["gaps"])
        pitches.append(float(s["pitch"]))
        snrs.append(float(s["snr"]))
    return {
        "elements": np.concatenate(elements) if elements else np.array([]),
        "gaps": np.concatenate(gaps) if gaps else np.array([]),
        "pitches": np.array(pitches),
        "snrs": np.array(snrs),
    }


def _real_audio_paths(real_dir: Path) -> list[Path]:
    """Pick up .wav and .mp3 in the real-OTA dir."""

    out: list[Path] = []
    for ext in ("*.wav", "*.mp3"):
        out.extend(sorted(real_dir.glob(ext)))
    return out


def _augment_paths(manifest: Path, n_max: int = 200, seed: int = 0xC0FFEE) -> list[Path]:
    rows: list[dict] = []
    with manifest.open("r", encoding="utf-8") as fh:
        for line in fh:
            line = line.strip()
            if line:
                rows.append(json.loads(line))
    if not rows:
        return []
    rng = np.random.default_rng(seed)
    idx = rng.choice(len(rows), size=min(n_max, len(rows)), replace=False)
    return [Path(rows[int(i)]["wav_path"]) for i in idx]


def main(argv: list[str] | None = None) -> int:
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--manifest", default=str(cfg_mod.AUGMENTED_MANIFEST_PATH))
    p.add_argument("--real-dir", default=str(cfg_mod.DEFAULT_REAL_OTA_DIR))
    p.add_argument("--out", default=str(cfg_mod.DISTRIBUTION_PLOT_PATH))
    p.add_argument("--n-augmented", type=int, default=200)
    args = p.parse_args(argv)

    real_paths = _real_audio_paths(Path(args.real_dir))
    if not real_paths:
        print(f"ERROR: no .wav/.mp3 files in {args.real_dir}", file=sys.stderr)
        return 2
    print(f"Real OTA corpus: {len(real_paths)} files")
    real_stats = _aggregate(real_paths)

    aug_paths = _augment_paths(Path(args.manifest), n_max=args.n_augmented)
    if not aug_paths:
        print(f"ERROR: no augmented variants in {args.manifest}", file=sys.stderr)
        return 2
    print(f"Augmented sample: {len(aug_paths)} variants")
    aug_stats = _aggregate(aug_paths, cap=args.n_augmented)

    fig, axes = plt.subplots(2, 2, figsize=(11, 8))
    fig.suptitle(
        "ARRL augmented corpus distribution vs real-OTA bench (training-set-a)",
        fontsize=12,
    )

    def _hist(ax, real_data, aug_data, title, xlabel, log_x=False):
        bins = 50
        # Defensive: drop NaN/inf.
        rd = real_data[np.isfinite(real_data)]
        ad = aug_data[np.isfinite(aug_data)]
        all_d = np.concatenate([rd, ad]) if rd.size + ad.size > 0 else np.array([0.0])
        if log_x and all_d.size > 0 and all_d.min() > 0:
            bins = np.logspace(np.log10(max(1e-3, all_d.min())), np.log10(all_d.max()), 50)
            ax.set_xscale("log")
        ax.hist(rd, bins=bins, alpha=0.55, label=f"real OTA (n={rd.size})", color="C3", density=True)
        ax.hist(ad, bins=bins, alpha=0.55, label=f"augmented (n={ad.size})", color="C0", density=True)
        ax.set_title(title)
        ax.set_xlabel(xlabel)
        ax.set_ylabel("density")
        ax.legend(loc="upper right", fontsize=8)
        ax.grid(True, alpha=0.3)

    _hist(axes[0, 0], real_stats["elements"], aug_stats["elements"],
          "Element duration", "seconds", log_x=True)
    _hist(axes[0, 1], real_stats["gaps"], aug_stats["gaps"],
          "Inter-element gap", "seconds", log_x=True)
    _hist(axes[1, 0], real_stats["pitches"], aug_stats["pitches"],
          "Dominant pitch", "Hz")
    _hist(axes[1, 1], real_stats["snrs"], aug_stats["snrs"],
          "Estimated SNR (in-band peak / off-band median)", "dB")

    fig.tight_layout()
    Path(args.out).parent.mkdir(parents=True, exist_ok=True)
    fig.savefig(args.out, dpi=120)
    print(f"Wrote distribution plot: {args.out}")

    # Quick numeric summary.
    def _summarize(name, real, aug):
        if real.size == 0 or aug.size == 0:
            print(f"  {name}: (one side empty)")
            return
        print(
            f"  {name:18s}  real median={np.median(real):.3f}  "
            f"aug median={np.median(aug):.3f}  "
            f"real range=[{np.min(real):.3f},{np.max(real):.3f}]  "
            f"aug range=[{np.min(aug):.3f},{np.max(aug):.3f}]"
        )

    print("\nDistribution summary:")
    _summarize("element_dur_s", real_stats["elements"], aug_stats["elements"])
    _summarize("gap_dur_s", real_stats["gaps"], aug_stats["gaps"])
    _summarize("pitch_hz", real_stats["pitches"], aug_stats["pitches"])
    _summarize("snr_db", real_stats["snrs"], aug_stats["snrs"])
    return 0


if __name__ == "__main__":
    sys.exit(main())
