import { describe, expect, it } from "vitest";
import {
  decodeOutputTail,
  liveInflightCount,
  liveTaskElapsedMs,
  outcomeOutputText,
  progressLabel,
  sortLiveTasks,
  tailTextForDisplay,
} from "@/lib/liveTasks";
import type { LiveTaskView } from "@/types/daemon";

/** 中文每个字占 3 字节，正好用来验证窗口裁剪不切坏字符。 */
const NI_HAO = [0xe4, 0xbd, 0xa0, 0xe5, 0xa5, 0xbd];

function liveTask(overrides: Partial<LiveTaskView> = {}): LiveTaskView {
  return {
    id: "task_1",
    kind: "echo",
    state: "working",
    state_detail: null,
    progress: { phase: null, current: null, total: null },
    output: { tail: [], start_offset: 0, end_offset: 0 },
    outcome: null,
    artifacts: [],
    created_at: "2026-11-05T10:00:00Z",
    updated_at: "2026-11-05T10:00:30Z",
    ...overrides,
  };
}

describe("输出窗口解码", () => {
  it("完整字节解成原文", () => {
    expect(decodeOutputTail(NI_HAO)).toBe("你好");
  });

  it("窗口从字符中间开始：丢掉续接字节，不产生替换符", () => {
    expect(decodeOutputTail([0xbd, 0xa0, ...NI_HAO])).toBe("你好");
  });

  it("窗口在字符中间结束：丢掉半个字符，后面的字节不受影响", () => {
    expect(decodeOutputTail([...NI_HAO, 0xe5, 0xa5])).toBe("你好");
  });

  it("空窗口与只有续接字节的窗口都解出空串", () => {
    expect(decodeOutputTail([])).toBe("");
    expect(decodeOutputTail([0xbd, 0xa0])).toBe("");
  });

  it("窗口超过展示上限时只留末尾并标出截断", () => {
    const text = "a".repeat(5000) + "尾部";
    const tail = Array.from(new TextEncoder().encode(text));
    const displayed = tailTextForDisplay(tail);
    expect(displayed.truncated).toBe(true);
    expect(displayed.text).toHaveLength(4000);
    expect(displayed.text.endsWith("尾部")).toBe(true);
  });

  it("未超上限时不标截断", () => {
    expect(tailTextForDisplay(NI_HAO)).toEqual({
      text: "你好",
      truncated: false,
    });
  });
});

describe("进度一句话", () => {
  it.each([
    [{ phase: "生成中", current: 3, total: 10 }, "生成中 3 / 10"],
    [{ phase: null, current: 3, total: null }, "3"],
    [{ phase: "等待工具", current: null, total: null }, "等待工具"],
    [{ phase: "  ", current: null, total: null }, null],
    [null, null],
  ])("%o → %s", (progress, expected) => {
    expect(progressLabel(progress)).toBe(expected);
  });
});

describe("耗时", () => {
  it("在飞按「到现在」算", () => {
    const task = liveTask();
    const now = Date.parse("2026-11-05T10:00:45Z");
    expect(liveTaskElapsedMs(task, now)).toBe(45_000);
  });

  it("终态按 updated_at 算", () => {
    const task = liveTask({ state: "completed" });
    expect(liveTaskElapsedMs(task, Date.parse("2026-11-06T00:00:00Z"))).toBe(
      30_000,
    );
  });

  it("时间串读不出来时返回 null", () => {
    expect(
      liveTaskElapsedMs(liveTask({ created_at: "not a date" })),
    ).toBeNull();
  });
});

describe("列表顺序与在飞计数", () => {
  it("在飞在前，同组按创建时间倒序", () => {
    const tasks = sortLiveTasks([
      liveTask({
        id: "done_old",
        state: "completed",
        created_at: "2026-11-05T09:00:00Z",
      }),
      liveTask({
        id: "running_old",
        state: "working",
        created_at: "2026-11-05T08:00:00Z",
      }),
      liveTask({
        id: "done_new",
        state: "failed",
        created_at: "2026-11-05T11:00:00Z",
      }),
      liveTask({
        id: "running_new",
        state: "submitted",
        created_at: "2026-11-05T10:00:00Z",
      }),
    ]);
    expect(tasks.map((task) => task.id)).toEqual([
      "running_new",
      "running_old",
      "done_new",
      "done_old",
    ]);
  });

  it("在飞 = 非终态：等补输入的也算", () => {
    expect(
      liveInflightCount([
        liveTask({ id: "a", state: "working" }),
        liveTask({ id: "b", state: "input_required" }),
        liveTask({ id: "c", state: "completed" }),
      ]),
    ).toBe(2);
  });

  it("还没拿到数据时返回 null，让调用方决定回落", () => {
    expect(liveInflightCount(undefined)).toBeNull();
  });
});

describe("终态完整输出", () => {
  it("字符串载荷原样给", () => {
    expect(
      outcomeOutputText({ tag: "echo", payload: "整理好了，见表格。" }),
    ).toBe("整理好了，见表格。");
  });

  it("结构化载荷转成缩进 JSON", () => {
    expect(
      outcomeOutputText({ tag: "echo", payload: { ok: true, count: 2 } }),
    ).toBe('{\n  "ok": true,\n  "count": 2\n}');
  });

  it("没有输出时返回 null", () => {
    expect(outcomeOutputText(null)).toBeNull();
    expect(outcomeOutputText({ tag: "echo", payload: null })).toBeNull();
  });
});
