import { describe, expect, it } from "vitest";
import { fireEvent, render, screen } from "@testing-library/react";
import { DispatchTimeline } from "@/components/results/DispatchTimeline";
import type { Task } from "@/types/daemon";

function task(overrides: Partial<Task> = {}): Task {
  return {
    id: "d_1",
    task_id: "t_1",
    target_ws_id: "ws_a",
    target_work_dir: "/tmp/spaces/a/work",
    target_team_id: "research",
    origin_ws_id: "ws_peco",
    origin_kind: "peco",
    agent_name: "@master",
    task_state: "orphan",
    wake_state: "skipped",
    wake_skip_reason: null,
    created_at: "2026-11-05 10:00:00",
    terminal_at: "2026-11-05 10:05:00",
    result_digest: "改了三个文件",
    ...overrides,
  };
}

function renderRow(row: Task) {
  return render(<DispatchTimeline tasks={[row]} spaces={[]} />);
}

function expandOnlyRow() {
  fireEvent.click(screen.getByRole("button", { expanded: false }));
}

describe("成果时间线的唤醒信息", () => {
  it("skipped 在状态旁显示中文跳过原因", () => {
    renderRow(
      task({ wake_state: "skipped", wake_skip_reason: "space_closed" }),
    );

    expect(screen.getByText("已跳过 · 目标空间已关闭")).toBeInTheDocument();
    // 完整文案进 title，表格里放短文案。
    expect(screen.getByTitle("已跳过 · 目标空间已关闭")).toBeInTheDocument();
  });

  it("历史 skipped 行没有原因时写明原因未记录，不猜一种", () => {
    renderRow(task({ wake_state: "skipped", wake_skip_reason: null }));

    expect(screen.getByText("已跳过 · 原因未记录")).toBeInTheDocument();
  });

  it("failed 只显示投递失败，不拼原因", () => {
    renderRow(task({ wake_state: "failed", wake_skip_reason: null }));

    expect(screen.getByText("投递失败")).toBeInTheDocument();
    expect(screen.queryByText(/原因未记录/)).toBeNull();
  });

  it("delivered 收起与展开都不出现唤醒信息", () => {
    renderRow(
      task({
        task_state: "completed",
        wake_state: "delivered",
        wake_skip_reason: null,
      }),
    );

    expect(screen.queryByText(/已投递/)).toBeNull();
    expect(screen.queryByText(/原因未记录/)).toBeNull();

    expandOnlyRow();

    expect(screen.queryByText(/^唤醒：/)).toBeNull();
  });

  it("展开详情给出完整唤醒一行", () => {
    renderRow(
      task({
        wake_state: "skipped",
        wake_skip_reason: "attempts_exhausted",
      }),
    );

    expandOnlyRow();

    expect(screen.getByText(/唤醒：/)).toHaveTextContent(
      "唤醒：已跳过 · 重试次数用尽",
    );
  });
});
