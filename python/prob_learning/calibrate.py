#!/usr/bin/env python3
"""Probability calibration (decision Q1-P3, bayesian backend).

Updates a result table's outcome distribution with a Dirichlet-Multinomial model:
    posterior_alpha = prior_alpha + observed_counts
Sources: ① replay/historical outcomes (--data, JSONL) and ② expert priors (--prior, JSON).

Writes the posterior concentration params into a rules config so that
rules.prob.providers[<table>] = {"kind": "bayesian", "params": {...}}.

    python prob_learning/calibrate.py direct_vs_vehicle \
        --data runs/outcomes.jsonl --prior priors/expert.json \
        --rules ../config/rules.default.json --out ../config/rules.calibrated.json
"""
from __future__ import annotations

import argparse
import json
from collections import defaultdict
from pathlib import Path


def read_counts(data_path: Path, table: str):
    """JSONL records: {"table": str, "attackLevel": int, "outcome": str}."""
    counts = defaultdict(lambda: defaultdict(float))  # level -> outcome -> n
    if not data_path:
        return counts
    for line in Path(data_path).read_text(encoding="utf-8").splitlines():
        line = line.strip()
        if not line:
            continue
        rec = json.loads(line)
        if rec.get("table") != table:
            continue
        lvl = str(rec["attackLevel"])
        counts[lvl][str(rec["outcome"])] += 1.0
    return counts


def static_priors(rules: dict, table: str, strength: float):
    """Derive pseudo-count priors from the existing static table (so calibration starts
    from the ruleset and gently moves toward the data)."""
    priors = defaultdict(lambda: defaultdict(float))
    cells = rules.get("combatResultTables", {}).get(table, {}).get("cells", {})
    for level, by_roll in cells.items():
        hist = defaultdict(float)
        for _roll, outcome in by_roll.items():
            hist[str(outcome)] += 1.0
        total = sum(hist.values()) or 1.0
        for outcome, n in hist.items():
            priors[level][outcome] = strength * n / total
    return priors


def merge(prior, counts):
    posterior = defaultdict(lambda: defaultdict(float))
    levels = set(prior) | set(counts)
    for lvl in levels:
        outcomes = set(prior.get(lvl, {})) | set(counts.get(lvl, {}))
        for o in outcomes:
            posterior[lvl][o] = prior.get(lvl, {}).get(o, 0.0) + counts.get(lvl, {}).get(o, 0.0)
    return posterior


def posterior_means(posterior):
    means = {}
    for lvl, alphas in posterior.items():
        total = sum(alphas.values()) or 1.0
        means[lvl] = {o: round(a / total, 4) for o, a in alphas.items()}
    return means


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("table")
    ap.add_argument("--data", default=None, help="JSONL of observed outcomes")
    ap.add_argument("--prior", default=None, help="JSON expert prior: {level:{outcome:alpha}}")
    ap.add_argument("--prior-strength", type=float, default=4.0,
                    help="pseudo-count weight when deriving prior from the static table")
    ap.add_argument("--rules", required=True)
    ap.add_argument("--out", required=True)
    args = ap.parse_args()

    rules = json.loads(Path(args.rules).read_text(encoding="utf-8"))

    if args.prior:
        raw = json.loads(Path(args.prior).read_text(encoding="utf-8"))
        prior = defaultdict(lambda: defaultdict(float))
        for lvl, od in raw.items():
            for o, a in od.items():
                prior[str(lvl)][str(o)] = float(a)
    else:
        prior = static_priors(rules, args.table, args.prior_strength)

    counts = read_counts(Path(args.data) if args.data else None, args.table)
    posterior = merge(prior, counts)

    dice = rules.get("combatResultTables", {}).get(args.table, {}).get("randomDice", 2)
    params = {
        "dice": dice,
        "byLevel": {lvl: dict(alphas) for lvl, alphas in posterior.items()},
    }
    rules.setdefault("prob", {}).setdefault("providers", {})[args.table] = {
        "kind": "bayesian", "params": params,
    }

    Path(args.out).write_text(json.dumps(rules, indent=2, ensure_ascii=False), encoding="utf-8")
    print(f"calibrated '{args.table}' -> {args.out}")
    print("posterior outcome means by attack level:")
    print(json.dumps(posterior_means(posterior), indent=2))


if __name__ == "__main__":
    main()
