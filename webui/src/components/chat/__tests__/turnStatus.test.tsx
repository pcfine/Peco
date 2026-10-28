// 最终结论状态行 — 成功/失败/取消圆点 + 本轮总耗时；结论气泡与聊天区同宽

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import {
  ChatView,
  markTurnResult,
  reduceStreamEvent,
  snapshotToMessages,
  type ChatMessage,
} from "../ChatView";
import { TooltipProvider } from "@/components/ui/tooltip";
import { useAuthStore } from "@/stores/authStore";
import type { ChatSseEvent, TurnData } from "../../../types/chat";

(
  globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT: boolean }
).IS_REACT_ACT_ENVIRONMENT = true;

let container: HTMLDivElement;
let root: Root;

beforeEach(() => {
  useAuthStore.setState({ token: "test-token" });
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
});

afterEach(() => {
  act(() => root.unmount());
  container.remove();
  vi.restoreAllMocks();
});

function renderMessages(messages: ChatMessage[]) {
  return act(async () => {
    root.render(
      createElement(
        TooltipProvider,
        null,
        createElement(ChatView, {
          mode: "external",
          streamUrl: (m: string) => `/api/peco/stream?message=${m}`,
          initialMessages: messages,
          onExternalSend: () => {},
          onExternalStop: () => {},
          externalIsStreaming: false,
        }),
      ),
    );
  });
}

const slot = (name: string) =>
  container.querySelector(`[data-slot="${name}"]`) as HTMLElement | null;

const START_MS = 1_000_000;

/** 实时轮的最小形状：用户提问 + 已有正文的 assistant 宿主。 */
function liveTurn(content = "答"): ChatMessage[] {
  return [
    {
      role: "user",
      content: "问",
      turnIndex: 0,
      timestampMs: START_MS,
    },
    {
      role: "assistant",
      content,
      turnIndex: 0,
      timestampMs: START_MS,
    },
  ];
}

const TURN_COMPLETE: ChatSseEvent = {
  event: "turn_complete",
  data: {
    text: "答",
    usage: { input_tokens: 1, output_tokens: 1 },
    conversation_id: "c",
  },
};

const errorEvent = (message: string, cancelled?: boolean): ChatSseEvent => ({
  event: "error",
  data: { message, cancelled, conversation_id: "c" },
});

// ── 实时路径：终态挂到结论宿主 ───────────────────────────────────────────

describe("reduceStreamEvent 终态", () => {
  it("turn_complete 给结论宿主挂成功态与耗时", () => {
    const updated = reduceStreamEvent(
      TURN_COMPLETE,
      liveTurn(),
      START_MS + 4000,
    );

    expect(updated[1].turnResult).toEqual({
      status: "success",
      durationMs: 4000,
    });
  });

  it("error 把失败态挂在错误气泡上，耗时同口径", () => {
    const updated = reduceStreamEvent(
      errorEvent("模型报错"),
      liveTurn(),
      START_MS + 2500,
    );

    const bubble = updated.at(-1);
    expect(bubble?.isError).toBe(true);
    expect(bubble?.turnResult).toEqual({
      status: "failed",
      durationMs: 2500,
    });
  });

  it("取消被收编为失败原因时还原成「取消」态", () => {
    const updated = reduceStreamEvent(
      errorEvent("对话已被取消（响应中断前部分输出已展示）"),
      liveTurn("部分输出"),
      START_MS + 1000,
    );

    expect(updated.at(-1)?.turnResult?.status).toBe("cancelled");
  });

  it("error.cancelled=true 直接还原成「取消」态，不依赖文案", () => {
    // 文案本身不像取消：判定必须认服务端字段，否则会被当成失败
    const updated = reduceStreamEvent(
      errorEvent("上游超时", true),
      liveTurn("部分输出"),
      START_MS + 1000,
    );

    expect(updated.at(-1)?.turnResult?.status).toBe("cancelled");
  });

  it("本地取消给最后一条宿主挂取消态", () => {
    const updated = markTurnResult(
      liveTurn("部分输出"),
      "cancelled",
      START_MS + 800,
    );

    expect(updated[1].turnResult).toEqual({
      status: "cancelled",
      durationMs: 800,
    });
  });

  it("缺起点时间戳时只留状态、不编造耗时", () => {
    const messages: ChatMessage[] = [
      { role: "user", content: "问", turnIndex: 0 },
      { role: "assistant", content: "答", turnIndex: 0 },
    ];

    expect(markTurnResult(messages, "success", START_MS)[1].turnResult).toEqual(
      { status: "success", durationMs: undefined },
    );
  });
});

