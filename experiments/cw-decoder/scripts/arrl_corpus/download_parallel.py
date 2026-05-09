"""Stage 1: parallel download of MP3 + truth files using asyncio + aiohttp.

Reads ``index.jsonl`` (built by ``build_index.py``) and downloads each session
into ``<corpus>/<wpm>wpm/raw/{YYMMDD}.{mp3,txt}``.

* Per-host concurrency is capped via an asyncio semaphore (default 8).
* Downloads are validated by content-type — image responses (the ARRL CDN's
  GIF error page) are recorded as ``error_page`` in a ``*.dl.json`` sidecar
  and the partial file is removed.
* Idempotent: existing files above a sane minimum size are skipped.
"""

from __future__ import annotations

import argparse
import asyncio
import json
import random
import sys
import time
from dataclasses import dataclass
from pathlib import Path

import aiohttp

from common import (
    DEFAULT_SPEEDS,
    INDEX_PATH,
    USER_AGENT,
    configure_logging,
    ensure_corpus_dirs,
    parse_speeds_arg,
    read_jsonl,
    session_paths,
)

MIN_MP3_BYTES = 100_000
MIN_TXT_BYTES = 200
GET_TIMEOUT_S = 180
JITTER_S = 0.05


@dataclass
class DLResult:
    wpm: float
    date: str
    status: str  # ok | cached | error_page | http_error | size_too_small
    detail: str = ""


def _existing_ok(p: Path, min_bytes: int) -> bool:
    return p.exists() and p.stat().st_size >= min_bytes


async def _download_one(
    session: aiohttp.ClientSession,
    url: str,
    dest: Path,
    *,
    expect: str,  # "audio" or "text"
    min_bytes: int,
) -> tuple[str, str]:
    """Stream a single URL to disk. Returns (status, detail)."""

    if _existing_ok(dest, min_bytes):
        return "cached", str(dest.stat().st_size)

    dest.parent.mkdir(parents=True, exist_ok=True)
    tmp = dest.with_suffix(dest.suffix + ".part")
    if tmp.exists():
        tmp.unlink()

    try:
        async with session.get(url, timeout=aiohttp.ClientTimeout(total=GET_TIMEOUT_S)) as r:
            if r.status != 200:
                return "http_error", f"status={r.status}"
            ctype = (r.headers.get("Content-Type") or "").lower()
            if expect == "audio" and "audio" not in ctype:
                # ARRL CDN serves a GIF error page when the file is missing.
                return "error_page", f"content-type={ctype}"
            if expect == "text" and ("text" not in ctype and "octet-stream" not in ctype):
                return "error_page", f"content-type={ctype}"

            size = 0
            with tmp.open("wb") as fh:
                async for chunk in r.content.iter_chunked(64 * 1024):
                    if chunk:
                        fh.write(chunk)
                        size += len(chunk)
            if size < min_bytes:
                tmp.unlink(missing_ok=True)
                return "size_too_small", str(size)
            tmp.replace(dest)
            return "ok", str(size)
    except (aiohttp.ClientError, asyncio.TimeoutError, OSError) as exc:
        if tmp.exists():
            tmp.unlink(missing_ok=True)
        return "http_error", repr(exc)


