"""Stage 0: scrape ARRL archive index pages and emit ``index.jsonl``.

Each archive index page (e.g. https://www.arrl.org/20-wpm-code-archive) is a
single HTML document that lists every available MP3 + truth file as plain
``<a href="...">`` tags. We fetch one page per requested speed, extract the
(MP3, TXT) pairs, and write a single JSONL index that all downstream stages
consume.

This replaces all blind date-probing in the prior pipeline.
"""

from __future__ import annotations

import argparse
import re
import sys
import time
from collections import defaultdict
from urllib.parse import unquote

import urllib.request
import urllib.error

from common import (
    ARRL_BASE_URL,
    DEFAULT_SPEEDS,
    INDEX_PATH,
    USER_AGENT,
    configure_logging,
    parse_speeds_arg,
    parse_yymmdd,
    speed_filename_token,
    speed_truth_token,
    speed_url_slug,
    write_jsonl,
)

# Anchor href like /files/file/Morse/Archive/20%20WPM/250121_20WPM.mp3
HREF_RE = re.compile(
    r'href="((?:https?://(?:www\.)?arrl\.org)?/files/file/Morse/Archive/[^"]+\.(?:mp3|txt))"',
    re.IGNORECASE,
)

# Filenames look like 250121_20WPM.mp3 (audio) and 250121_20.txt (truth).
MP3_NAME_RE = re.compile(r"^(?P<date>\d{6})_(?P<tok>[\d.]+)WPM\.mp3$", re.IGNORECASE)
TXT_NAME_RE = re.compile(r"^(?P<date>\d{6})_(?P<tok>[\d.]+)\.txt$", re.IGNORECASE)


def fetch_index_html(wpm: float, *, timeout: float = 30.0, retries: int = 3) -> str | None:
    url = f"{ARRL_BASE_URL}/{speed_url_slug(wpm)}"
    last_err: Exception | None = None
    for attempt in range(retries):
        if attempt:
            time.sleep(1.5 * attempt)
        try:
            req = urllib.request.Request(url, headers={"User-Agent": USER_AGENT})
            with urllib.request.urlopen(req, timeout=timeout) as r:
                if r.status != 200:
                    last_err = RuntimeError(f"HTTP {r.status}")
                    continue
                return r.read().decode("utf-8", "replace")
        except (urllib.error.URLError, TimeoutError, OSError) as exc:  # type: ignore[misc]
            last_err = exc
            continue
    print(f"  WARN: failed to fetch {url}: {last_err}", file=sys.stderr)
    return None


def parse_links(html: str) -> list[tuple[str, str]]:
    """Return list of (filename, absolute_url) for Morse archive anchors."""

    out: list[tuple[str, str]] = []
    for href in HREF_RE.findall(html):
        # href may be server-relative (/files/...) or absolute (http://...).
        if href.lower().startswith("http"):
            abs_url = href
            path = href.split("/", 3)[-1]  # drop scheme://host/
            decoded = unquote("/" + path)
        else:
            abs_url = ARRL_BASE_URL + href
            decoded = unquote(href)
        name = decoded.rsplit("/", 1)[-1]
        out.append((name, abs_url))
    return out


def collect_sessions(wpm: float, html: str) -> list[dict]:
    """Pair MP3 + TXT links from one index page into session records."""

    # Both regexes capture the bare numeric token (without "WPM"); compare to
    # the truth token form (e.g. "20" or "7.5").
    mp3_token = speed_truth_token(wpm).lower()
    txt_token = speed_truth_token(wpm).lower()

    mp3_by_date: dict[str, str] = {}
    txt_by_date: dict[str, str] = {}

    for name, url in parse_links(html):
        lower = name.lower()
        m = MP3_NAME_RE.match(lower)
        if m and m.group("tok") == mp3_token:
            mp3_by_date[m.group("date")] = url
            continue
        t = TXT_NAME_RE.match(lower)
        if t and t.group("tok") == txt_token:
            txt_by_date[t.group("date")] = url

    sessions: list[dict] = []
    for d, mp3_url in mp3_by_date.items():
        txt_url = txt_by_date.get(d)
        if not txt_url:
            continue
        try:
            parse_yymmdd(d)
        except ValueError:
            continue
        sessions.append({
            "wpm": wpm,
            "date": d,
            "mp3_url": mp3_url,
            "txt_url": txt_url,
            "mp3_filename": mp3_url.rsplit("/", 1)[-1],
            "txt_filename": txt_url.rsplit("/", 1)[-1],
        })
    sessions.sort(key=lambda r: r["date"], reverse=True)
    return sessions


def build_index(speeds: list[float], logger) -> list[dict]:
    all_rows: list[dict] = []
    counts: dict[float, int] = defaultdict(int)
    for wpm in speeds:
        html = fetch_index_html(wpm)
        if not html:
            logger.warning(f"index: skipped {wpm} WPM (no html)")
            continue
        rows = collect_sessions(wpm, html)
        counts[wpm] = len(rows)
        all_rows.extend(rows)
        logger.info(f"index: {wpm:>5} WPM -> {len(rows)} sessions")
    n = write_jsonl(INDEX_PATH, all_rows)
    logger.info(f"index: wrote {n} sessions -> {INDEX_PATH}")
    return all_rows


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--speeds", default=None,
                        help=f"Comma-separated speeds. Default {','.join(str(s) for s in DEFAULT_SPEEDS)}.")
    args = parser.parse_args(argv)

    logger = configure_logging()
    speeds = parse_speeds_arg(args.speeds)
    rows = build_index(speeds, logger)
    return 0 if rows else 1


if __name__ == "__main__":
    sys.exit(main())
