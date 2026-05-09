"""Stage 2: parallel trim of intro/outro silence using ProcessPoolExecutor.

Reuses the Goertzel @ 700 Hz tone-extent detector from the prior pilot
(`trim.py`) but parallelizes per-session work across all CPU cores.
"""

from __future__ import annotations

import argparse
import json
import math
import os
import shutil
import subprocess
import sys
import wave
from concurrent.futures import ProcessPoolExecutor, as_completed
from pathlib import Path

import numpy as np

from common import (
    CORPUS_ROOT,
    DEFAULT_SPEEDS,
    configure_logging,
    ensure_corpus_dirs,
    parse_speeds_arg,
    session_paths,
    speed_dirname,
)

SAMPLE_RATE = 8000
TONE_HZ = 700.0
WINDOW_MS = 25
HOP_MS = 10
SUSTAIN_MS = 200
PRE_PAD_S = 5.0
POST_PAD_S = 1.0
SILENCE_TAIL_S = 3.0


def have_ffmpeg() -> bool:
    return shutil.which("ffmpeg") is not None


def _decode_to_pcm(mp3: Path) -> np.ndarray:
    cmd = [
        "ffmpeg", "-loglevel", "error",
        "-i", str(mp3),
        "-ac", "1",
        "-ar", str(SAMPLE_RATE),
        "-f", "s16le",
        "-",
    ]
    proc = subprocess.run(cmd, check=True, capture_output=True)
    return np.frombuffer(proc.stdout, dtype=np.int16).astype(np.float32) / 32768.0


