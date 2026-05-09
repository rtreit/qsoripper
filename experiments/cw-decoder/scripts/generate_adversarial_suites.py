"""Generate adversarial CW test suites for decoder evaluation.

Each suite targets a specific failure mode and produces N WAV files plus
ground-truth text and a manifest. WAVs are written under
data/cw-samples/synthetic-adversarial/<suite>/ (gitignored). The combined
manifest is written next to this script.

Deterministic: every example uses a per-(suite, index) seeded RNG so the
suites can be regenerated bit-exactly.

Dependencies: numpy, soundfile (scipy not strictly required).

Usage:
    python generate_adversarial_suites.py [--out OUT_DIR] [--manifest PATH]
                                          [--n PER_SUITE] [--suite NAME]+
"""

from __future__ import annotations

import argparse
import json
import math
import random
from dataclasses import dataclass, field
from pathlib import Path
from typing import Callable, Iterable

import numpy as np
import soundfile as sf


# ---------------------------------------------------------------------------
# Constants

SAMPLE_RATE = 12_000  # decoder native rate; matches stream-region pipeline
DEFAULT_PITCH_HZ = 600.0

MORSE = {
    "A": ".-", "B": "-...", "C": "-.-.", "D": "-..", "E": ".", "F": "..-.",
    "G": "--.", "H": "....", "I": "..", "J": ".---", "K": "-.-", "L": ".-..",
    "M": "--", "N": "-.", "O": "---", "P": ".--.", "Q": "--.-", "R": ".-.",
    "S": "...", "T": "-", "U": "..-", "V": "...-", "W": ".--", "X": "-..-",
    "Y": "-.--", "Z": "--..",
    "0": "-----", "1": ".----", "2": "..---", "3": "...--", "4": "....-",
    "5": ".....", "6": "-....", "7": "--...", "8": "---..", "9": "----.",
    "/": "-..-.", "=": "-...-", ".": ".-.-.-", ",": "--..--", "?": "..--..",
}


# ---------------------------------------------------------------------------
# Element-level synthesis

@dataclass
class ElementEvent:
    """One emitted morse event with absolute sample timing.

    kind:
        'd'  -- dit  (key down)
        'D'  -- dah  (key down)
        'eg' -- intra-character gap (key up)
        'lg' -- inter-character gap (key up)
        'wg' -- inter-word gap (key up)
    """
    kind: str
    start: int
    length: int

    @property
    def end(self) -> int:
        return self.start + self.length

    @property
    def is_key_down(self) -> bool:
        return self.kind in ("d", "D")


def text_to_events(
    text: str,
    sample_rate: int,
    element_wpm: float,
    farnsworth_wpm: float | None = None,
    start_sample: int = 0,
) -> list[ElementEvent]:
    """Convert text to a list of ElementEvent with sample-accurate timing.

    PARIS-style timing: dot = 1.2 / wpm seconds.

    If farnsworth_wpm < element_wpm, intra-character timing uses element_wpm,
    while letter/word gaps are stretched to match overall farnsworth_wpm.
    """
    dot = 1.2 / float(element_wpm)
    elem_gap = dot
    if farnsworth_wpm is None or farnsworth_wpm >= element_wpm:
        letter_gap = 3.0 * dot
        word_gap = 7.0 * dot
    else:
        # Standard ARRL Farnsworth distribution: pad letter and word gaps so
        # that total time per "PARIS" word matches farnsworth_wpm.
        # Total per char-time at element_wpm: characters take (50 - 19) = 31
        # dot units; gaps take 19 dot units. Stretch the 19-unit gap budget.
        target_word_s = 60.0 / farnsworth_wpm / 5.0 * 5.0  # seconds per word
        # Simpler approximation: 3x and 7x at farnsworth wpm.
        f_dot = 1.2 / float(farnsworth_wpm)
        letter_gap = 3.0 * f_dot
        word_gap = 7.0 * f_dot

    dot_n = max(1, int(round(dot * sample_rate)))
    dah_n = max(1, int(round(3.0 * dot * sample_rate)))
    egap_n = max(1, int(round(elem_gap * sample_rate)))
    lgap_n = max(1, int(round(letter_gap * sample_rate)))
    wgap_n = max(1, int(round(word_gap * sample_rate)))

    events: list[ElementEvent] = []
    cur = start_sample
    words = text.upper().split()
    for wi, word in enumerate(words):
        for ci, ch in enumerate(word):
            sym = MORSE.get(ch)
            if sym is None:
                continue
            for ei, m in enumerate(sym):
                n = dot_n if m == "." else dah_n
                kind = "d" if m == "." else "D"
                events.append(ElementEvent(kind, cur, n))
                cur += n
                if ei < len(sym) - 1:
                    events.append(ElementEvent("eg", cur, egap_n))
                    cur += egap_n
            if ci < len(word) - 1:
                events.append(ElementEvent("lg", cur, lgap_n))
                cur += lgap_n
        if wi < len(words) - 1:
            events.append(ElementEvent("wg", cur, wgap_n))
            cur += wgap_n
    return events


