#!/usr/bin/env python3
"""浏览器验收台的演示数据：只写 daemon 的 SQLite，不发任何 HTTP 请求。

数据是为了让断言「有东西可量」而造的最小集合：多行空间列表（/spaces 才有行高可测、
main 才有滚动余量）、覆盖各 task_state / wake_state 的台账行（台账行高与唤醒标记）、
一条带 markdown 正文的会话（正文行宽与垂直贴合）。

时间列一律写 UTC 无时区串，与 daemon 的 `datetime('now')` 同格式。
"""

from __future__ import annotations

import argparse
import datetime as dt
import os
import sqlite3
import sys
from pathlib import Path

TEAMS = {"legal": "法务", "finance": "财务", "ops": "运维"}

# (ws_id 后缀, label, team, 是否已关闭)
DEMO_SPACES = [
    ("legal", "客户合同审阅", "legal", False),
    ("finance", "报销自动化", "finance", False),
    ("ops", "日志归档", "ops", False),
    ("supplier", "供应商尽调", "legal", False),
    ("reconcile", "月度对账", "finance", False),
    ("rollout", "发布巡检", "ops", False),
    ("clause", "合规条款库", "legal", False),
    ("invoice", "发票抽取", "finance", False),
    ("alerts", "告警归并", "ops", False),
    ("arbitration", "仲裁材料", "legal", False),
    ("budget", "预算复核", "finance", False),
    ("backup", "备份校验", "ops", False),
    ("trademark", "商标续展", "legal", False),
    ("tax", "税务留档", "finance", False),
    ("capacity", "容量盘点", "ops", False),
    ("crossborder", "数据出境评估", "legal", False),
    ("costsplit", "成本分摊", "finance", False),
    ("handover", "值班交接", "ops", False),
    ("authorization", "授权函归档", "legal", False),
    ("collection", "收款核对", "finance", False),
    ("postmortem", "变更复盘", "ops", False),
    ("legacycontract", "旧合同清理", "legal", False),
    ("payout", "供应商付款", "finance", False),
    ("inspection", "巡检报告", "ops", True),
]

CONVERSATION_ID = "conv-demo-contract"
CONVERSATION_WS = "ws_demo_legal"


def utc(**delta: int) -> str:
    moment = dt.datetime.now(dt.timezone.utc).replace(tzinfo=None) - dt.timedelta(**delta)
    return moment.strftime("%Y-%m-%d %H:%M:%S")


def dispatch(
    task_id: str,
    *,
    target: str,
    team: str,
    state: str,
    # 在飞行也写 `delivered`：`pending` 会被唤醒泵认领并真的叫起 Peco 的一轮
    # （演示数据不该引来一次模型调用），而 skipped/failed 才会在台账里长出唤醒刻度。
    wake: str = "delivered",
    reason: str | None = None,
    digest: str | None = None,
    created: dict,
    terminal: dict | None = None,
) -> dict:
    return {
        "id": f"dis-{task_id}",
        "task_id": task_id,
        "target_ws_id": target,
        "target_work_dir": f"/tmp/unused/{target}",
        "target_team_id": team,
        "origin_ws_id": "ws_peco",
        "agent_name": "@master",
        "origin_kind": "dispatch_to_workspace",
        "task_state": state,
        "wake_state": wake,
        "wake_skip_reason": reason,
        "wake_attempts": 1 if wake in ("delivered", "skipped") else 0,
        "wake_claimed_at": None,
        "delivered_at": utc(minutes=5) if wake == "delivered" else None,
        "result_digest": digest,
        "created_at": utc(**created),
        "terminal_at": utc(**terminal) if terminal else None,
    }


def dispatches() -> list[dict]:
    return [
        dispatch(
            "e2e-completed-legal", target="ws_demo_legal", team="legal",
            state="completed", digest="合同清单已抽出 42 条关键条款",
            created={"hours": 3}, terminal={"hours": 2},
        ),
        dispatch(
            "e2e-completed-finance", target="ws_demo_finance", team="finance",
            state="completed", digest="报销单据校验完成 18 张",
            created={"hours": 5}, terminal={"hours": 4},
        ),
        dispatch(
            "e2e-working-ops", target="ws_demo_ops", team="ops",
            state="working", created={"minutes": 12},
        ),
        dispatch(
            "e2e-submitted-legal", target="ws_demo_legal", team="legal",
            state="submitted", created={"minutes": 2},
        ),
        dispatch(
            "e2e-waiting-finance", target="ws_demo_finance", team="finance",
            state="input_required", created={"minutes": 30},
        ),
        dispatch(
            "e2e-skip-origin", target="ws_demo_legal", team="legal",
            state="completed", wake="skipped", reason="not_peco_origin",
            digest="上游系统补录的派发记录", created={"hours": 6}, terminal={"hours": 6},
        ),
        dispatch(
            "e2e-skip-orphaned", target="ws_demo_ops", team="ops",
            state="failed", wake="skipped", reason="orphaned",
            digest="目标空间失联，无人报进度", created={"hours": 8}, terminal={"hours": 7},
        ),
        dispatch(
            "e2e-orphan-finance", target="ws_demo_finance", team="finance",
            state="orphan", wake="skipped", reason="orphaned",
            digest="观察者失联，终态未知", created={"minutes": 20},
        ),
        dispatch(
            "e2e-failed-legal", target="ws_demo_legal", team="legal",
            state="failed", digest="解析器超时", created={"hours": 9}, terminal={"hours": 9},
        ),
        dispatch(
            "e2e-yesterday-inspection", target="ws_demo_inspection", team="ops",
            state="completed", digest="巡检报告：3 项待处理",
            created={"days": 1, "hours": 2}, terminal={"days": 1, "hours": 1},
        ),
    ]