async def _download_session(
    session: aiohttp.ClientSession,
    sem: asyncio.Semaphore,
    entry: dict,
) -> DLResult:
    wpm = float(entry["wpm"])
    date = entry["date"]
    paths = session_paths(wpm, date)
    ensure_corpus_dirs(wpm)

    sidecar = paths.raw_dir / f"{date}.dl.json"

    async with sem:
        # tiny jitter to avoid bursty starts
        await asyncio.sleep(random.uniform(0, JITTER_S))
        # Truth first — if we can't get the label, don't bother with the audio.
        txt_status, txt_detail = await _download_one(
            session, entry["txt_url"], paths.truth, expect="text", min_bytes=MIN_TXT_BYTES,
        )
        if txt_status not in ("ok", "cached"):
            sidecar.write_text(json.dumps({
                "wpm": wpm, "date": date,
                "txt": {"status": txt_status, "detail": txt_detail},
                "mp3": {"status": "skipped", "detail": "no truth"},
            }, indent=2), encoding="utf-8")
            return DLResult(wpm, date, f"txt_{txt_status}", txt_detail)

        mp3_status, mp3_detail = await _download_one(
            session, entry["mp3_url"], paths.mp3, expect="audio", min_bytes=MIN_MP3_BYTES,
        )

    sidecar.write_text(json.dumps({
        "wpm": wpm, "date": date,
        "txt": {"status": txt_status, "detail": txt_detail},
        "mp3": {"status": mp3_status, "detail": mp3_detail},
    }, indent=2), encoding="utf-8")

    if mp3_status in ("ok", "cached") and txt_status in ("ok", "cached"):
        return DLResult(wpm, date, "ok" if mp3_status == "ok" else "cached", mp3_detail)
    return DLResult(wpm, date, f"mp3_{mp3_status}", mp3_detail)


async def _run(entries: list[dict], workers: int, logger) -> list[DLResult]:
    sem = asyncio.Semaphore(workers)
    connector = aiohttp.TCPConnector(limit=workers, limit_per_host=workers, ttl_dns_cache=300)
    headers = {"User-Agent": USER_AGENT}
    results: list[DLResult] = []
    completed = 0
    started = time.monotonic()

    async with aiohttp.ClientSession(connector=connector, headers=headers) as session:
        tasks = [asyncio.create_task(_download_session(session, sem, e)) for e in entries]
        for coro in asyncio.as_completed(tasks):
            res = await coro
            results.append(res)
            completed += 1
            if completed % 10 == 0 or completed == len(tasks):
                elapsed = time.monotonic() - started
                rate = completed / max(elapsed, 1e-3)
                logger.info(
                    f"download: {completed}/{len(tasks)} "
                    f"({rate:.1f}/s, {elapsed:.1f}s elapsed) — last: {res.wpm}wpm {res.date} {res.status}"
                )
    return results


def select_entries(index_rows: list[dict], speeds: list[float], limit_per_speed: int) -> list[dict]:
    """Pick newest N sessions per speed from the index."""

    by_speed: dict[float, list[dict]] = {s: [] for s in speeds}
    for r in index_rows:
        wpm = float(r["wpm"])
        if wpm in by_speed:
            by_speed[wpm].append(r)
    out: list[dict] = []
    for s in speeds:
        rows = sorted(by_speed[s], key=lambda r: r["date"], reverse=True)[:limit_per_speed]
        out.extend(rows)
    return out


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--speeds", default=None)
    parser.add_argument("--limit-per-speed", type=int, default=50)
    parser.add_argument("--workers", type=int, default=8,
                        help="Max concurrent connections to arrl.org (default 8).")
    args = parser.parse_args(argv)

    logger = configure_logging()
    speeds = parse_speeds_arg(args.speeds)
    index_rows = read_jsonl(INDEX_PATH)
    if not index_rows:
        logger.error(f"download: no index at {INDEX_PATH}; run build_index.py first")
        return 2

    entries = select_entries(index_rows, speeds, args.limit_per_speed)
    logger.info(
        f"download: {len(entries)} sessions across speeds={speeds} (limit_per_speed={args.limit_per_speed}, "
        f"workers={args.workers})"
    )

    results = asyncio.run(_run(entries, args.workers, logger))
    counts: dict[str, int] = {}
    for r in results:
        counts[r.status] = counts.get(r.status, 0) + 1
    logger.info(f"download summary: {dict(sorted(counts.items()))}")
    ok = counts.get("ok", 0) + counts.get("cached", 0)
    logger.info(f"download: {ok}/{len(results)} ok+cached")
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
