"""Generate a small Kaggle-shape synthetic mini-suite to smoke-test the bench
harness without requiring Kaggle credentials.

Produces N WAV files matching the competition's format and randomization
envelope:
- 8 kHz, mono, 16-bit PCM (kaggle is float32; CW decoder accepts both)
- Per-file SNR in [-14, +20] dB (uniform)
- Per-file pitch in [600, 1200] Hz (uniform)
- Per-file speed in [12, 80] WPM (uniform)
- Per-file truth: short random alphanumeric phrase

Writes:
- synthetic_minisuite/cw001.wav .. cwNNN.wav
- synthetic_minisuite_manifest.jsonl  (compatible with bench.py)

Usage:
    python generate_synthetic_minisuite.py [--n 12] [--seed 42]
"""

from __future__ import annotations

import argparse
import json
import math
import random
import struct
import wave
from pathlib import Path

OUT_DIR = Path(__file__).with_name("synthetic_minisuite")
MANIFEST = Path(__file__).with_name("synthetic_minisuite_manifest.jsonl")

MORSE = {
    "A": ".-", "B": "-...", "C": "-.-.", "D": "-..", "E": ".", "F": "..-.",
    "G": "--.", "H": "....", "I": "..", "J": ".---", "K": "-.-", "L": ".-..",
    "M": "--", "N": "-.", "O": "---", "P": ".--.", "Q": "--.-", "R": ".-.",
    "S": "...", "T": "-", "U": "..-", "V": "...-", "W": ".--", "X": "-..-",
    "Y": "-.--", "Z": "--..",
    "0": "-----", "1": ".----", "2": "..---", "3": "...--", "4": "....-",
    "5": ".....", "6": "-....", "7": "--...", "8": "---..", "9": "----.",
}

CALLSIGN_PARTS = ["W1AW", "K3LR", "N5XYZ", "AA6PW", "WA6MOW", "K7DX", "VE3ABC"]
PHRASES = ["CQ CQ DE", "599 TU 73", "POTA K-1234", "TEST DE", "QRZ QRZ"]

SAMPLE_RATE = 8000


def _phrase(rng: random.Random) -> str:
    return f"{rng.choice(PHRASES)} {rng.choice(CALLSIGN_PARTS)}"


def _render(text: str, wpm: float, pitch_hz: float, snr_db: float, rng: random.Random) -> bytes:
    """Render `text` as 16-bit PCM bytes at SAMPLE_RATE.

    PARIS standard: dit length = 1.2 / WPM seconds.
    """
    dit_s = 1.2 / wpm
    dah_s = 3.0 * dit_s
    intra_s = dit_s        # gap between elements within a character
    inter_s = 3.0 * dit_s  # gap between characters
    word_s = 7.0 * dit_s   # gap between words

    sig_amp = 0.5  # tone amplitude before noise mix
    samples: list[float] = []

    def append_silence(seconds: float) -> None:
        n = int(seconds * SAMPLE_RATE)
        samples.extend([0.0] * n)

    def append_tone(seconds: float) -> None:
        n = int(seconds * SAMPLE_RATE)
        # 5 ms cosine ramp on each end to suppress click artifacts (matches
        # what human keyers and the Kaggle synth would do).
        ramp_n = max(1, int(0.005 * SAMPLE_RATE))
        for i in range(n):
            phase = 2.0 * math.pi * pitch_hz * (i / SAMPLE_RATE)
            env = 1.0
            if i < ramp_n:
                env = 0.5 - 0.5 * math.cos(math.pi * i / ramp_n)
            elif i > n - ramp_n:
                env = 0.5 - 0.5 * math.cos(math.pi * (n - i) / ramp_n)
            samples.append(sig_amp * env * math.sin(phase))

    # Pre-roll silence so onset detector can warm up.
    append_silence(0.5)
    words = text.split()
    for wi, word in enumerate(words):
        for ci, ch in enumerate(word):
            sym = MORSE.get(ch.upper())
            if sym is None:
                continue
            for ei, e in enumerate(sym):
                if e == ".":
                    append_tone(dit_s)
                else:
                    append_tone(dah_s)
                if ei < len(sym) - 1:
                    append_silence(intra_s)
            if ci < len(word) - 1:
                append_silence(inter_s)
        if wi < len(words) - 1:
            append_silence(word_s)
    append_silence(0.5)

    # Noise mix at SNR_db relative to mean tone power (tone-on segments only,
    # which approximates the per-tone definition used by the Kaggle synth).
    tone_power = (sig_amp / math.sqrt(2.0)) ** 2  # mean square of full-amp tone
    snr_lin = 10.0 ** (snr_db / 10.0)
    noise_var = tone_power / max(snr_lin, 1e-9)
    noise_sigma = math.sqrt(noise_var)
    out_floats = [s + rng.gauss(0.0, noise_sigma) for s in samples]
    peak = max(abs(s) for s in out_floats) or 1.0
    norm = 0.95 / peak  # avoid clipping after noise add
    out_int = [max(-32768, min(32767, int(round(s * norm * 32767)))) for s in out_floats]
    return struct.pack(f"<{len(out_int)}h", *out_int)


def main() -> None:
    p = argparse.ArgumentParser()
    p.add_argument("--n", type=int, default=12,
                   help="Number of synthetic files to generate (default 12).")
    p.add_argument("--seed", type=int, default=42)
    p.add_argument("--out-dir", type=Path, default=OUT_DIR)
    p.add_argument("--manifest", type=Path, default=MANIFEST)
    args = p.parse_args()

    rng = random.Random(args.seed)
    args.out_dir.mkdir(parents=True, exist_ok=True)

    rows = []
    for i in range(1, args.n + 1):
        cw_id = f"cw{i:03d}"
        text = _phrase(rng)
        wpm = rng.uniform(12.0, 80.0)
        pitch_hz = rng.uniform(600.0, 1200.0)
        snr_db = rng.uniform(-14.0, 20.0)
        pcm = _render(text, wpm, pitch_hz, snr_db, rng)
        wav_path = args.out_dir / f"{cw_id}.wav"
        with wave.open(str(wav_path), "wb") as w:
            w.setnchannels(1)
            w.setsampwidth(2)
            w.setframerate(SAMPLE_RATE)
            w.writeframes(pcm)
        rows.append({
            "id": cw_id,
            "wav": str(wav_path.resolve()),
            "truth": text.upper(),
            "split": "train",
            "synth_params": {
                "wpm": round(wpm, 1),
                "pitch_hz": round(pitch_hz, 1),
                "snr_db": round(snr_db, 1),
            },
        })

    with args.manifest.open("w", encoding="utf-8") as f:
        for r in rows:
            f.write(json.dumps(r) + "\n")

    print(f"Generated {len(rows)} files under {args.out_dir}")
    print(f"Manifest -> {args.manifest}")


if __name__ == "__main__":
    main()