CONVERSATION_MESSAGES = [
    ("user", "把 ~/work/legal/contracts 里的合同过一遍，列出需要法务确认的条款。"),
    (
        "assistant",
        """## 结论

共 12 份合同，其中 4 份存在需要人工确认的条款。下面是最需要先处理的两份：付款条件与终止条款互相矛盾，单看任何一条都说得通，摆在一起就不可能同时成立。

`SOW-2024-031.md` 第 7 条约定「验收后 30 日内一次性付款」，而附录 B 又写「按季度分四期结算」。这不是措辞差异，是两个不同的现金流承诺：签下去之后无论按哪一条执行，另一方都能拿另一条主张违约。同一份合同里出现两套结算节奏，说明附录是从旧模板整段搬来的，改主文时没人回头改附件。

`NDA-2024-018.md` 的保密期写成「自签署之日起五年」，但同一份文件的违约责任条款里引用了「保密期届满后两年内仍不得披露」。两句连读等于把保密义务解释成五年加两年，与主文的五年不符，也与对方在其他项目里接受的三年口径不一致。这类条款不会在签署当天出问题，只会在两年后有人翻旧账时出问题。
""",
    ),
    ("user", "另外两份呢？"),
    (
        "assistant",
        """另外两份的问题轻一些，但也不该原样签。

一份的不可抗力条款把「供应商上游涨价」列为免责事由，这已经不是不可抗力，是商业风险转移；另一份的争议解决地写成「甲方所在地」，而甲方注册地在境外，实际仲裁成本会比标的额还高。

建议按风险从高到低处理：先改结算节奏，再统一保密期口径，最后调整不可抗力与争议解决地。前两项涉及金额与责任，最好在下一轮谈判前定稿。
""",
    ),
]


def seed(home: Path) -> None:
    db = home / "peco.db"
    if not db.exists():
        raise SystemExit(f"找不到 {db}：daemon 还没建库，先等它引导完成再灌数据")

    conn = sqlite3.connect(db)
    conn.execute("PRAGMA foreign_keys = ON")
    try:
        with conn:
            for team_id, display in TEAMS.items():
                conn.execute(
                    "INSERT OR REPLACE INTO teams (id, display_name, template_version) "
                    "VALUES (?, ?, 0)",
                    (team_id, display),
                )

            # 重跑幂等：先按本次数据的前缀清干净，再整批插入。
            for ws_id, *_ in DEMO_SPACES:
                conn.execute("DELETE FROM workspaces WHERE id = ?", (f"ws_demo_{ws_id}",))
            conn.execute("DELETE FROM messages WHERE conversation_id = ?", (CONVERSATION_ID,))
            conn.execute("DELETE FROM conversations WHERE id = ?", (CONVERSATION_ID,))
            conn.execute("DELETE FROM d_dispatches WHERE task_id LIKE 'e2e-%'")

            for index, (suffix, label, team, closed) in enumerate(DEMO_SPACES):
                ws_id = f"ws_demo_{suffix}"
                work_dir = home / "spaces" / f"demo_{suffix}" / "work"
                work_dir.mkdir(parents=True, exist_ok=True)
                conn.execute(
                    "INSERT INTO workspaces (id, work_dir, team_id, label, exit_policy, "
                    "created_at, last_opened_at, closed_at) VALUES (?, ?, ?, ?, 'inherit', ?, ?, ?)",
                    (
                        ws_id,
                        str(work_dir),
                        team,
                        label,
                        utc(days=30),
                        None if closed else utc(hours=index + 1),
                        utc(days=2) if closed else None,
                    ),
                )

            for row in dispatches():
                columns = ", ".join(row)
                marks = ", ".join("?" for _ in row)
                conn.execute(
                    f"INSERT INTO d_dispatches ({columns}) VALUES ({marks})",
                    tuple(row.values()),
                )

            conn.execute(
                "INSERT INTO conversations (id, user_id, agent_name, workspace_id, title, "
                "created_at, updated_at, scope) VALUES (?, 'local', '@master', ?, ?, ?, ?, 'agent')",
                (
                    CONVERSATION_ID,
                    CONVERSATION_WS,
                    "合同清单复核",
                    utc(hours=6),
                    utc(hours=1),
                ),
            )
            for index, (role, content) in enumerate(CONVERSATION_MESSAGES):
                conn.execute(
                    "INSERT INTO messages (id, conversation_id, role, content, agent_name, created_at) "
                    "VALUES (?, ?, ?, ?, '@master', ?)",
                    (
                        f"msg-demo-{index:02d}",
                        CONVERSATION_ID,
                        role,
                        content,
                        utc(minutes=60 - index * 5),
                    ),
                )
    finally:
        conn.close()

    print(
        f"[seed] {db}: {len(DEMO_SPACES)} 个演示空间 + {len(dispatches())} 条台账行 + "
        f"1 条会话（{len(CONVERSATION_MESSAGES)} 条消息）"
    )


def main() -> int:
    parser = argparse.ArgumentParser(description="写入浏览器验收台的演示数据")
    parser.add_argument("--home", help="PECO_HOME，缺省读环境变量", default=None)
    args = parser.parse_args()

    raw = args.home or os.environ.get("PECO_HOME")
    if not raw:
        print("缺少 PECO_HOME（或 --home）", file=sys.stderr)
        return 2
    seed(Path(raw))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