// ── 快照路径：终态与耗时由轮数据还原 ─────────────────────────────────────

describe("snapshotToMessages 终态", () => {
  const completedTurn = (): TurnData => ({
    turn_index: 0,
    messages: [
      { role: "user", content: "问", timestamp_ms: START_MS },
      { role: "assistant", content: "答", timestamp_ms: START_MS + 12_300 },
    ],
  });

  it("正常轮挂成功态，耗时取首末消息时间差", () => {
    const messages = snapshotToMessages([completedTurn()]);

    expect(messages[1].turnResult).toEqual({
      status: "success",
      durationMs: 12_300,
    });
  });

  it("取消冻结轮挂取消态，其他中断原因挂失败态", () => {
    const cancelled: TurnData = {
      ...completedTurn(),
      interrupted: true,
      interrupted_reason: "cancelled by user",
    };
    const failed: TurnData = {
      ...completedTurn(),
      interrupted: true,
      interrupted_reason: "max iterations exceeded",
    };

    // 中断轮在最前多一条居中横幅，结论宿主恒在本轮末尾
    expect(snapshotToMessages([cancelled]).at(-1)?.turnResult?.status).toBe(
      "cancelled",
    );
    expect(snapshotToMessages([failed]).at(-1)?.turnResult?.status).toBe(
      "failed",
    );
  });

  it("只有工具调用、没有正文的轮次不挂状态（无结论气泡可挂）", () => {
    const messages = snapshotToMessages([
      {
        turn_index: 0,
        messages: [
          { role: "user", content: "问", timestamp_ms: START_MS },
          {
            role: "assistant",
            content: "",
            timestamp_ms: START_MS + 500,
            tool_calls: [{ id: "c1", name: "shell", arguments: "{}" }],
          },
        ],
      },
    ]);

    expect(messages.every((m) => m.turnResult === undefined)).toBe(true);
  });
});

// ── 渲染：状态行在结论气泡之上，气泡占满聊天区宽度 ───────────────────────

describe("TurnStatusRow 渲染", () => {
  const statusOf = (status: "success" | "failed" | "cancelled") => ({
    status,
    durationMs: 12_300,
  });

  it("成功：绿色圆点 + 状态 + 耗时，且在结论气泡之前", async () => {
    await renderMessages([
      {
        role: "assistant",
        content: "结论",
        turnIndex: 0,
        turnResult: statusOf("success"),
      },
    ]);

    const row = slot("turn-status");
    expect(row?.getAttribute("data-status")).toBe("success");
    expect(row?.textContent).toContain("成功");
    expect(row?.textContent).toContain("12.3 秒");
    expect(row?.querySelector("span")?.className).toContain("bg-emerald-500");

    const column = slot("assistant-column");
    expect(
      [...(column?.children ?? [])].map((el) => el.getAttribute("data-slot")),
    ).toEqual(["turn-status", "assistant-result"]);
  });

  it("失败：错误气泡上方是红色圆点", async () => {
    await renderMessages([
      {
        role: "assistant",
        content: "模型报错",
        turnIndex: 0,
        isError: true,
        turnResult: statusOf("failed"),
      },
    ]);

    const row = slot("turn-status");
    expect(row?.getAttribute("data-status")).toBe("failed");
    expect(row?.textContent).toContain("失败");
    expect(row?.querySelector("span")?.className).toContain("bg-destructive");
    expect(slot("assistant-bubble")).not.toBeNull();
  });

  it("取消：黄色圆点", async () => {
    await renderMessages([
      {
        role: "assistant",
        content: "部分输出",
        turnIndex: 0,
        turnResult: statusOf("cancelled"),
      },
    ]);

    const row = slot("turn-status");
    expect(row?.getAttribute("data-status")).toBe("cancelled");
    expect(row?.textContent).toContain("取消");
    expect(row?.querySelector("span")?.className).toContain("bg-amber-500");
  });

  it("结论气泡占满可用宽度", async () => {
    await renderMessages([
      { role: "assistant", content: "结论", turnIndex: 0 },
    ]);

    const result = slot("assistant-result");
    expect(result?.classList.contains("w-full")).toBe(true);
    expect(result?.classList.contains("w-fit")).toBe(false);
    expect(result?.classList.contains("max-w-[80%]")).toBe(false);
  });

  it("没有终态的在途轮不渲染状态行", async () => {
    await renderMessages([
      { role: "assistant", content: "生成中", turnIndex: 0 },
    ]);

    expect(slot("turn-status")).toBeNull();
  });
});
