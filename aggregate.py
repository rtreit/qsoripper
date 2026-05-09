"""Aggregate per-config experiment_report.*.json files into one."""
from __future__ import annotations
import json
from pathlib import Path

ROOT = Path(__file__).parent

configs = [
    ("viterbi_only", ROOT / "experiment_report.viterbi_only.json"),
    ("bigram_l0.0",  ROOT / "experiment_report.bigram_l0.0.json"),
    ("bigram_l0.25", ROOT / "experiment_report.bigram_l0.25.json"),
    ("bigram_l0.5",  ROOT / "experiment_report.bigram_l0.5.json"),
    ("bigram_l1.0",  ROOT / "experiment_report.bigram_l1.0.json"),
    ("bigram_l2.0",  ROOT / "experiment_report.bigram_l2.0.json"),
]

agg = {"description": "Bigram-LM Viterbi rescoring on ARRL corpus (Round 4)", "configs": {}}
for label, path in configs:
    if not path.exists():
        continue
    data = json.loads(path.read_text())
    agg["configs"][label] = {
        "mean_cer": data.get("mean_cer"),
        "mean_wer": data.get("mean_wer"),
        "samples": {k: {"cer": v["cer"], "wer": v["wer"], "hyp": v["hyp"]}
                    for k, v in data.get("samples", {}).items()},
    }

# Pick best by mean CER
best = min(agg["configs"].items(), key=lambda kv: kv[1]["mean_cer"] or 99)
agg["best_config"] = best[0]
agg["best_mean_cer"] = best[1]["mean_cer"]

(ROOT / "experiment_report.json").write_text(json.dumps(agg, indent=2))
print("best:", best[0], "mean_cer:", best[1]["mean_cer"])
