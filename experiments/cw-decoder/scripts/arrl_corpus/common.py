"""Shared helpers for the index-driven ARRL CW corpus pipeline.

All paths are anchored to the repository root so the scripts work regardless
of the working directory the orchestrator is launched from.
"""

from __future__ import annotations

import json
import logging
import os
import re
import sys
from dataclasses import dataclass
from datetime import date, datetime
from pathlib import Path
from typing import Iterable

# ---------------------------------------------------------------------------
# Paths
# ---------------------------------------------------------------------------

SCRIPT_DIR = Path(__file__).resolve().parent
# .../experiments/cw-decoder/scripts/arrl_corpus -> repo root is 4 levels up.
REPO_ROOT = SCRIPT_DIR.parents[3]
CW_DECODER_DIR = REPO_ROOT / "experiments" / "cw-decoder"
DECODER_BIN = CW_DECODER_DIR / "target" / "release" / (
    "cw-decoder.exe" if os.name == "nt" else "cw-decoder"
)

CORPUS_ROOT = REPO_ROOT / "data" / "cw-samples" / "arrl-archive"
INDEX_PATH = CORPUS_ROOT / "index.jsonl"
MANIFEST_PATH = CORPUS_ROOT / "manifest.jsonl"
SAMPLE_MANIFEST_PATH = SCRIPT_DIR / "sample_manifest.jsonl"
QUALITY_REPORT_PATH = SCRIPT_DIR / "quality_report.md"
PIPELINE_LOG = SCRIPT_DIR / "pipeline.log"

ARRL_BASE_URL = "https://www.arrl.org"

# Speeds the pilot harvests by default. 5/7.5/10/13 WPM frequently return the
# CDN error page on the older entries; opt in via --speeds if you really want
# them.
DEFAULT_SPEEDS: tuple[float, ...] = (15.0, 20.0, 25.0, 30.0)
ALL_SPEEDS: tuple[float, ...] = (5.0, 7.5, 10.0, 13.0, 15.0, 18.0, 20.0, 25.0, 30.0, 35.0, 40.0)

USER_AGENT = (
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) "
    "AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0 Safari/537.36 "
    "QsoRipper-ARRL-Corpus/1.0"
)


# ---------------------------------------------------------------------------
# WPM helpers
# ---------------------------------------------------------------------------


def speed_url_slug(wpm: float) -> str:
    """Return the URL slug for an archive page (handles 7.5 -> 7pt5)."""

    if float(wpm).is_integer():
        return f"{int(wpm)}-wpm-code-archive"
    s = str(wpm).replace(".", "pt")
    return f"{s}-wpm-code-archive"


def speed_dirname(wpm: float) -> str:
    """Return a filesystem-safe directory name for a speed."""

    if float(wpm).is_integer():
        return f"{int(wpm)}wpm"
    return f"{str(wpm).replace('.', '_')}wpm"


def speed_filename_token(wpm: float) -> str:
    """Token used in MP3 filenames, e.g. ``20WPM`` or ``7.5WPM``."""

    if float(wpm).is_integer():
        return f"{int(wpm)}WPM"
    return f"{wpm}WPM"


def speed_truth_token(wpm: float) -> str:
    """Token used in the ``.txt`` truth filename (no ``WPM`` suffix)."""

    if float(wpm).is_integer():
        return f"{int(wpm)}"
    return f"{wpm}"


# ---------------------------------------------------------------------------
# Date helpers
# ---------------------------------------------------------------------------


def parse_yymmdd(s: str) -> date:
    return datetime.strptime(s, "%y%m%d").date()


def iso(d: date) -> str:
    return d.strftime("%Y-%m-%d")


# ---------------------------------------------------------------------------
# Filesystem layout
# ---------------------------------------------------------------------------


@dataclass(frozen=True)
class SessionPaths:
    wpm: float
    yymmdd: str
    raw_dir: Path
    mp3: Path
    truth: Path
    trimmed_wav: Path
    chunks_dir: Path

    @property
    def date(self) -> date:
        return parse_yymmdd(self.yymmdd)