def _goertzel_powers(samples: np.ndarray, sr: int) -> tuple[np.ndarray, int]:
    win = int(sr * WINDOW_MS / 1000)
    hop = int(sr * HOP_MS / 1000)
    k = int(0.5 + (win * TONE_HZ) / sr)
    omega = (2.0 * math.pi * k) / win
    coeff = 2.0 * math.cos(omega)

    n_frames = max(0, 1 + (len(samples) - win) // hop)
    if n_frames == 0:
        return np.zeros(0, dtype=np.float32), hop

    frames = np.lib.stride_tricks.sliding_window_view(samples, win)[::hop][:n_frames]
    hann = np.hanning(win).astype(np.float32)
    frames = frames * hann

    s_prev = np.zeros(n_frames, dtype=np.float32)
    s_prev2 = np.zeros(n_frames, dtype=np.float32)
    for n in range(win):
        s = frames[:, n] + coeff * s_prev - s_prev2
        s_prev2 = s_prev
        s_prev = s
    powers = s_prev * s_prev + s_prev2 * s_prev2 - coeff * s_prev * s_prev2
    return powers.astype(np.float32), hop


def _find_tone_extent(samples: np.ndarray, sr: int) -> tuple[float, float] | None:
    powers, hop = _goertzel_powers(samples, sr)
    if len(powers) == 0:
        return None
    smooth_n = max(1, 250 // HOP_MS)
    kernel = np.ones(smooth_n, dtype=np.float32) / smooth_n
    smoothed = np.convolve(powers, kernel, mode="same")

    noise = float(np.percentile(smoothed, 10))
    signal = float(np.percentile(smoothed, 95))
    if signal <= noise * 1.5:
        return None
    threshold = noise + 0.3 * (signal - noise)
    active = smoothed > threshold

    sustain_frames = max(1, SUSTAIN_MS // HOP_MS)
    silence_tail_frames = max(1, int(SILENCE_TAIL_S * 1000) // HOP_MS)

    first_idx = -1
    run = 0
    for i, a in enumerate(active):
        run = run + 1 if a else 0
        if run >= sustain_frames:
            first_idx = i - sustain_frames + 1
            break
    if first_idx < 0:
        return None

    last_idx = -1
    run_off = 0
    last_active = first_idx
    for i in range(first_idx, len(active)):
        if active[i]:
            last_active = i
            run_off = 0
        else:
            run_off += 1
            if run_off >= silence_tail_frames and last_active >= first_idx + sustain_frames:
                last_idx = last_active
                break
    if last_idx < 0:
        last_idx = last_active

    first_s = (first_idx * hop) / sr
    last_s = ((last_idx + 1) * hop) / sr
    return first_s, last_s


def _write_wav_mono(path: Path, samples: np.ndarray, sr: int) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    pcm = np.clip(samples * 32767.0, -32768, 32767).astype(np.int16)
    with wave.open(str(path), "wb") as w:
        w.setnchannels(1)
        w.setsampwidth(2)
        w.setframerate(sr)
        w.writeframes(pcm.tobytes())


# ---------------------------------------------------------------------------
# Worker entry point — must be top-level for ProcessPoolExecutor on Windows.
# ---------------------------------------------------------------------------


def trim_one(args: tuple[float, str]) -> dict:
    wpm, yymmdd_s = args
    paths = session_paths(wpm, yymmdd_s)
    sidecar = paths.trimmed_wav.with_suffix(".trim.json")

    result: dict = {"wpm": wpm, "yymmdd": yymmdd_s, "status": "skipped"}

    if not paths.mp3.exists():
        result["status"] = "no-mp3"
        return result

    if paths.trimmed_wav.exists() and sidecar.exists():
        try:
            meta = json.loads(sidecar.read_text(encoding="utf-8"))
            meta["status"] = "cached"
            return meta
        except Exception:
            pass

    try:
        samples = _decode_to_pcm(paths.mp3)
    except subprocess.CalledProcessError as exc:
        result["status"] = "ffmpeg-failed"
        result["error"] = (exc.stderr[:200].decode("utf-8", "replace") if exc.stderr else "")
        return result

    duration_s = len(samples) / SAMPLE_RATE
    if duration_s < 30:
        result["status"] = "too-short"
        result["original_s"] = round(duration_s, 2)
        return result

    extent = _find_tone_extent(samples, SAMPLE_RATE)
    if extent is None:
        first_s, last_s = 0.0, duration_s
        intro_stripped = False
        outro_stripped = False
    else:
        first_s, last_s = extent
        first_s = max(0.0, first_s - PRE_PAD_S)
        last_s = min(duration_s, last_s + POST_PAD_S)
        intro_stripped = first_s > 0.5
        outro_stripped = (duration_s - last_s) > 0.5

    start_idx = int(first_s * SAMPLE_RATE)
    end_idx = int(last_s * SAMPLE_RATE)
    trimmed = samples[start_idx:end_idx]
    if len(trimmed) < SAMPLE_RATE * 30:
        trimmed = samples
        first_s = 0.0
        last_s = duration_s
        intro_stripped = False
        outro_stripped = False

    _write_wav_mono(paths.trimmed_wav, trimmed, SAMPLE_RATE)

    meta = {
        "wpm": wpm,
        "yymmdd": yymmdd_s,
        "mp3": paths.mp3.name,
        "original_s": round(duration_s, 2),
        "trim_start_s": round(first_s, 2),
        "trim_end_s": round(last_s, 2),
        "trimmed_s": round(len(trimmed) / SAMPLE_RATE, 2),
        "intro_stripped": intro_stripped,
        "outro_stripped": outro_stripped,
        "sample_rate": SAMPLE_RATE,
        "status": "ok",
    }
    sidecar.write_text(json.dumps({k: v for k, v in meta.items() if k != "status"}, indent=2),
                       encoding="utf-8")
    return meta


def discover_sessions(speeds: list[float]) -> list[tuple[float, str]]:
    out: list[tuple[float, str]] = []
    for wpm in speeds:
        ensure_corpus_dirs(wpm)
        raw_dir = CORPUS_ROOT / speed_dirname(wpm) / "raw"
        if not raw_dir.exists():
            continue
        for mp3 in sorted(raw_dir.glob("*.mp3")):
            out.append((wpm, mp3.stem))
    return out


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--speeds", default=None)
    parser.add_argument("--workers", type=int, default=os.cpu_count() or 4)
    args = parser.parse_args(argv)

    if not have_ffmpeg():
        print("ERROR: ffmpeg not found on PATH.", file=sys.stderr)
        return 2

    logger = configure_logging()
    speeds = parse_speeds_arg(args.speeds)
    sessions = discover_sessions(speeds)
    if not sessions:
        logger.warning("trim: no MP3 files found; run download_parallel.py first")
        return 1

    logger.info(f"trim: {len(sessions)} sessions across speeds={speeds} workers={args.workers}")

    counts: dict[str, int] = {}
    completed = 0
    with ProcessPoolExecutor(max_workers=args.workers) as ex:
        futs = [ex.submit(trim_one, s) for s in sessions]
        for f in as_completed(futs):
            meta = f.result()
            counts[meta["status"]] = counts.get(meta["status"], 0) + 1
            completed += 1
            if completed % 10 == 0 or completed == len(sessions):
                logger.info(f"trim: {completed}/{len(sessions)} done — last: "
                            f"{meta.get('wpm')}wpm {meta.get('yymmdd')} {meta['status']}")

    logger.info(f"trim summary: {dict(sorted(counts.items()))}")
    ok = counts.get("ok", 0) + counts.get("cached", 0)
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
