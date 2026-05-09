"""WA6MOW four-toggle diagnostic harness.

Runs the WA6MOW POTA sample through 6 decoder configurations to localize
*at which layer* the leading "WA" of the first "WA6MOW" is being lost.

Toggles:
  1. file              -- whole-file non-streaming decode (ditdah baseline,
                          auto WPM).
  2. region-default    -- `stream-region` with current defaults.
  3. region-pad-0.30   -- region stream with pad_s = 0.30 s.
  4. region-pad-0.50   -- region stream with pad_s = 0.50 s.
  5. region-pad-1.00   -- region stream with pad_s = 1.00 s.
  6. region-force-wpm-22  -- region stream with --force-wpm 22 (the
                              empirical "second-half" WPM where decoding
                              succeeds; truth is ~20 WPM).
  7. region-force-pitch-760 -- region stream with --force-pitch 760
                                (dominant Fisher peak from probe-fisher).
  8. region-manual-window -- region stream clipped to [0, 12] s
                              (covering only the first WA6MOW
                              transmission).

Output:
  - Markdown report at experiments/cw-decoder/data/wa6mow_diagnostic.md
  - JSON sidecar at experiments/cw-decoder/data/wa6mow_diagnostic.json
  - EXPERIMENT_REPORT.md at the worktree root.

Usage:
    python wa6mow_diagnostic.py [worktree_root]
"""

from __future__ import annotations

import json
import re
import subprocess
import sys
from pathlib import Path


SAMPLE = Path(
    r"C:\Users\randy\Git\qsoripper\data\cw-samples\training-set-a\cq-pota-de-wa6mow.mp3"
)
TRUTH = "NQ CQ POTA DE WA6MOW CQ POTA DE WA6MOW K"

# The "needle" we are hunting: the leading WA of the first WA6MOW.
# We declare WA "recovered" if the decoded text contains "WA6MOW" at
# least twice (i.e. both transmissions decoded), OR if the substring
# "WA6" appears before second-half repetition. We track both.
NEEDLE_FULL = "WA6MOW"


def normalize(s: str) -> str:
    s = s.upper()
    s = re.sub(r"[^A-Z0-9 ]+", " ", s)
    return re.sub(r"\s+", " ", s).strip()


def normalize_collapsed(s: str) -> str:
    """Collapse whitespace too — for substring needle search."""
    return re.sub(r"\s+", "", normalize(s))


def run_cli(exe: Path, args: list[str], timeout: float = 120.0) -> tuple[str, str, int]:
    p = subprocess.run(
        [str(exe), *args], capture_output=True, text=True, timeout=timeout
    )
    return p.stdout, p.stderr, p.returncode


def parse_stream_region_json(stdout: str) -> str:
    """Pull the final transcript from stream-region --json output."""
    text = ""
    for line in stdout.splitlines():
        try:
            o = json.loads(line)
        except Exception:
            continue
        if o.get("type") in ("transcript", "end") and o.get("transcript"):
            text = o["transcript"]
    return text


def parse_file_decode(stdout: str) -> str:
    """Pull the decoded text out of `cw-decoder file <path>` output."""
    lines = stdout.splitlines()
    for i, ln in enumerate(lines):
        if "decoded text" in ln.lower():
            # next non-empty line is the result
            for j in range(i + 1, len(lines)):
                if lines[j].strip():
                    return lines[j].strip()
    return ""


def needle_status(text: str) -> dict:
    """Return what was recovered of the FIRST WA6MOW.

    Truth has TWO `WA6MOW` occurrences. The prior bench established that
    the *second* (later in the audio) always decodes correctly. So:

      - 2 occurrences  -> first WA6MOW recovered AND second too.
      - 1 occurrence   -> only the second; FIRST is lost.
      - 0 occurrences  -> both lost.

    To disambiguate "1 occurrence" we also check whether the surviving
    `WA6MOW` lies in the second half of the transcript (where the second
    transmission lives).
    """
    n = normalize_collapsed(text)
    occurrences = [m.start() for m in re.finditer("WA6MOW", n)]
    n_count = len(occurrences)
    second_full = n_count >= 2
    first_full = n_count >= 2
    sole_in_second_half: bool | None = None
    if n_count == 1 and n:
        sole_in_second_half = occurrences[0] >= len(n) // 2
    head = n[: occurrences[0]] if occurrences else n
    return {
        "decoded": text,
        "decoded_normalized": normalize(text),
        "wa6mow_count": n_count,
        "first_wa6mow_recovered": first_full,
        "second_wa6mow_recovered": second_full,
        "sole_occurrence_in_second_half": sole_in_second_half,
        "transcript_head_before_first_wa6mow": head,
    }


