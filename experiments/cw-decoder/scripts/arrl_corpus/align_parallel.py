"""Stage 3: parallel align + slice using ProcessPoolExecutor.

Each worker invokes its own ``cw-decoder.exe stream-region`` subprocess on a
trimmed WAV, then runs Levenshtein-anchored alignment against the truth file
and emits per-chunk slices. Reuses the algorithm from the prior pilot
(``align_slice.py``) and parallelizes per-session work.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import subprocess
import sys
import wave
from concurrent.futures import ProcessPoolExecutor, as_completed
from dataclasses import dataclass
from difflib import SequenceMatcher
from pathlib import Path

import numpy as np

from common import (
    CORPUS_ROOT,
    DECODER_BIN,
    DEFAULT_SPEEDS,
    configure_logging,
    ensure_corpus_dirs,
    iso,
    normalize_decoded,
    normalize_truth,
    parse_speeds_arg,
    parse_yymmdd,
    relpath_for_manifest,
    session_paths,
    speed_dirname,
)

CHUNK_TARGET_CHARS = 80
CHUNK_MIN_S = 5.0
CHUNK_MAX_S = 90.0
CHUNK_DROP_CER = 0.05
WHOLE_FILE_DROP_CER = 0.5
DECODER_TIMEOUT_S = 600


def _run_decoder(wav: Path) -> str:
    cmd = [
        str(DECODER_BIN),
        "stream-region",
        "--file", str(wav),
        "--json",
        "--no-realtime",
        "--decode-every-ms", "3600000",
    ]
    try:
        proc = subprocess.run(
            cmd,
            check=True,
            capture_output=True,
            timeout=DECODER_TIMEOUT_S,
            text=True,
            encoding="utf-8",
            errors="replace",
        )
    except (subprocess.CalledProcessError, subprocess.TimeoutExpired):
        return ""

    final = ""
    for line in proc.stdout.splitlines():
        line = line.strip()
        if not line or not line.startswith("{"):
            continue
        try:
            evt = json.loads(line)
        except json.JSONDecodeError:
            continue
        t = evt.get("transcript")
        if isinstance(t, str) and t.strip():
            final = t
        elif isinstance(t, dict):
            for key in ("text", "value", "full"):
                v = t.get(key)
                if isinstance(v, str) and v.strip():
                    final = v
                    break
    return final


# ---------------------------------------------------------------------------
# Truth chunking
# ---------------------------------------------------------------------------

_SENTENCE_SPLIT_RE = re.compile(r"(?<=[.!?])\s+")


@dataclass
class TruthChunk:
    start_char: int
    end_char: int
    text: str


def _split_long_sentence(s: str, target: int) -> list[str]:
    if len(s) <= target * 2:
        return [s]
    parts = re.split(r"(?<=,)\s+", s)
    out: list[str] = []
    buf: list[str] = []
    cur_len = 0
    for p in parts:
        if cur_len + len(p) + 1 > target * 1.5 and buf:
            out.append(" ".join(buf))
            buf = [p]
            cur_len = len(p)
        else:
            buf.append(p)
            cur_len += len(p) + 1
    if buf:
        out.append(" ".join(buf))
    final: list[str] = []
    for piece in out:
        if len(piece) <= target * 2:
            final.append(piece)
            continue
        words = piece.split()
        cur: list[str] = []
        cur_len = 0
        for w in words:
            if cur_len + len(w) + 1 > target and cur:
                final.append(" ".join(cur))
                cur = [w]
                cur_len = len(w)
            else:
                cur.append(w)
                cur_len += len(w) + 1
        if cur:
            final.append(" ".join(cur))
    return final


def chunk_truth(truth: str, target: int = CHUNK_TARGET_CHARS) -> list[TruthChunk]:
    paragraphs = re.split(r"\n\s*\n", truth)
    sentences: list[str] = []
    for p in paragraphs:
        p = p.replace("\n", " ").strip()
        if not p:
            continue
        for s in _SENTENCE_SPLIT_RE.split(p):
            s = s.strip()
            if s:
                sentences.extend(_split_long_sentence(s, target))
    if not sentences:
        sentences = [s.strip() for s in truth.splitlines() if s.strip()]

    chunks: list[TruthChunk] = []
    cursor = 0
    buf: list[str] = []
    buf_start = 0

    def find_pos(needle: str, start: int) -> int:
        idx = truth.find(needle, start)
        return idx if idx >= 0 else start

    for s in sentences:
        if not buf:
            buf_start = find_pos(s, cursor)
            buf.append(s)
            continue
        candidate = " ".join(buf) + " " + s
        if len(candidate) > target * 1.5:
            text = " ".join(buf)
            start = buf_start
            end = start + len(text)
            chunks.append(TruthChunk(start, end, text))
            cursor = end
            buf_start = find_pos(s, cursor)
            buf = [s]
        else:
            buf.append(s)
            if len(candidate) >= target:
                text = candidate
                start = buf_start
                end = start + len(text)
                chunks.append(TruthChunk(start, end, text))
                cursor = end
                buf = []
    if buf:
        text = " ".join(buf)
        start = buf_start
        end = start + len(text)
        chunks.append(TruthChunk(start, end, text))
    return chunks


def cer(a: str, b: str) -> float:
    if not b:
        return 0.0 if not a else 1.0
    matcher = SequenceMatcher(None, a, b, autojunk=False)
    matches = sum(blk.size for blk in matcher.get_matching_blocks())
    distance = max(len(a), len(b)) - matches
    return distance / len(b)


# ---------------------------------------------------------------------------
# WAV IO
# ---------------------------------------------------------------------------


def _read_wav_mono(path: Path) -> tuple[np.ndarray, int]:
    with wave.open(str(path), "rb") as w:
        sr = w.getframerate()
        n = w.getnframes()
        raw = w.readframes(n)
    return np.frombuffer(raw, dtype=np.int16), sr


def _write_wav_slice(path: Path, samples: np.ndarray, sr: int) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with wave.open(str(path), "wb") as w:
        w.setnchannels(1)
        w.setsampwidth(2)
        w.setframerate(sr)
        w.writeframes(samples.tobytes())


# ---------------------------------------------------------------------------
# Worker entry point
# ---------------------------------------------------------------------------


def align_one(args: tuple[float, str]) -> dict:
    wpm, yymmdd_s = args
    paths = session_paths(wpm, yymmdd_s)
    summary = {
        "wpm": wpm,
        "yymmdd": yymmdd_s,
        "status": "skipped",
        "whole_file_cer": None,
        "chunks_emitted": 0,
        "chunks_dropped_cer": 0,
        "chunks_dropped_duration": 0,
    }

    out_jsonl = paths.chunks_dir / f"{yymmdd_s}.chunks.jsonl"
    if out_jsonl.exists() and out_jsonl.stat().st_size > 0:
        # Skip when chunks already produced.
        try:
            n = sum(1 for _ in out_jsonl.open("r", encoding="utf-8"))
        except Exception:
            n = 0
        summary["status"] = "cached"
        summary["chunks_emitted"] = n
        return summary

    if not paths.trimmed_wav.exists():
        summary["status"] = "no-wav"
        return summary
    if not paths.truth.exists():
        summary["status"] = "no-truth"
        return summary

    truth = normalize_truth(paths.truth.read_text(encoding="utf-8", errors="replace"))
    if not truth:
        summary["status"] = "empty-truth"
        return summary

    samples, sr = _read_wav_mono(paths.trimmed_wav)
    audio_duration_s = len(samples) / sr
    if audio_duration_s < 30:
        summary["status"] = "audio-too-short"
        return summary

    decoded = normalize_decoded(_run_decoder(paths.trimmed_wav))
    if not decoded:
        summary["status"] = "decoder-empty"
        return summary

    file_cer = cer(decoded, truth)
    summary["whole_file_cer"] = round(file_cer, 4)
    if file_cer > WHOLE_FILE_DROP_CER:
        summary["status"] = "whole-file-poor"
        return summary

    matcher = SequenceMatcher(None, truth, decoded, autojunk=False)
    anchors = [(0, 0)]
    for blk in matcher.get_matching_blocks():
        if blk.size > 0:
            anchors.append((blk.a, blk.b))
    anchors.append((len(truth), len(decoded)))
    anchors = sorted(set(anchors))

    def truth_to_decoded(pos: int) -> int:
        lo = 0
        hi = len(anchors) - 1
        while lo + 1 < hi:
            mid = (lo + hi) // 2
            if anchors[mid][0] <= pos:
                lo = mid
            else:
                hi = mid
        a0, b0 = anchors[lo]
        a1, b1 = anchors[hi]
        if a1 == a0:
            return b0
        frac = (pos - a0) / (a1 - a0)
        return int(b0 + frac * (b1 - b0))

    truth_chunks = chunk_truth(truth)
    total_decoded_chars = max(1, len(decoded))

    paths.chunks_dir.mkdir(parents=True, exist_ok=True)
    rows: list[dict] = []

    for seq, ch in enumerate(truth_chunks):
        d_start = truth_to_decoded(ch.start_char)
        d_end = truth_to_decoded(ch.end_char)
        if d_end <= d_start:
            summary["chunks_dropped_duration"] += 1
            continue
        t_start = (d_start / total_decoded_chars) * audio_duration_s
        t_end = (d_end / total_decoded_chars) * audio_duration_s
        duration_s = t_end - t_start
        if duration_s < CHUNK_MIN_S or duration_s > CHUNK_MAX_S:
            summary["chunks_dropped_duration"] += 1
            continue

        decoded_chunk = decoded[d_start:d_end]
        chunk_cer = cer(decoded_chunk, ch.text)
        if chunk_cer > CHUNK_DROP_CER:
            summary["chunks_dropped_cer"] += 1
            continue

        s_start = max(0, int(t_start * sr))
        s_end = min(len(samples), int(t_end * sr))
        if s_end <= s_start:
            summary["chunks_dropped_duration"] += 1
            continue
        chunk_samples = samples[s_start:s_end]

        wav_path = paths.chunks_dir / f"{yymmdd_s}_{seq:04d}.wav"
        _write_wav_slice(wav_path, chunk_samples, sr)

        rows.append({
            "wav_path": relpath_for_manifest(wav_path),
            "text": ch.text,
            "wpm": wpm,
            "date": iso(parse_yymmdd(yymmdd_s)),
            "alignment_score": round(chunk_cer, 4),
            "duration_s": round(len(chunk_samples) / sr, 3),
            "sample_rate": sr,
            "char_count": len(ch.text),
            "source_file": paths.mp3.name,
        })

    out_jsonl.write_text(
        "\n".join(json.dumps(r, ensure_ascii=False) for r in rows) + ("\n" if rows else ""),
        encoding="utf-8",
    )
    summary["chunks_emitted"] = len(rows)
    summary["status"] = "ok" if rows else "no-chunks"
    return summary


def discover_sessions(speeds: list[float]) -> list[tuple[float, str]]:
    out: list[tuple[float, str]] = []
    for wpm in speeds:
        ensure_corpus_dirs(wpm)
        td = CORPUS_ROOT / speed_dirname(wpm) / "trimmed"
        if not td.exists():
            continue
        for wav in sorted(td.glob("*.wav")):
            out.append((wpm, wav.stem))
    return out


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--speeds", default=None)
    parser.add_argument("--workers", type=int, default=os.cpu_count() or 4)
    args = parser.parse_args(argv)

    if not DECODER_BIN.exists():
        print(f"ERROR: decoder binary missing: {DECODER_BIN}\n"
              f"Run: cargo build --release --bin cw-decoder", file=sys.stderr)
        return 2

    logger = configure_logging()
    speeds = parse_speeds_arg(args.speeds)
    sessions = discover_sessions(speeds)
    if not sessions:
        logger.warning("align: no trimmed wavs found; run trim_parallel.py first")
        return 1

    logger.info(f"align: {len(sessions)} sessions across speeds={speeds} workers={args.workers}")

    counts: dict[str, int] = {}
    total_chunks = 0
    completed = 0
    with ProcessPoolExecutor(max_workers=args.workers) as ex:
        futs = [ex.submit(align_one, s) for s in sessions]
        for f in as_completed(futs):
            meta = f.result()
            counts[meta["status"]] = counts.get(meta["status"], 0) + 1
            total_chunks += meta["chunks_emitted"]
            completed += 1
            if completed % 5 == 0 or completed == len(sessions):
                logger.info(
                    f"align: {completed}/{len(sessions)} — last: "
                    f"{meta['wpm']}wpm {meta['yymmdd']} {meta['status']} "
                    f"chunks={meta['chunks_emitted']} cer={meta['whole_file_cer']}"
                )

    logger.info(f"align summary: {dict(sorted(counts.items()))}, total_chunks={total_chunks}")
    return 0 if total_chunks else 1


if __name__ == "__main__":
    sys.exit(main())
