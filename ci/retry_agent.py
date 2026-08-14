#!/usr/bin/env python3
"""Retrying CI agent that attributes both attempts to one Maul session.

The process exits 0 after attempting a retry so `maul test --fail-on resilience`
is the merge gate, not the agent exit code.
"""

from __future__ import annotations

import json
import os
import sys
import urllib.error
import urllib.request

SESSION_HEADER = "X-Maul-Session-Id"


def completion_url() -> str:
    base = os.environ.get("MAUL_BASE_URL") or os.environ.get("OPENAI_BASE_URL")
    if not base:
        raise SystemExit("MAUL_BASE_URL or OPENAI_BASE_URL is required")
    return f"{base.rstrip('/')}/chat/completions"


def session_id() -> str:
    session = os.environ.get("MAUL_SESSION_ID", "").strip()
    if not session:
        raise SystemExit("MAUL_SESSION_ID is required for session-aware scoring")
    return session


def call(url: str, session: str) -> int:
    payload = json.dumps(
        {
            "model": "gpt-4o-mini",
            "messages": [{"role": "user", "content": "ping"}],
            "user": session,
        }
    ).encode("utf-8")
    request = urllib.request.Request(
        url,
        data=payload,
        method="POST",
        headers={
            "Content-Type": "application/json",
            "Authorization": "Bearer ci-not-a-secret",
            SESSION_HEADER: session,
        },
    )
    try:
        with urllib.request.urlopen(request, timeout=10) as response:
            return int(response.status)
    except urllib.error.HTTPError as error:
        return int(error.code)


def main() -> int:
    url = completion_url()
    session = session_id()
    status = call(url, session)
    if status >= 400:
        call(url, session)
    return 0


if __name__ == "__main__":
    sys.exit(main())