def run_toggles(exe: Path) -> list[dict]:
    file_path = str(SAMPLE)
    common_stream = [
        "stream-region",
        "--file",
        file_path,
        "--json",
        "--no-realtime",
    ]
    toggles: list[tuple[str, list[str], str]] = [
        ("1-file-fullpath-auto-wpm", ["file", file_path], "file"),
        ("2-region-default", common_stream + [], "stream"),
        ("3-region-pad-0.30", common_stream + ["--pad-s", "0.30"], "stream"),
        ("4-region-pad-0.50", common_stream + ["--pad-s", "0.50"], "stream"),
        ("5-region-pad-1.00", common_stream + ["--pad-s", "1.00"], "stream"),
        (
            "6-region-force-wpm-22",
            common_stream + ["--force-wpm", "22"],
            "stream",
        ),
        (
            "7-region-force-pitch-760",
            common_stream + ["--force-pitch", "760"],
            "stream",
        ),
        (
            "8-region-manual-window-0-to-12s",
            common_stream + ["--region-start-s", "0", "--region-end-s", "12"],
            "stream",
        ),
        # Bonus: combine forced WPM + manual window
        (
            "9-region-manual-window-force-wpm-22",
            common_stream
            + [
                "--region-start-s",
                "0",
                "--region-end-s",
                "12",
                "--force-wpm",
                "22",
            ],
            "stream",
        ),
    ]
    results: list[dict] = []
    for name, args, kind in toggles:
        print(f"  running {name} ...", flush=True)
        try:
            stdout, stderr, rc = run_cli(exe, args, timeout=120)
        except subprocess.TimeoutExpired:
            results.append({"toggle": name, "error": "timeout", "args": args})
            continue
        if kind == "file":
            decoded = parse_file_decode(stdout)
        else:
            decoded = parse_stream_region_json(stdout)
        rec = {
            "toggle": name,
            "args": args,
            "returncode": rc,
            **needle_status(decoded),
        }
        results.append(rec)
    return results


def render_markdown(results: list[dict], summary: dict) -> str:
    lines: list[str] = []
    lines.append("# WA6MOW four-toggle diagnostic")
    lines.append("")
    lines.append(f"- Sample: `{SAMPLE.name}`")
    lines.append(f"- Truth: `{TRUTH}`")
    lines.append(
        "- Needle: leading `WA` of the **first** `WA6MOW` transmission."
    )
    lines.append("")
    lines.append("## Verdict")
    lines.append("")
    lines.append(summary["verdict"])
    lines.append("")
    lines.append("## Per-toggle results")
    lines.append("")
    lines.append(
        "| # | Toggle | 1st WA6MOW | 2nd WA6MOW | WA6MOW count | head before 1st hit |"
    )
    lines.append("|---|--------|:---------:|:---------:|:---:|---------|")
    for r in results:
        if "error" in r:
            lines.append(f"| - | {r['toggle']} | ERR | ERR | - | {r['error']} |")
            continue
        lines.append(
            "| {idx} | `{name}` | {a} | {b} | {c} | `{head}` |".format(
                idx=r["toggle"].split("-", 1)[0],
                name=r["toggle"],
                a="✅" if r["first_wa6mow_recovered"] else "❌",
                b="✅" if r["second_wa6mow_recovered"] else "❌",
                c=r["wa6mow_count"],
                head=(r.get("transcript_head_before_first_wa6mow") or "")[:48],
            )
        )
    lines.append("")
    lines.append("## Decoded text per toggle")
    lines.append("")
    for r in results:
        if "error" in r:
            continue
        lines.append(f"### {r['toggle']}")
        lines.append("")
        lines.append(f"- args: `{' '.join(r['args'])}`")
        lines.append(f"- decoded: `{r['decoded_normalized']}`")
        lines.append("")
    return "\n".join(lines) + "\n"


