#!/usr/bin/env python3
"""CLI runner for Waddle Tenant Isolation & Inference Credentials E2E Suite.

Usage:
    python3 server/tests/e2e_tenant_inference/run_e2e.py --list
    python3 server/tests/e2e_tenant_inference/run_e2e.py --milestone m1 --verbose
    python3 server/tests/e2e_tenant_inference/run_e2e.py --tier 1|2|3|4|all
    python3 server/tests/e2e_tenant_inference/run_e2e.py --tier all --json /tmp/e2e_report.json
"""

from __future__ import annotations

import argparse
import sys
from pathlib import Path

# Ensure sibling modules import cleanly regardless of CWD
THIS_DIR = Path(__file__).resolve().parent
if str(THIS_DIR) not in sys.path:
    sys.path.insert(0, str(THIS_DIR))

from harness import SuiteRunner, TestCase  # noqa: E402
import tier1_m1_formal_and_core  # noqa: E402
import tier1_m2_to_m6_features  # noqa: E402
import tier2_boundaries  # noqa: E402
import tier3_pairwise  # noqa: E402
import tier4_scenarios  # noqa: E402


def collect_all_tests() -> list[TestCase]:
    cases: list[TestCase] = []
    cases.extend(tier1_m1_formal_and_core.get_tests())
    cases.extend(tier1_m2_to_m6_features.get_tests())
    cases.extend(tier2_boundaries.get_tests())
    cases.extend(tier3_pairwise.get_tests())
    cases.extend(tier4_scenarios.get_tests())
    return cases


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        description="Waddle Tenant Isolation & Inference Credentials E2E Test Runner",
    )
    parser.add_argument(
        "--tier",
        choices=("1", "2", "3", "4", "all"),
        default="all",
        help="Filter tests by tier (default: all)",
    )
    parser.add_argument(
        "--milestone",
        choices=("m1", "m2", "m3", "m4", "m5", "m6", "e2e", "all"),
        default="all",
        help="Filter tests by milestone (default: all)",
    )
    parser.add_argument(
        "--list",
        action="store_true",
        help="List selected tests and exit 0",
    )
    parser.add_argument(
        "--json",
        dest="json_path",
        type=Path,
        default=None,
        help="Write machine-readable JSON results report to path",
    )
    parser.add_argument(
        "--fail-fast",
        action="store_true",
        help="Stop executing after the first failing test",
    )
    parser.add_argument(
        "--verbose",
        action="store_true",
        help="Print per-test PASS/FAIL lines as tests run",
    )
    return parser


def main(argv: list[str] | None = None) -> int:
    parser = build_parser()
    args = parser.parse_args(argv)

    runner = SuiteRunner(collect_all_tests())
    selected = runner.filter_cases(tier=args.tier, milestone=args.milestone)

    if args.list:
        print(f"{'ID':<12} {'Tier':<5} {'MS':<4} {'Features':<18} {'Gates':<14} Description")
        print("-" * 100)
        for tc in selected:
            feats = ",".join(tc.feature_ids)
            gates = ",".join(tc.gate_ids)
            print(f"{tc.test_id:<12} T{tc.tier:<4} {tc.milestone:<4} {feats:<18} {gates:<14} {tc.description}")
        print(f"\nListed {len(selected)} test(s).")
        return 0

    if not selected:
        print(
            f"ERROR: 0 tests matched filters --tier {args.tier} --milestone {args.milestone}",
            file=sys.stderr,
        )
        return 1

    results = runner.run(selected, fail_fast=args.fail_fast, verbose=args.verbose)
    runner.print_summary(results, verbose=args.verbose)

    if args.json_path is not None:
        runner.write_json(results, args.json_path)
        print(f"Wrote JSON report to {args.json_path}")

    failed = sum(1 for r in results if not r.passed)
    return 0 if (len(results) > 0 and failed == 0) else 1


if __name__ == "__main__":
    raise SystemExit(main())
