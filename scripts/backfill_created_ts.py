#!/usr/bin/env python3
"""回填 Document 节点的 `created_ts`（Unix 毫秒），供列表按时间倒序。

幂等；`created_at` 缺失/不可解析写 0；默认 dry-run，`--apply` 才写入。

用法：
    python3 scripts/backfill_created_ts.py [--apply] [--url URL]
"""

from __future__ import annotations

import argparse
import json
import re
import sys
import urllib.request
from datetime import datetime, timezone

PAGE = 200


def run(url: str, payload: dict) -> dict:
    req = urllib.request.Request(
        url,
        data=json.dumps(payload).encode(),
        headers={"Content-Type": "application/json"},
    )
    with urllib.request.urlopen(req, timeout=60) as resp:
        body = json.load(resp)
    if "error" in body:
        raise RuntimeError(body["error"])
    return body


def fetch_documents(url: str) -> list[dict]:
    docs: list[dict] = []
    offset = 0
    while True:
        page = run(
            url,
            {
                "request_type": "read",
                "query": {
                    "queries": [
                        {
                            "Query": {
                                "name": "docs",
                                "steps": [
                                    {"NWhere": {"Eq": ["$label", {"String": "Document"}]}},
                                    {"OrderByMultiple": [["id", "Asc"]]},
                                    {"Skip": offset},
                                    {"Limit": PAGE},
                                    {
                                        "Project": [
                                            {"source": "id", "alias": "id"},
                                            {"source": "metadata", "alias": "metadata"},
                                        ]
                                    },
                                ],
                                "condition": None,
                            }
                        }
                    ],
                    "returns": ["docs"],
                },
            },
        )["docs"]["properties"]
        docs.extend(page)
        if len(page) < PAGE:
            return docs
        offset += PAGE


def to_millis(created_at: str | None) -> int:
    """RFC3339（可含 9 位纳秒）→ Unix 毫秒；不可解析 → 0。"""
    if not created_at:
        return 0
    s = created_at.strip()
    # Python 的 fromisoformat 只接受最多 6 位小数：截断纳秒部分。
    s = re.sub(r"(\.\d{6})\d+", r"\1", s)
    if s.endswith("Z"):
        s = s[:-1] + "+00:00"
    try:
        dt = datetime.fromisoformat(s)
    except ValueError:
        return 0
    if dt.tzinfo is None:
        dt = dt.replace(tzinfo=timezone.utc)
    return int(dt.timestamp() * 1000)


def set_prop(url: str, doc_id: str, millis: int) -> None:
    run(
        url,
        {
            "request_type": "write",
            "query": {
                "queries": [
                    {
                        "Query": {
                            "name": "upd",
                            "steps": [
                                {
                                    "NWhere": {
                                        "And": [
                                            {"Eq": ["$label", {"String": "Document"}]},
                                            {"Eq": ["id", {"String": doc_id}]},
                                        ]
                                    }
                                },
                                {"SetProperty": ["created_ts", {"Value": {"I64": millis}}]},
                            ],
                            "condition": None,
                        }
                    }
                ],
                "returns": [],
            },
        }
    )


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--url", default="http://localhost:6970")
    ap.add_argument("--apply", action="store_true", help="实际写入（默认 dry-run）")
    args = ap.parse_args()

    url = args.url.rstrip("/") + "/v1/query"
    docs = fetch_documents(url)
    print(f"documents: {len(docs)}")

    plan: list[tuple[str, int]] = []
    zero = 0
    for d in docs:
        doc_id = d.get("id")
        if not doc_id:
            continue
        md_raw = d.get("metadata")
        created_at = None
        if md_raw:
            try:
                created_at = json.loads(md_raw).get("created_at")
            except (TypeError, ValueError):
                created_at = None
        millis = to_millis(created_at)
        if millis == 0:
            zero += 1
        plan.append((doc_id, millis))

    print(f"to backfill: {len(plan)}  (missing/unparsable created_at -> 0: {zero})")
    if plan:
        newest = max(plan, key=lambda p: p[1])
        oldest = min(plan, key=lambda p: p[1])
        print(f"range: {oldest[1]} .. {newest[1]}")

    if not args.apply:
        print("dry-run only; pass --apply to write")
        return 0

    done = 0
    for doc_id, millis in plan:
        set_prop(url, doc_id, millis)
        done += 1
        if done % 50 == 0:
            print(f"  written {done}/{len(plan)}")
    print(f"done: {done} nodes updated")
    return 0


if __name__ == "__main__":
    sys.exit(main())
