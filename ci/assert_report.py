#!/usr/bin/env python3
"""Assert Maul report schema 0.2 and session-aware recovery metrics."""

from __future__ import annotations

import argparse
import json
import sys
from typing import Any


def load_report(path: str) -> dict[str, Any]:
    with open(path, encoding="utf-8") as handle:
        report: dict[str, Any] = json.load(handle)
    return report


def assert_schema(report: dict[str, Any]) -> None:
    if report.get("schema_version") != "0.2":
        raise SystemExit(f"expected schema 0.2, got {report.get('schema_version')!r}")
    if not report.get("run_id"):
        raise SystemExit("report is missing run_id")
    summary = report.get("summary") or {}
    recovery = summary.get("recovery_events")
    alias = summary.get("post_fault_successes")
    if recovery != alias:
        raise SystemExit(
            f"recovery_events ({recovery}) must equal post_fault_successes ({alias})"
        )
    requests = report.get("requests") or []
    if requests and any("session_id" not in item or "sequence" not in item for item in requests):
        raise SystemExit("request records must include session_id and sequence")


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("report")
    parser.add_argument(
        "--expect",
        choices=("recovered", "unrecovered", "schema"),
        default="schema",
    )
    args = parser.parse_args()
    report = load_report(args.report)
    assert_schema(report)
    summary = report.get("summary") or {}
    if args.expect == "recovered":
        if int(summary.get("recovery_events") or 0) < 1:
            raise SystemExit("expected at least one recovery event")
        if int(summary.get("unrecovered_sessions") or 0) != 0:
            raise SystemExit("expected zero unrecovered sessions")
        if int(summary.get("recovered_sessions") or 0) < 1:
            raise SystemExit("expected a recovered session")
    elif args.expect == "unrecovered":
        if int(summary.get("unrecovered_sessions") or 0) < 1:
            raise SystemExit("expected an unrecovered session")
    print("report contract ok")
    return 0


if __name__ == "__main__":
    sys.exit(main())