def render_events(
    events: Iterable[ElementEvent],
    total_len: int,
    sample_rate: int,
    pitch_hz: float,
    amplitude: float = 0.4,
    rise_ms: float = 5.0,
) -> np.ndarray:
    """Render key-down events into a tone with raised-cosine edges."""
    out = np.zeros(total_len, dtype=np.float32)
    rise_n = max(1, int(rise_ms * 1e-3 * sample_rate))
    # Pre-build raised-cosine edge
    edge = 0.5 - 0.5 * np.cos(np.linspace(0.0, np.pi, rise_n, dtype=np.float32))
    t_axis = np.arange(total_len, dtype=np.float32) / sample_rate
    full_tone = np.sin(2.0 * np.pi * pitch_hz * t_axis).astype(np.float32)
    for ev in events:
        if not ev.is_key_down:
            continue
        s, e = ev.start, ev.end
        if s >= total_len:
            break
        e = min(e, total_len)
        n = e - s
        env = np.ones(n, dtype=np.float32)
        r = min(rise_n, n // 2 if n >= 2 else 1)
        if r > 0:
            env[:r] = edge[:r]
            env[-r:] = edge[:r][::-1]
        out[s:e] += amplitude * env * full_tone[s:e]
    return out


# ---------------------------------------------------------------------------
# Noise helpers

def white_noise(rng: random.Random, n: int, amp: float) -> np.ndarray:
    rs = np.random.default_rng(rng.randrange(0, 2**31 - 1))
    return (amp * rs.standard_normal(n)).astype(np.float32)


def pink_noise(rng: random.Random, n: int, amp: float) -> np.ndarray:
    """Pink-ish noise via Voss-McCartney with 16 octaves; cheap & dependency-free."""
    rs = np.random.default_rng(rng.randrange(0, 2**31 - 1))
    rows = 16
    array = np.empty((rows, n), dtype=np.float32)
    for r in range(rows):
        period = 1 << r
        vals = rs.standard_normal((n + period - 1) // period).astype(np.float32)
        array[r] = np.repeat(vals, period)[:n]
    pink = array.sum(axis=0) / rows
    pink -= pink.mean()
    pink /= max(1e-9, float(np.std(pink)))
    return (amp * pink).astype(np.float32)


def db_to_amp(db: float) -> float:
    return 10.0 ** (db / 20.0)


# ---------------------------------------------------------------------------
# Per-suite synthesis

@dataclass
class SuiteSpec:
    name: str
    description: str
    builder: Callable[["BuildCtx"], "Example"]
    n: int = 20


@dataclass
class BuildCtx:
    suite: str
    index: int
    rng: random.Random
    sample_rate: int = SAMPLE_RATE


@dataclass
class Example:
    truth: str
    samples: np.ndarray
    sample_rate: int
    meta: dict = field(default_factory=dict)


# Content pools ------------------------------------------------------------

CALLSIGNS = [
    "W7N", "K1ABC", "N0CALL", "VE3XYZ", "W5DX", "K7RAD", "WA6MOW", "AA6PW",
    "KC7AVA", "KB1HQ", "N9ZZ", "K3LR", "W2GD", "AA1K", "K6XX", "N7XM",
    "VA3PEN", "DL1ABC", "G3SXW", "JA1NUT",
]

CQ_TEMPLATES = [
    "CQ CQ DE {a} {a} K",
    "CQ DE {a} K",
    "CQ CQ CQ DE {a} {a} {a} K",
]

EXCHANGE_TEMPLATES = [
    "{a} DE {b} GM RST 599 NAME RANDY QTH WA K",
    "{a} DE {b} TNX UR 579 OM 73 SK",
    "{a} DE {b} RR FB QSO 73 GL SK",
    "{a} DE {b} GA UR 589 IN OR NAME LEE K",
]

ABBREVS = [
    "TNX FER QSO 73",
    "FB OM RST 599",
    "GM HW CPY",
    "QSB QRM AGN",
    "RIG K3 ANT DIPOLE",
    "WX SUNNY 25C",
    "PSE QSL VIA BURO",
]


def pick_text(rng: random.Random) -> str:
    bucket = rng.random()
    if bucket < 0.33:
        a = rng.choice(CALLSIGNS)
        return rng.choice(CQ_TEMPLATES).format(a=a)
    if bucket < 0.75:
        a, b = rng.sample(CALLSIGNS, 2)
        return rng.choice(EXCHANGE_TEMPLATES).format(a=a, b=b)
    return rng.choice(ABBREVS)


# Helpers ------------------------------------------------------------------

def synth_clean(text: str, sample_rate: int, wpm: float, pitch_hz: float,
                amp: float = 0.4, lead_ms: float = 200.0, trail_ms: float = 400.0,
                farnsworth_wpm: float | None = None,
                ) -> tuple[np.ndarray, list[ElementEvent]]:
    lead = int(lead_ms * 1e-3 * sample_rate)
    events = text_to_events(text, sample_rate, wpm, farnsworth_wpm,
                            start_sample=lead)
    trail = int(trail_ms * 1e-3 * sample_rate)
    total = (events[-1].end if events else lead) + trail
    audio = render_events(events, total, sample_rate, pitch_hz, amplitude=amp)
    return audio, events


# Suite builders -----------------------------------------------------------

def build_weak_prefix(ctx: BuildCtx) -> Example:
    text = pick_text(ctx.rng)
    wpm = ctx.rng.choice([18.0, 20.0, 22.0, 25.0])
    pitch = ctx.rng.choice([550.0, 600.0, 650.0, 700.0])
    audio, events = synth_clean(text, ctx.sample_rate, wpm, pitch, amp=0.5)
    n = len(audio)
    # Build a per-sample gain envelope: -10 dB SNR baseline floor for the
    # first ~200 ms of CW activity then ramp linearly back to clean over the
    # next 400 ms. We attenuate the first key-down events directly.
    weak_db = -10.0
    weak_amp = db_to_amp(weak_db)  # ~0.316 of nominal
    gain = np.ones(n, dtype=np.float32)
    if events:
        first_on = events[0].start
        ramp_end = first_on + int(0.6 * ctx.sample_rate)
        ramp_end = min(ramp_end, n)
        ramp_n = ramp_end - first_on
        if ramp_n > 0:
            ramp = np.linspace(weak_amp, 1.0, ramp_n, dtype=np.float32)
            gain[first_on:ramp_end] = ramp
            gain[:first_on] = weak_amp
    audio = audio * gain
    audio = audio + white_noise(ctx.rng, n, amp=0.05)
    truncated_chars = _chars_in_window(text, events, ctx.sample_rate, 0.3)
    meta = dict(
        wpm=wpm, pitch_hz=pitch,
        weak_db=weak_db,
        weak_window_s=0.6,
        first_chars_under_attack=truncated_chars,
    )
    return Example(text, audio.astype(np.float32), ctx.sample_rate, meta)


def _chars_in_window(text: str, events: list[ElementEvent], sr: int,
                     window_s: float) -> int:
    """Count how many characters of `text` are fully or partially inside the
    first `window_s` of CW activity."""
    if not events:
        return 0
    t0 = events[0].start
    cutoff = t0 + int(window_s * sr)
    chars = 0
    seen_chars = 0
    # Walk text, mirror the timing
    cur = t0
    dot = events[0].length  # rough; overridden below if dah
    # Much easier to just count by re-walking: re-derive boundaries.
    word_idx = char_in_word_idx = 0
    in_char = False
    boundaries = []  # absolute end-time of each character
    for ev in events:
        if ev.kind in ("d", "D"):
            in_char = True
            cur = ev.end
        elif ev.kind == "eg":
            cur = ev.end
        elif ev.kind in ("lg", "wg"):
            if in_char:
                boundaries.append(cur)
                in_char = False
    if in_char:
        boundaries.append(cur)
    return sum(1 for b in boundaries if b <= cutoff)


def build_mid_region_collapse(ctx: BuildCtx) -> Example:
    # Build a longer, multi-burst region so a 2 s drop sits in the middle.
    parts = [pick_text(ctx.rng) for _ in range(3)]
    text = " ".join(parts)
    wpm = ctx.rng.choice([18.0, 20.0, 22.0])
    pitch = ctx.rng.choice([580.0, 600.0, 620.0])
    audio, events = synth_clean(text, ctx.sample_rate, wpm, pitch, amp=0.5)
    n = len(audio)
    sr = ctx.sample_rate
    dur_s = n / sr
    drop_start_s = max(0.5, dur_s * 0.4)
    drop_len_s = 2.0
    drop_end_s = min(dur_s - 0.2, drop_start_s + drop_len_s)
    s = int(drop_start_s * sr)
    e = int(drop_end_s * sr)
    # 12 dB drop with short ramp edges to avoid clicks
    gain = np.ones(n, dtype=np.float32)
    drop_amp = db_to_amp(-12.0)
    edge_n = int(0.05 * sr)
    if e - s > 2 * edge_n:
        gain[s:s + edge_n] = np.linspace(1.0, drop_amp, edge_n)
        gain[s + edge_n:e - edge_n] = drop_amp
        gain[e - edge_n:e] = np.linspace(drop_amp, 1.0, edge_n)
    audio = audio * gain
    audio = audio + white_noise(ctx.rng, n, amp=0.04)
    meta = dict(
        wpm=wpm, pitch_hz=pitch,
        drop_start_s=round(drop_start_s, 3),
        drop_end_s=round(drop_end_s, 3),
        drop_db=-12.0,
        silence_window_s=round(drop_end_s - drop_start_s, 3),
    )
    return Example(text, audio.astype(np.float32), sr, meta)


def build_qrm_same_pitch(ctx: BuildCtx) -> Example:
    # Two stations at the same pitch with ~50% temporal overlap. We treat
    # the louder (primary) text as the truth; the secondary is interferer.
    primary_text = pick_text(ctx.rng)
    secondary_text = pick_text(ctx.rng)
    wpm_a = ctx.rng.choice([18.0, 20.0, 22.0])
    wpm_b = ctx.rng.choice([16.0, 19.0, 24.0])
    pitch = ctx.rng.choice([590.0, 605.0, 615.0])
    a, _ev_a = synth_clean(primary_text, ctx.sample_rate, wpm_a, pitch, amp=0.5)
    b, _ev_b = synth_clean(secondary_text, ctx.sample_rate, wpm_b, pitch, amp=0.35)
    # Align so secondary starts ~50% into primary
    overlap_offset = len(a) // 2
    n = max(len(a), overlap_offset + len(b))
    out = np.zeros(n, dtype=np.float32)
    out[:len(a)] += a
    out[overlap_offset:overlap_offset + len(b)] += b
    out = out + white_noise(ctx.rng, n, amp=0.04)
    meta = dict(
        wpm_primary=wpm_a, wpm_secondary=wpm_b, pitch_hz=pitch,
        primary_amp=0.5, secondary_amp=0.35, overlap_offset_s=overlap_offset / ctx.sample_rate,
        secondary_text=secondary_text,
    )
    return Example(primary_text, out.astype(np.float32), ctx.sample_rate, meta)


def build_off_pitch_start(ctx: BuildCtx) -> Example:
    text = pick_text(ctx.rng)
    wpm = ctx.rng.choice([18.0, 20.0, 22.0])
    target_pitch = ctx.rng.choice([580.0, 600.0, 620.0])
    drift_hz = ctx.rng.choice([-100.0, 100.0])
    sr = ctx.sample_rate
    lead_ms = 200.0
    lead = int(lead_ms * 1e-3 * sr)
    events = text_to_events(text, sr, wpm, start_sample=lead)
    trail = int(0.4 * sr)
    n = (events[-1].end if events else lead) + trail
    drift_end = int(min(n, lead + 1.0 * sr))
    inst_freq = np.full(n, target_pitch, dtype=np.float32)
    if drift_end > lead:
        # Linear chirp from (target + drift_hz) -> target across first second
        inst_freq[lead:drift_end] = np.linspace(
            target_pitch + drift_hz, target_pitch, drift_end - lead, dtype=np.float32)
        inst_freq[:lead] = target_pitch + drift_hz
    # Integrate frequency to get phase
    phase = 2.0 * np.pi * np.cumsum(inst_freq) / sr
    tone = np.sin(phase).astype(np.float32)
    # Apply key-down envelope
    rise_n = max(1, int(0.005 * sr))
    edge = 0.5 - 0.5 * np.cos(np.linspace(0.0, np.pi, rise_n, dtype=np.float32))
    audio = np.zeros(n, dtype=np.float32)
    for ev in events:
        if not ev.is_key_down:
            continue
        s = ev.start
        e = min(ev.end, n)
        seg = e - s
        env = np.ones(seg, dtype=np.float32)
        r = min(rise_n, seg // 2 if seg >= 2 else 1)
        if r > 0:
            env[:r] = edge[:r]
            env[-r:] = edge[:r][::-1]
        audio[s:e] += 0.45 * env * tone[s:e]
    audio = audio + white_noise(ctx.rng, n, amp=0.03)
    meta = dict(wpm=wpm, target_pitch_hz=target_pitch, drift_hz=drift_hz,
                drift_window_s=1.0)
    return Example(text, audio, sr, meta)


def build_farnsworth_extremes(ctx: BuildCtx) -> Example:
    text = pick_text(ctx.rng)
    pitch = ctx.rng.choice([580.0, 600.0, 620.0])
    audio, _events = synth_clean(text, ctx.sample_rate,
                                 wpm=30.0, pitch_hz=pitch,
                                 farnsworth_wpm=10.0,
                                 amp=0.5,
                                 lead_ms=300.0, trail_ms=500.0)
    audio = audio + white_noise(ctx.rng, len(audio), amp=0.03)
    meta = dict(element_wpm=30.0, farnsworth_wpm=10.0, pitch_hz=pitch)
    return Example(text, audio.astype(np.float32), ctx.sample_rate, meta)


def build_noise_only(ctx: BuildCtx) -> Example:
    sr = ctx.sample_rate
    dur_s = ctx.rng.uniform(8.0, 14.0)
    n = int(dur_s * sr)
    flavor = ctx.rng.choice(["white", "pink", "white_loud"])
    if flavor == "white":
        audio = white_noise(ctx.rng, n, amp=0.08)
    elif flavor == "white_loud":
        audio = white_noise(ctx.rng, n, amp=0.15)
    else:
        audio = pink_noise(ctx.rng, n, amp=0.12)
    meta = dict(flavor=flavor, duration_s=round(dur_s, 3))
    # Truth is empty by design.
    return Example("", audio.astype(np.float32), sr, meta)


def build_slow_arrl_style(ctx: BuildCtx) -> Example:
    text = pick_text(ctx.rng)
    wpm = [5.0, 10.0, 13.0, 15.0][ctx.index % 4]
    pitch = ctx.rng.choice([580.0, 600.0, 620.0, 700.0])
    audio, _ev = synth_clean(text, ctx.sample_rate, wpm, pitch, amp=0.5,
                             lead_ms=300.0, trail_ms=500.0)
    audio = audio + white_noise(ctx.rng, len(audio), amp=0.025)
    meta = dict(wpm=wpm, pitch_hz=pitch)
    return Example(text, audio.astype(np.float32), ctx.sample_rate, meta)


def build_fast_contest(ctx: BuildCtx) -> Example:
    # Short contest-style exchange at high WPM.
    a, b = ctx.rng.sample(CALLSIGNS, 2)
    text = ctx.rng.choice([
        f"{a} DE {b} 599 WA K",
        f"{a} DE {b} TU 5NN K",
        f"CQ TEST {a} {a}",
        f"{a} 599 001 K",
    ])
    wpm = [35.0, 40.0, 45.0][ctx.index % 3]
    pitch = ctx.rng.choice([580.0, 600.0, 650.0])
    audio, _ev = synth_clean(text, ctx.sample_rate, wpm, pitch, amp=0.5,
                             lead_ms=150.0, trail_ms=300.0)
    audio = audio + white_noise(ctx.rng, len(audio), amp=0.025)
    meta = dict(wpm=wpm, pitch_hz=pitch)
    return Example(text, audio.astype(np.float32), ctx.sample_rate, meta)


SUITES: list[SuiteSpec] = [
    SuiteSpec("weak-prefix", "First ~200ms of CW activity at -10 dB SNR, ramp to clean by 600ms.", build_weak_prefix),
    SuiteSpec("mid-region-collapse", "Middle of long region drops 12 dB for 2 s then recovers.", build_mid_region_collapse),
    SuiteSpec("qrm-same-pitch", "Two stations at same pitch with ~50% temporal overlap.", build_qrm_same_pitch),
    SuiteSpec("off-pitch-start", "Pitch drifts +/-100 Hz over first 1 s, stable after.", build_off_pitch_start),
    SuiteSpec("farnsworth-extremes", "Element WPM 30, overall Farnsworth 10 WPM.", build_farnsworth_extremes),
    SuiteSpec("noise-only", "White or pink noise, no CW. Truth is empty.", build_noise_only),
    SuiteSpec("slow-arrl-style", "Clean ARRL-style 5/10/13/15 WPM signals.", build_slow_arrl_style),
    SuiteSpec("fast-contest", "35/40/45 WPM short contest exchanges.", build_fast_contest),
]


# ---------------------------------------------------------------------------
# Driver

DEFAULT_OUT = Path(r"C:\Users\randy\Git\qsoripper-experiments\adversarial-suite\data\cw-samples\synthetic-adversarial")
DEFAULT_MANIFEST = Path(__file__).with_name("adversarial_manifest.jsonl")
import zlib
SEED_BASE = 0xC0DEC0DE
REPO_ROOT = Path(__file__).resolve().parents[3]


def _stable_suite_seed(name: str) -> int:
    return zlib.crc32(name.encode("utf-8")) & 0xFFFFFFFF


def write_wav(path: Path, sample_rate: int, samples: np.ndarray) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    # Clip safely to [-1, 1) and write 16-bit PCM (matches decoder native).
    clipped = np.clip(samples, -0.999, 0.999).astype(np.float32)
    sf.write(str(path), clipped, sample_rate, subtype="PCM_16")


def events_to_compact(events: list[ElementEvent]) -> list[dict]:
    """Compact element list: only key-down events, with sample indices."""
    return [
        {"k": ev.kind, "s": ev.start, "n": ev.length}
        for ev in events
        if ev.is_key_down
    ]


def truth_events(text: str, sample_rate: int, wpm: float,
                 farnsworth_wpm: float | None = None) -> list[ElementEvent]:
    return text_to_events(text, sample_rate, wpm, farnsworth_wpm)


def main() -> None:
    p = argparse.ArgumentParser()
    p.add_argument("--out", type=Path, default=DEFAULT_OUT)
    p.add_argument("--manifest", type=Path, default=DEFAULT_MANIFEST)
    p.add_argument("--n", type=int, default=20)
    p.add_argument("--suite", action="append", default=None,
                   help="restrict to one or more suites (repeatable)")
    args = p.parse_args()

    args.out.mkdir(parents=True, exist_ok=True)
    manifest_lines: list[str] = []
    for suite in SUITES:
        if args.suite and suite.name not in args.suite:
            continue
        suite_dir = args.out / suite.name
        suite_dir.mkdir(parents=True, exist_ok=True)
        for i in range(args.n):
            seed = SEED_BASE ^ _stable_suite_seed(suite.name) ^ (i * 2654435761 & 0xFFFFFFFF)
            rng = random.Random(seed & 0xFFFFFFFF)
            ctx = BuildCtx(suite.name, i, rng)
            ex = suite.builder(ctx)
            example_id = f"{suite.name}-{i:02d}"
            wav_path = suite_dir / f"{example_id}.wav"
            write_wav(wav_path, ex.sample_rate, ex.samples)
            truth_path = suite_dir / f"{example_id}.truth.txt"
            truth_path.write_text(ex.truth, encoding="utf-8")
            try:
                wav_rel = str(wav_path.resolve().relative_to(REPO_ROOT)).replace("\\", "/")
            except ValueError:
                wav_rel = str(wav_path).replace("\\", "/")

            # Compute truth element events (in the absence of impairment).
            wpm_for_truth = ex.meta.get("wpm") or ex.meta.get("element_wpm") or ex.meta.get("wpm_primary") or 20.0
            farns = ex.meta.get("farnsworth_wpm")
            truth_evs = truth_events(ex.truth, ex.sample_rate,
                                     float(wpm_for_truth), farns)
            n_dits = sum(1 for ev in truth_evs if ev.kind == "d")
            n_dahs = sum(1 for ev in truth_evs if ev.kind == "D")

            row = {
                "suite": suite.name,
                "id": example_id,
                "seed": seed & 0xFFFFFFFF,
                "wav": wav_rel,
                "truth": ex.truth,
                "sample_rate": ex.sample_rate,
                "duration_s": round(len(ex.samples) / ex.sample_rate, 4),
                "n_dits": n_dits,
                "n_dahs": n_dahs,
                "meta": ex.meta,
            }
            manifest_lines.append(json.dumps(row, sort_keys=True))
        print(f"[{suite.name}] {args.n} examples -> {suite_dir}")

    args.manifest.write_text("\n".join(manifest_lines) + "\n", encoding="utf-8")
    print(f"manifest -> {args.manifest} ({len(manifest_lines)} rows)")


if __name__ == "__main__":
    main()