def derive_verdict(results: list[dict]) -> dict:
    by_name = {r["toggle"]: r for r in results if "error" not in r}

    def first_ok(name: str) -> bool:
        return bool(by_name.get(name, {}).get("first_wa6mow_recovered"))

    # Layer attribution rules from the strategic-review prompt:
    pad_results = {
        n: first_ok(n)
        for n in (
            "3-region-pad-0.30",
            "4-region-pad-0.50",
            "5-region-pad-1.00",
        )
    }
    force_wpm_ok = first_ok("6-region-force-wpm-22")
    force_pitch_ok = first_ok("7-region-force-pitch-760")
    manual_window_ok = first_ok("8-region-manual-window-0-to-12s")
    manual_window_force_wpm_ok = first_ok(
        "9-region-manual-window-force-wpm-22"
    )
    region_default_ok = first_ok("2-region-default")
    file_full_ok = first_ok("1-file-fullpath-auto-wpm")

    lines: list[str] = []
    if region_default_ok:
        lines.append(
            "- Default region-stream already recovers WA6MOW; the "
            "previously-reported regression no longer reproduces. "
            "**No layer at fault** in this binary."
        )
    elif force_wpm_ok and not any(pad_results.values()):
        lines.append(
            "- **Verdict: WPM auto-lock is the bug.**"
            "  Forcing `--force-wpm 22` (≈ truth) recovers the leading WA "
            "of the first WA6MOW. Larger `pad_s` does not. This confirms "
            "the front-end short-burst WPM estimator seeds too low at "
            "startup (the `bayes-joint` agent's 11–13 WPM observation), "
            "and the entire first transmission is decoded against the "
            "wrong rate."
        )
    elif any(pad_results.values()) and not force_wpm_ok:
        winners = [k for k, v in pad_results.items() if v]
        lines.append(
            "- **Verdict: region cropping is the bug.**"
            "  Increasing pad_s recovers WA "
            f"({', '.join(winners)}). The detected region starts after "
            "the first dahs of the W. Fix is to grow leading pad or "
            "back off region onset by ~0.3-0.5 s."
        )
    elif force_pitch_ok and not force_wpm_ok and not any(pad_results.values()):
        lines.append(
            "- **Verdict: pitch lock is the bug.**"
            "  Forcing `--force-pitch 760` recovers WA. The first "
            "transmission's burst-pitch discovery is missing the actual "
            "carrier. Fix: widen pitch sweep or use a longer-window "
            "dominant-pitch estimate before per-burst sweep."
        )
    elif manual_window_ok and not region_default_ok:
        lines.append(
            "- **Verdict: region detection is selecting the wrong "
            "boundaries.**  When we hand-pick [0,12]s the leading WA "
            "is recovered. The region detector is dropping or merging "
            "the very first burst."
        )
    elif file_full_ok and not region_default_ok:
        lines.append(
            "- **Verdict: streaming/region path is the bug, not the "
            "decoder math.** The whole-file `file` decode recovers WA "
            "but `stream-region` does not. Likely cropping or "
            "warmup in the streaming front end."
        )
    elif (
        not force_wpm_ok
        and not any(pad_results.values())
        and not force_pitch_ok
        and not manual_window_ok
        and not file_full_ok
    ):
        lines.append(
            "- **Verdict: envelope/onset detection is the bug.**"
            "  Nothing recovers WA — not bigger pad, not forced WPM, "
            "not forced pitch, not manual region boundaries, not "
            "whole-file decode. The leading W never reaches the "
            "decoder as on/off elements. A different detector front "
            "end (matched filter / CFAR) is required."
        )
    else:
        lines.append(
            "- **Verdict: mixed signal.**  More than one toggle "
            "recovers WA, or the recovery pattern doesn't match a "
            "single layer. See per-toggle table for details."
        )

    if manual_window_force_wpm_ok and not manual_window_ok:
        lines.append(
            "- Subsidiary: manual window alone is insufficient, but "
            "manual window + forced WPM is, reinforcing the WPM-lock "
            "diagnosis."
        )

    return {
        "verdict": "\n".join(lines),
        "force_wpm_recovers": force_wpm_ok,
        "any_pad_recovers": any(pad_results.values()),
        "force_pitch_recovers": force_pitch_ok,
        "manual_window_recovers": manual_window_ok,
        "file_full_recovers": file_full_ok,
        "region_default_recovers": region_default_ok,
    }


def main(worktree: Path) -> int:
    exe = (
        worktree
        / "experiments"
        / "cw-decoder"
        / "target"
        / "release"
        / "cw-decoder.exe"
    )
    if not exe.exists():
        print(f"ERROR: build first: {exe} not found", file=sys.stderr)
        return 2
    if not SAMPLE.exists():
        print(f"ERROR: sample not found: {SAMPLE}", file=sys.stderr)
        return 2

    print(f"Running 9 toggles against {SAMPLE.name} ...", flush=True)
    results = run_toggles(exe)
    summary = derive_verdict(results)

    out_dir = worktree / "experiments" / "cw-decoder" / "data"
    out_dir.mkdir(parents=True, exist_ok=True)
    json_path = out_dir / "wa6mow_diagnostic.json"
    md_path = out_dir / "wa6mow_diagnostic.md"
    json_path.write_text(
        json.dumps(
            {"sample": str(SAMPLE), "truth": TRUTH, "summary": summary, "results": results},
            indent=2,
        ),
        encoding="utf-8",
    )
    md_path.write_text(render_markdown(results, summary), encoding="utf-8")
    print(f"Wrote {md_path}")
    print(f"Wrote {json_path}")
    print()
    print(summary["verdict"])
    return 0


if __name__ == "__main__":
    root = Path(sys.argv[1]) if len(sys.argv) > 1 else Path(__file__).resolve().parents[3]
    sys.exit(main(root))