def session_paths(wpm: float, yymmdd_s: str) -> SessionPaths:
    base = CORPUS_ROOT / speed_dirname(wpm)
    raw_dir = base / "raw"
    return SessionPaths(
        wpm=wpm,
        yymmdd=yymmdd_s,
        raw_dir=raw_dir,
        mp3=raw_dir / f"{yymmdd_s}.mp3",
        truth=raw_dir / f"{yymmdd_s}.txt",
        trimmed_wav=base / "trimmed" / f"{yymmdd_s}.wav",
        chunks_dir=base / "chunks",
    )


def ensure_corpus_dirs(wpm: float) -> None:
    base = CORPUS_ROOT / speed_dirname(wpm)
    (base / "raw").mkdir(parents=True, exist_ok=True)
    (base / "trimmed").mkdir(parents=True, exist_ok=True)
    (base / "chunks").mkdir(parents=True, exist_ok=True)


# ---------------------------------------------------------------------------
# Truth normalization
# ---------------------------------------------------------------------------

_PROSIGN_RE = re.compile(r"<[^>]*>")
_CTRL_RE = re.compile(r"[\x00-\x1f\x7f-\xff]")


def normalize_truth(raw: str) -> str:
    """Return uppercase ASCII truth with control chars stripped."""

    text = _PROSIGN_RE.sub("", raw)
    text = _CTRL_RE.sub(" ", text)
    text = text.upper()
    paragraphs = [re.sub(r"[ \t]+", " ", p).strip() for p in re.split(r"\n\s*\n", text)]
    paragraphs = [p for p in paragraphs if p]
    return "\n\n".join(paragraphs).strip()


def normalize_decoded(raw: str) -> str:
    text = _PROSIGN_RE.sub("", raw)
    text = _CTRL_RE.sub(" ", text)
    text = text.upper()
    text = re.sub(r"\s+", " ", text)
    return text.strip()


# ---------------------------------------------------------------------------
# Logging
# ---------------------------------------------------------------------------


def configure_logging(log_path: Path = PIPELINE_LOG, level: int = logging.INFO) -> logging.Logger:
    log_path.parent.mkdir(parents=True, exist_ok=True)
    logger = logging.getLogger("arrl_corpus")
    if logger.handlers:
        return logger
    logger.setLevel(level)
    fmt = logging.Formatter("%(asctime)s %(levelname)-7s %(message)s")
    fh = logging.FileHandler(log_path, encoding="utf-8")
    fh.setFormatter(fmt)
    logger.addHandler(fh)
    sh = logging.StreamHandler(sys.stdout)
    sh.setFormatter(logging.Formatter("%(message)s"))
    logger.addHandler(sh)
    return logger


# ---------------------------------------------------------------------------
# JSONL helpers
# ---------------------------------------------------------------------------


def write_jsonl(path: Path, rows: Iterable[dict]) -> int:
    path.parent.mkdir(parents=True, exist_ok=True)
    n = 0
    with path.open("w", encoding="utf-8") as fh:
        for row in rows:
            fh.write(json.dumps(row, ensure_ascii=False) + "\n")
            n += 1
    return n


def read_jsonl(path: Path) -> list[dict]:
    if not path.exists():
        return []
    rows: list[dict] = []
    with path.open("r", encoding="utf-8") as fh:
        for line in fh:
            line = line.strip()
            if not line:
                continue
            rows.append(json.loads(line))
    return rows


def relpath_for_manifest(p: Path) -> str:
    """Return a forward-slash path relative to the repo root."""

    return p.resolve().relative_to(REPO_ROOT).as_posix()


# ---------------------------------------------------------------------------
# Speed parsing for CLI args
# ---------------------------------------------------------------------------


def parse_speeds_arg(arg: str | None) -> list[float]:
    if not arg:
        return list(DEFAULT_SPEEDS)
    out: list[float] = []
    for tok in arg.split(","):
        tok = tok.strip()
        if not tok:
            continue
        out.append(float(tok))
    return out
