import { describe, expect, it } from "vitest";
import { fireEvent, render, screen, within } from "@testing-library/react";
import { SpaceTasksTab } from "@/components/spaces/SpaceTasksTab";
import { formatCreatedAt } from "@/lib/format";
import type { Task } from "@/types/daemon";

// 台账的六列在均分后的右栏（616）里塞不下：这里量两件事 ——
// 每一档出现哪几列，以及摘要还剩多少。jsdom 不做布局，宽度由类名还原：
// 档位是容器变体里的方括号像素值（`/panel:block` 这一段），定宽来自 `w-*`。

const COLUMNS = ["状态", "agent", "来源", "耗时", "提交", "摘要"] as const;
type Column = (typeof COLUMNS)[number];

/** Tailwind 的间距单位。 */
const UNIT_PX = 4;
/** 行内距 `px-3`。 */
const ROW_PADDING_PX = 12 * 2;
/** `gap-2`。 */
const GAP_PX = 8;
/** 连接状态单元：8px 点 + 6px 缝 + 「未连接」3 个 12px 汉字 + `mr-2`。 */
const STATUS_CELL_PX = 8 + 6 + 36 + 8;

function task(overrides: Partial<Task> = {}): Task {
  return {
    id: "t_1",
    task_id: "t_1",
    target_ws_id: "ws_a",
    target_work_dir: "/tmp/spaces/ws_a/work",
    target_team_id: "research",
    origin_ws_id: "ws_peco",
    origin_kind: "dispatch",
    agent_name: "coder",
    task_state: "working",
    wake_state: "delivered",
    wake_skip_reason: null,
    created_at: "2026-11-05 10:00:00",
    terminal_at: "2026-11-05 11:00:00",
    result_digest: "把合同里的违约条款摘出来",
    ...overrides,
  };
}

function renderTab() {
  return render(
    <SpaceTasksTab
      tasks={[task()]}
      isPending={false}
      error={null}
      onRetry={() => {}}
    />,
  );
}

/** 表头行：第一个子元素是连接状态，之后才是六列。 */
function headerCells(header: HTMLElement): HTMLElement[] {
  return Array.from(header.children).slice(1) as HTMLElement[];
}

function headerRow(): HTMLElement {
  return screen.getByText("摘要").parentElement as HTMLElement;
}

function dataRow(): HTMLElement {
  return screen.getByRole("button", { expanded: false });
}

/** `hidden … @min-[520px]/panel:block` → 520；没挂档位的列常显。 */
function breakpointOf(cell: HTMLElement): number {
  const match = cell.className.match(/@min-\[(\d+)px\]\/panel:block/);
  if (!match) {
    // 常显列不该同时被 hidden 藏起来，否则哪一档都看不见。
    expect(cell.className).not.toContain("hidden");
    return 0;
  }
  expect(cell.className).toContain("hidden");
  return Number(match[1]);
}

/** `w-36` → 144。 */
function widthOf(cell: HTMLElement): number {
  const match = cell.className.match(/(?:^|\s)w-(\d+)(?:\s|$)/);
  return match ? Number(match[1]) * UNIT_PX : 0;
}

/**
 * 面板宽 `panel` 时摘要拿到多少像素。
 *
 * 摘要吃掉定宽列与间隙以外的全部余量，所以「摘要还剩多少」等价于
 * 「其他列加起来有没有超宽」。
 */
function summaryWidth(
  panel: number,
  gates: Map<Column, number>,
  widths: Map<Column, number>,
  only?: readonly Column[],
): number {
  const visible = only ?? COLUMNS.filter((name) => gates.get(name)! <= panel);
  const fixed = visible
    .filter((name) => name !== "摘要")
    .reduce((sum, name) => sum + widths.get(name)!, 0);
  // 可见列之外还有连接状态单元，间隙数 = 子元素数 − 1 = 可见列数。
  const gaps = visible.length * GAP_PX;
  return panel - ROW_PADDING_PX - STATUS_CELL_PX - fixed - gaps;
}

describe("SpaceTasksTab 的列宽档", () => {
  it("表头与数据行的列可见性一一对应：耗时 520，来源与提交 840", () => {
    renderTab();

    const columns = headerCells(headerRow());
    const rowCells = Array.from(dataRow().children) as HTMLElement[];
    expect(columns).toHaveLength(COLUMNS.length);
    expect(rowCells).toHaveLength(COLUMNS.length);

    const gates = new Map<Column, number>();
    COLUMNS.forEach((name, index) => {
      const gate = breakpointOf(columns[index]);
      // 表头出现而数据行不出现（或反过来）就是错位的列。
      expect(breakpointOf(rowCells[index])).toBe(gate);
      gates.set(name, gate);
    });

    expect(gates.get("状态")).toBe(0);
    expect(gates.get("agent")).toBe(0);
    expect(gates.get("耗时")).toBe(520);
    expect(gates.get("来源")).toBe(840);
    expect(gates.get("提交")).toBe(840);
  });

  it("摘要在每一档都留得下：最窄的 481 也不为 0", () => {
    renderTab();

    const columns = headerCells(headerRow());
    const rowCells = Array.from(dataRow().children) as HTMLElement[];
    const gates = new Map<Column, number>();
    const widths = new Map<Column, number>();
    COLUMNS.forEach((name, index) => {
      gates.set(name, breakpointOf(columns[index]));
      widths.set(name, widthOf(columns[index]));
      expect(widthOf(rowCells[index])).toBe(widthOf(columns[index]));
    });
    // 摘要不定宽：靠 flex-1 吃余量，且必须能截断。
    for (const cell of [columns[5], rowCells[5]]) {
      expect(cell).toHaveClass("min-w-0");
      expect(cell).toHaveClass("flex-1");
    }

    // 481 = 窄屏独占右栏，616 = 1440 双开均分，1232 = 1440 独占。
    const cases = [
      { panel: 481, visible: ["状态", "agent", "摘要"], minSummary: 120 },
      {
        panel: 616,
        visible: ["状态", "agent", "耗时", "摘要"],
        minSummary: 160,
      },
      {
        panel: 1232,
        visible: ["状态", "agent", "来源", "耗时", "提交", "摘要"],
        minSummary: 500,
      },
    ] as const;

    for (const { panel, visible, minSummary } of cases) {
      expect(COLUMNS.filter((name) => gates.get(name)! <= panel)).toEqual(
        visible,
      );
      expect(summaryWidth(panel, gates, widths)).toBeGreaterThanOrEqual(
        minSummary,
      );
    }

    // 六列全开在 616 里放不下：档位是这组数字的前提，不是装饰。
    expect(summaryWidth(616, gates, widths, COLUMNS)).toBeLessThan(0);
  });

  it("展开详情补上被藏起来的那一列：提交时间", () => {
    renderTab();
    fireEvent.click(dataRow());

    const detail = screen.getByText(/^task_id/).parentElement as HTMLElement;
    const submitted = within(detail).getByText(/^提交/);
    // 提交列在小于 840 时不可见，这条信息不能跟着列一起消失。
    expect(submitted).toHaveTextContent(
      `提交 ${formatCreatedAt("2026-11-05 10:00:00")}`,
    );
    // 写的是提交时刻，不是终态时刻。
    expect(submitted).not.toHaveTextContent(
      formatCreatedAt("2026-11-05 11:00:00"),
    );
  });
});
