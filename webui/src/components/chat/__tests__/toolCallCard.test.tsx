// ToolCallCard 的渲染契约 + 它在 ChatView 里的挂载位置

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { ToolCallCard } from "../ToolCallCard";
import { ChatView, type ChatMessage } from "../ChatView";
import { useAuthStore } from "@/stores/authStore";
import { TooltipProvider } from "@/components/ui/tooltip";
import {
  ARGUMENTS_CHAR_CAP,
  OUTPUT_CHAR_CAP,
  type ToolCallView,
} from "@/lib/toolCall";

(
  globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT: boolean }
).IS_REACT_ACT_ENVIRONMENT = true;

let container: HTMLDivElement;
let root: Root;

beforeEach(() => {
  vi.clearAllMocks();
  useAuthStore.setState({ token: "test-token" });
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
});

afterEach(() => {
  act(() => root.unmount());
  container.remove();
});

const trigger = () =>
  container.querySelector('[data-slot="collapsible-trigger"]') as HTMLElement;
const content = () =>
  container.querySelector('[data-slot="collapsible-content"]') as HTMLElement;
const section = (name: "arguments" | "output") =>
  container.querySelector(`[data-section="${name}"]`) as HTMLElement | null;

async function renderCard(toolCall: ToolCallView) {
  await act(async () => {
    root.render(
      <ul>
        <ToolCallCard toolCall={toolCall} />
      </ul>,
    );
  });
  return container.querySelector('[data-slot="tool-call-card"]') as HTMLElement;
}

async function expand() {
  await act(async () => {
    trigger().click();
  });
}

async function renderChatView(messages: ChatMessage[]) {
  await act(async () => {
    root.render(
      <TooltipProvider>
        <ChatView
          mode="external"
          streamUrl={(m) => `/api/peco/stream?message=${m}`}
          initialMessages={messages}
          onExternalSend={() => {}}
          onExternalStop={() => {}}
          externalIsStreaming={false}
        />
      </TooltipProvider>,
    );
  });
}

const RUNNING: ToolCallView = {
  id: "call_1",
  name: "shell",
  arguments: '{"command":"ls -l"}',
  status: "pending",
};

describe("折叠行", () => {
  it("默认折叠：aria-expanded=false 且展开体为空", async () => {
    await renderCard(RUNNING);

    expect(trigger().getAttribute("aria-expanded")).toBe("false");
    // Radix 折叠时保留外壳、丢弃子树：判据看内容而不是节点是否存在
    expect(content().hasAttribute("hidden")).toBe(true);
    expect(content().childElementCount).toBe(0);
  });

  it("有裸 bg-* 类即判红（折叠行必须静态透明）", async () => {
    await renderCard(RUNNING);

    const bareBg = [...trigger().classList].filter(
      (token) => !token.includes(":") && token.startsWith("bg-"),
    );
    expect(bareBg).toEqual([]);
    // 反向保护：上一条断言在实现删掉 hover 反馈时也不会假绿
    expect(trigger().className).toContain("hover:bg-tool-row-hover");
  });

  it("工具名与参数摘要分列显示", async () => {
    await renderCard(RUNNING);

    expect(trigger().textContent).toContain("shell");
    expect(trigger().textContent).toContain("ls -l");
  });

  it("两态 aria-label 精确匹配：参数摘要进标签、动作词不进", async () => {
    await renderCard(RUNNING);
    expect(trigger().getAttribute("aria-label")).toBe("shell，执行中，ls -l");

    await renderCard({ ...RUNNING, status: "done", result: "total 0" });
    expect(trigger().getAttribute("aria-label")).toBe("shell，已完成，ls -l");

    expect(trigger().getAttribute("aria-label")).not.toMatch(/展开|收起/);
  });

  it("无摘要时 aria-label 省略参数段", async () => {
    await renderCard({
      id: "c2",
      name: "read_file",
      arguments: "",
      status: "missing",
    });
    expect(trigger().getAttribute("aria-label")).toBe("read_file，输出缺失");
  });

  it("卡片带 tool-id / tool-name / status 钩子", async () => {
    const card = await renderCard({ ...RUNNING, status: "done", result: "x" });

    expect(card.dataset.toolId).toBe("call_1");
    expect(card.dataset.toolName).toBe("shell");
    expect(card.dataset.status).toBe("done");
    expect(card.hasAttribute("data-orphan")).toBe(false);
  });

  it("孤儿卡另标 data-orphan", async () => {
    const card = await renderCard({
      id: "",
      name: "工具输出",
      arguments: "",
      result: "无主输出",
      status: "done",
      orphan: true,
    });

    expect(card.dataset.orphan).toBe("true");
    expect(card.dataset.status).toBe("done");
  });
});

describe("展开体", () => {
  it("是描边 + 抬起的面，参数与输出分区显示", async () => {
    await renderCard({
      id: "call_1",
      name: "shell",
      arguments: '{"command":"ls -l"}',
      status: "done",
      result: "a\nb",
    });
    await expand();

    expect(content().className).toContain("bg-tool-surface");
    expect(content().className).toContain("border");
    expect(section("arguments")?.textContent).toBe('{"command":"ls -l"}');
    expect(section("output")?.textContent).toBe("a\nb");
    // 输出保留原始排版并可横向滚动
    expect(section("output")?.className).toContain("whitespace-pre");
    expect(section("output")?.className).toContain("overflow-auto");
  });

  it("pending 显示等待占位", async () => {
    await renderCard(RUNNING);
    await expand();

    const empty = container.querySelector('[data-slot="tool-empty"]');
    expect(empty?.getAttribute("data-reason")).toBe("pending");
    expect(empty?.textContent).toBe("等待工具返回…");
    expect(section("output")).toBeNull();
  });

  it("missing 显示缺输出占位", async () => {
    await renderCard({ ...RUNNING, status: "missing" });
    await expand();

    const empty = container.querySelector('[data-slot="tool-empty"]');
    expect(empty?.getAttribute("data-reason")).toBe("missing");
    expect(empty?.textContent).toBe("本轮未记录到该工具的输出");
  });

  it("done + 空输出显示空输出占位，不渲染空体", async () => {
    await renderCard({ ...RUNNING, status: "done", result: "" });
    await expand();

    expect(
      container
        .querySelector('[data-slot="tool-empty"]')
        ?.getAttribute("data-reason"),
    ).toBe("empty");
    expect(section("output")).toBeNull();
  });

  it("done 但 result 缺失同样按空输出处理（status 是唯一权威）", async () => {
    await renderCard({ ...RUNNING, status: "done" });
    await expand();

    expect(
      container
        .querySelector('[data-slot="tool-empty"]')
        ?.getAttribute("data-reason"),
    ).toBe("empty");
  });

  it("输出超 20000 字符时截断并提示", async () => {
    const long = "x".repeat(OUTPUT_CHAR_CAP + 1);
    await renderCard({ ...RUNNING, status: "done", result: long });
    await expand();

    const output = section("output");
    expect(output?.textContent).toHaveLength(OUTPUT_CHAR_CAP + 1);
    expect(output?.textContent?.endsWith("…")).toBe(true);
    const hint = container.querySelector('[data-slot="tool-truncated"]');
    expect(hint?.textContent).toContain(String(OUTPUT_CHAR_CAP));
    expect(hint?.textContent).toContain(String(OUTPUT_CHAR_CAP + 1));
  });

  it("恰好 20000 字符时不提示截断", async () => {
    await renderCard({
      ...RUNNING,
      status: "done",
      result: "x".repeat(OUTPUT_CHAR_CAP),
    });
    await expand();

    expect(container.querySelector('[data-slot="tool-truncated"]')).toBeNull();
  });

  it("参数超 8000 字符时提示截断", async () => {
    await renderCard({
      ...RUNNING,
      arguments: "x".repeat(ARGUMENTS_CHAR_CAP + 1),
    });
    await expand();

    expect(section("arguments")?.textContent).toHaveLength(
      ARGUMENTS_CHAR_CAP + 1,
    );
    expect(
      container.querySelector('[data-slot="tool-truncated"]')?.textContent,
    ).toContain(String(ARGUMENTS_CHAR_CAP));
  });

  it("有输出图片时渲染在展开体内", async () => {
    const url = "data:image/png;base64,AAAA";
    await renderCard({
      ...RUNNING,
      status: "done",
      result: "截图完成",
      images: [url],
    });
    await expand();

    const img = container.querySelector('img[alt="shell 输出图片"]');
    expect(img?.getAttribute("src")).toBe(url);
  });

  it("无图片时不渲染 img", async () => {
    await renderCard({ ...RUNNING, status: "done", result: "纯文本" });
    await expand();

    expect(container.querySelector("img")).toBeNull();
  });

  it("只有图片没有文本时图片可见，且不出空输出占位", async () => {
    // 只返回图片的 MCP 工具：text_view() 为空串、image_urls() 非空
    const url = "data:image/png;base64,AAAA";
    await renderCard({ ...RUNNING, status: "done", result: "", images: [url] });
    await expand();

    expect(
      container.querySelector('img[alt="shell 输出图片"]')?.getAttribute("src"),
    ).toBe(url);
    expect(container.querySelector('[data-slot="tool-empty"]')).toBeNull();
    expect(container.textContent).not.toContain("（工具返回空输出）");
  });

  it("status 是唯一权威：missing 带 result 时仍显示缺输出占位", async () => {
    await renderCard({ ...RUNNING, status: "missing", result: "x" });
    await expand();

    const empty = container.querySelector('[data-slot="tool-empty"]');
    expect(empty?.getAttribute("data-reason")).toBe("missing");
    expect(empty?.textContent).toBe("本轮未记录到该工具的输出");
    // 矛盾态的 result 不渲染
    expect(section("output")).toBeNull();
  });
});

describe("ChatView 中的工具卡", () => {
  const CARD: ToolCallView = {
    id: "call_1",
    name: "shell",
    arguments: '{"command":"ls"}',
    status: "done",
    result: "out",
  };

  const columnSlots = () =>
    [
      ...(container.querySelector('[data-slot="assistant-column"]')?.children ??
        []),
    ].map((el) => el.getAttribute("data-slot"));

  it("工具卡在气泡子树之外", async () => {
    await renderChatView([
      { role: "assistant", content: "正文", turnIndex: 0, toolCalls: [CARD] },
    ]);

    const result = container.querySelector('[data-slot="assistant-result"]');
    expect(result).not.toBeNull();
    expect(result?.querySelector('[data-slot="tool-call-card"]')).toBeNull();
    expect(
      container.querySelector('[data-slot="tool-call-card"]'),
    ).not.toBeNull();
  });

  it("列内顺序固定：推理 → 工具组 → 正文", async () => {
    await renderChatView([
      {
        role: "assistant",
        content: "正文",
        turnIndex: 0,
        reasoning: "想一想",
        toolCalls: [CARD],
      },
    ]);

    expect(columnSlots()).toEqual(["assistant-process", "assistant-result"]);
    expect(
      [
        ...(container.querySelector('[data-slot="assistant-process"]')
          ?.children ?? []),
      ].map((el) => el.getAttribute("data-slot")),
    ).toEqual(["reasoning-fold", "tool-call-group"]);
  });

  it("缺项跳过：只有正文时不渲染推理块与工具组", async () => {
    await renderChatView([
      { role: "assistant", content: "只有正文", turnIndex: 0 },
    ]);

    expect(columnSlots()).toEqual(["assistant-result"]);
  });

  it("空正文不渲染气泡", async () => {
    await renderChatView([
      { role: "user", content: "问", turnIndex: 0 },
      { role: "assistant", content: "", turnIndex: 0 },
    ]);

    expect(
      container.querySelector('[data-slot="assistant-result"]'),
    ).toBeNull();
    expect(
      container.querySelector('[data-slot="assistant-column"]'),
    ).toBeNull();
  });

  it("错误气泡保留 destructive 配色，不被整列判空吞掉", async () => {
    await renderChatView([
      { role: "assistant", content: "模型报错", turnIndex: 0, isError: true },
    ]);

    const bubble = container.querySelector('[data-slot="assistant-bubble"]');
    expect(bubble?.className).toContain("bg-destructive/10");
    expect(bubble?.className).toContain("border-destructive/30");
    expect(bubble?.className).toContain("text-destructive");
  });

  it("通知消息不受「全空不渲染」影响", async () => {
    await renderChatView([
      {
        role: "assistant",
        content: "更早的 2 轮对话已归档为摘要，仍在模型上下文中",
        turnIndex: 0,
        isNotice: true,
      },
    ]);

    expect(container.textContent).toContain("已归档为摘要");
  });
});

// ── v6.1 折叠行规格（结果优先）────────────────────────────────────────────
//
// 判据只碰 DOM / 类名 / 文本 / 属性 / 顺序 —— jsdom 没有布局与计算样式，
// 像素与计算值断言在浏览器里跑。

const DONE_WITH_TEXT: ToolCallView = {
  id: "call_done",
  name: "shell",
  arguments: '{"command":"ls -l"}',
  status: "done",
  result: "总用量 12\ndrwxr-xr-x",
};

const IMAGE_URL = "data:image/png;base64,AAAA";

const VIEW_MESSAGE: ChatMessage = {
  role: "assistant",
  content: "正文",
  turnIndex: 0,
  reasoning: "想一想",
  toolCalls: [DONE_WITH_TEXT],
};

const slot = (name: string) =>
  container.querySelector(`[data-slot="${name}"]`) as HTMLElement | null;

const foldRow = (kind: "tool" | "reasoning") =>
  container.querySelector(`[data-fold-row="${kind}"]`) as HTMLElement;

/** content() 只取文档序第一个展开体（推理行），工具行的展开体要按行取。 */
const foldBody = (kind: "tool" | "reasoning") =>
  foldRow(kind).parentElement?.querySelector(
    '[data-slot="collapsible-content"]',
  ) as HTMLElement;

/** 同一 root 反复 renderCard 会保留折叠态，按 aria-expanded 幂等展开。 */
async function ensureExpanded() {
  if (trigger().getAttribute("aria-expanded") === "false") await expand();
}

describe("v6.1 折叠行规格", () => {
  it("J1/J2/J3 两行共用同一串类，含 cursor-pointer 与 hover 且无裸底色", async () => {
    await renderChatView([VIEW_MESSAGE]);

    // 逐 token 相等（不是「都含」）：任一行多一个尺寸类即红
    expect([...foldRow("reasoning").classList]).toEqual([
      ...foldRow("tool").classList,
    ]);

    for (const row of [foldRow("tool"), foldRow("reasoning")]) {
      expect(row.classList.contains("cursor-pointer")).toBe(true);
      expect(row.className).toContain("hover:bg-tool-row-hover");
      const bareBg = [...row.classList].filter(
        (token) => !token.includes(":") && token.startsWith("bg-"),
      );
      expect(bareBg).toEqual([]);
    }
  });

  it("J4/J5 箭头是第一个子元素，状态槽靠右且在其后；推理行没有状态槽", async () => {
    await renderChatView([VIEW_MESSAGE]);

    const row = foldRow("tool");
    const chevron = row.firstElementChild as HTMLElement;
    expect(chevron.getAttribute("data-slot")).toBe("tool-chevron");

    const status = slot("tool-status");
    expect(status).not.toBeNull();
    expect(status?.classList.contains("ml-auto")).toBe(true);
    expect(
      chevron.compareDocumentPosition(status as HTMLElement) &
        Node.DOCUMENT_POSITION_FOLLOWING,
    ).toBeTruthy();

    expect(
      foldRow("reasoning").querySelector('[data-slot="tool-status"]'),
    ).toBeNull();
  });

  it("J6/J7 第二轨是参数摘要，结果退出折叠行", async () => {
    await renderCard(DONE_WITH_TEXT);

    expect(slot("tool-line")?.textContent).toBe("ls -l");
    expect(trigger().textContent).not.toContain("总用量 12");
  });

  it("J8 pending 无结果时用参数摘要兜底，颜色留在过程档", async () => {
    await renderCard(RUNNING);

    const line = slot("tool-line") as HTMLElement;
    expect(line.textContent).toBe("ls -l");
    expect(line.classList.contains("text-tool-args-fg")).toBe(true);
    expect(line.classList.contains("text-foreground")).toBe(false);

    // 占位也是「轨元素永远非空」的一部分：空轨会让行在读屏器里失去第二轨
    await renderCard({ ...RUNNING, status: "pending", arguments: "" });
    const waiting = slot("tool-line") as HTMLElement;
    expect(waiting.textContent).toBe("（参数未记录）");
    expect(waiting.classList.contains("text-muted-foreground")).toBe(true);
    expect(trigger().getAttribute("aria-label")).toBe("shell，执行中");

    await renderCard({ ...RUNNING, status: "missing" });
    const missing = slot("tool-line") as HTMLElement;
    expect(missing.textContent).toBe("ls -l");
    expect(missing.classList.contains("text-tool-args-fg")).toBe(true);
    expect(trigger().getAttribute("aria-label")).toBe("shell，输出缺失，ls -l");
  });

  it("J9 收起态展开体留壳且内容为空，展开后才有内容", async () => {
    await renderCard(DONE_WITH_TEXT);

    expect(content()).not.toBeNull();
    expect(content().hasAttribute("hidden")).toBe(true);
    expect(content().textContent).toBe("");

    await expand();
    expect(content().textContent).not.toBe("");
  });

  it("B3 展开态跨流式重渲染保持：折叠态不随内容重建", async () => {
    const streaming: ChatMessage = {
      role: "assistant",
      content: "",
      turnIndex: 0,
      reasoning: "想",
      toolCalls: [{ ...DONE_WITH_TEXT, status: "pending", result: undefined }],
    };
    await renderChatView([streaming]);
    for (const kind of ["reasoning", "tool"] as const) {
      await act(async () => {
        foldRow(kind).click();
      });
      expect(foldRow(kind).getAttribute("aria-expanded")).toBe("true");
    }

    // 同位置、同 key 的重渲染：推理变长 + 工具落定 + 正文到货
    await renderChatView([
      {
        ...streaming,
        content: "正文到货",
        reasoning: "想得更久了，于是推理正文变长",
        toolCalls: [DONE_WITH_TEXT],
      },
    ]);

    expect(foldRow("reasoning").getAttribute("aria-expanded")).toBe("true");
    expect(foldRow("tool").getAttribute("aria-expanded")).toBe("true");
    // 展开体读的是新 props 而不是留下的旧文本（旧文本只有「想」）
    expect(foldBody("reasoning").textContent).toContain("想得更久了");
    expect(foldBody("tool").textContent).not.toBe("");
  });

  it("B3 可以收起：连点两次回到 hidden 且内容清空", async () => {
    await renderChatView([
      {
        role: "assistant",
        content: "正文",
        turnIndex: 0,
        toolCalls: [DONE_WITH_TEXT],
      },
    ]);

    expect(content().hasAttribute("hidden")).toBe(true);
    for (let i = 0; i < 2; i += 1) {
      await act(async () => {
        foldRow("tool").click();
      });
    }

    expect(foldRow("tool").getAttribute("aria-expanded")).toBe("false");
    expect(content().hasAttribute("hidden")).toBe(true);
    expect(content().textContent).toBe("");
  });

  it("J10 推理行不含任何数字，工具行不含字符数", async () => {
    await renderChatView([VIEW_MESSAGE]);

    expect(foldRow("reasoning").textContent).not.toMatch(/\d/);
    expect(foldRow("tool").textContent).not.toMatch(/\d+\s*字符/);
  });

  it("J11/J12/J20 输出在参数之前，输出升到前景色且只有结果加字重", async () => {
    await renderCard(DONE_WITH_TEXT);
    await expand();

    const output = section("output") as HTMLElement;
    const args = section("arguments") as HTMLElement;
    expect(
      output.compareDocumentPosition(args) & Node.DOCUMENT_POSITION_FOLLOWING,
    ).toBeTruthy();

    expect(output.classList.contains("text-foreground")).toBe(true);
    expect(args.classList.contains("text-tool-args-fg")).toBe(true);

    expect(output.classList.contains("font-medium")).toBe(false);
    expect(slot("tool-line")?.classList.contains("font-medium")).toBe(false);
    expect(slot("tool-name")?.classList.contains("font-medium")).toBe(false);
    expect(args.classList.contains("font-medium")).toBe(false);

    // 单行轨靠这两条收尾：等宽让结果列对齐，truncate 兜住不换行
    const name = slot("tool-name") as HTMLElement;
    const line = slot("tool-line") as HTMLElement;
    expect(name.classList.contains("font-mono")).toBe(true);
    expect(line.classList.contains("font-mono")).toBe(true);
    expect(name.classList.contains("truncate")).toBe(true);
    expect(line.classList.contains("truncate")).toBe(true);
  });

  it("J13 参数未知时降级为「参数未记录」，不渲染参数区", async () => {
    await renderCard({ ...DONE_WITH_TEXT, arguments: "" });
    await expand();

    expect(slot("tool-args-missing")?.textContent).toBe("参数未记录");
    expect(section("arguments")).toBeNull();
  });

  it("J14 空态文案精确；仅图片无空态；missing 带 result 仍是缺输出", async () => {
    await renderCard(RUNNING);
    await ensureExpanded();
    expect(slot("tool-empty")?.textContent).toBe("等待工具返回…");

    await renderCard({ ...RUNNING, status: "missing" });
    await ensureExpanded();
    expect(slot("tool-empty")?.textContent).toBe("本轮未记录到该工具的输出");

    await renderCard({ ...RUNNING, status: "done", result: "" });
    await ensureExpanded();
    expect(slot("tool-empty")?.textContent).toBe("（工具返回空输出）");

    await renderCard({
      ...RUNNING,
      status: "done",
      result: "",
      images: [IMAGE_URL],
    });
    await ensureExpanded();
    expect(slot("tool-empty")).toBeNull();

    await renderCard({ ...RUNNING, status: "missing", result: "x" });
    await ensureExpanded();
    const empty = slot("tool-empty");
    expect(empty?.getAttribute("data-reason")).toBe("missing");
    expect(empty?.textContent).toBe("本轮未记录到该工具的输出");

    // 图片也是输出：status 说了没有输出，图片就不能单独露出来
    await renderCard({
      ...RUNNING,
      status: "missing",
      result: "x",
      images: [IMAGE_URL],
    });
    await ensureExpanded();
    expect(container.querySelector("img")).toBeNull();
    expect(slot("tool-empty")?.textContent).toBe("本轮未记录到该工具的输出");
  });

  it("J15 仅图片时第二轨说「输出 2 张图片」，用前景色且进 aria-label", async () => {
    await renderCard({
      id: "c_img",
      name: "browser",
      arguments: "{}",
      status: "done",
      result: "",
      images: [IMAGE_URL, IMAGE_URL],
    });

    const line = slot("tool-line") as HTMLElement;
    expect(line.textContent).toBe("输出 2 张图片");
    expect(line.classList.contains("text-tool-args-fg")).toBe(true);
    expect(trigger().getAttribute("aria-label")).toBe(
      "browser，已完成，输出 2 张图片",
    );
  });

  it("J16 aria-label 四例：真实轨文本追加，占位不追加", async () => {
    await renderCard(RUNNING);
    expect(trigger().getAttribute("aria-label")).toBe("shell，执行中，ls -l");

    await renderCard(DONE_WITH_TEXT);
    expect(trigger().getAttribute("aria-label")).toBe("shell，已完成，ls -l");

    await renderCard({ ...RUNNING, status: "done", result: "" });
    expect(trigger().getAttribute("aria-label")).toBe("shell，已完成，ls -l");

    await renderCard({
      id: "c2",
      name: "read_file",
      arguments: "",
      status: "missing",
    });
    expect(trigger().getAttribute("aria-label")).toBe("read_file，输出缺失");
  });

  it("S1 轨文本超 80 字符时 aria-label 截断并加省略号", async () => {
    await renderCard({
      ...RUNNING,
      status: "done",
      arguments: JSON.stringify({ command: "y".repeat(300) }),
    });

    expect(trigger().getAttribute("aria-label")).toBe(
      `shell，已完成，${"y".repeat(80)}…`,
    );
  });

  it("S1 边界：轨文本恰好 80 字符不截断，不加省略号", async () => {
    await renderCard({
      ...RUNNING,
      status: "done",
      arguments: JSON.stringify({ command: "y".repeat(80) }),
    });

    expect(trigger().getAttribute("aria-label")).toBe(
      `shell，已完成，${"y".repeat(80)}`,
    );
  });

  it("J17 推理行的可见标签与 aria-label 同文", async () => {
    await renderChatView([VIEW_MESSAGE]);

    expect(foldRow("reasoning").getAttribute("aria-label")).toBe("推理过程");
    expect(slot("reasoning-label")?.textContent).toBe("推理过程");
  });

  it("J18 卡片钩子不变，孤儿卡参数未知时降级", async () => {
    const card = await renderCard(DONE_WITH_TEXT);
    expect(card.dataset.toolId).toBe("call_done");
    expect(card.dataset.toolName).toBe("shell");
    expect(card.dataset.status).toBe("done");
    expect(card.hasAttribute("data-orphan")).toBe(false);

    const orphan = await renderCard({
      id: "",
      name: "工具输出",
      arguments: "",
      result: "无主输出",
      status: "done",
      orphan: true,
    });
    await ensureExpanded();

    expect(orphan.dataset.status).toBe("done");
    expect(orphan.dataset.orphan).toBe("true");
    expect(slot("tool-args-missing")?.textContent).toBe("参数未记录");
  });

  it("J19 消息滚动容器带 scroll hook", async () => {
    await renderChatView([VIEW_MESSAGE]);

    expect(
      document.querySelector('[data-slot="message-scroller"]'),
    ).not.toBeNull();
  });

  it("J21 工具名与推理标签的颜色互斥", async () => {
    await renderChatView([VIEW_MESSAGE]);

    const name = slot("tool-name") as HTMLElement;
    expect(name.classList.contains("text-muted-foreground")).toBe(true);
    expect(name.classList.contains("text-tool-args-fg")).toBe(false);

    const label = slot("reasoning-label") as HTMLElement;
    expect(label.classList.contains("text-muted-foreground")).toBe(true);
  });
});

// ── 结果优先 v7.1：过程组 / 结果区 / 参数轨 ─────────────────────────────────
//
// 既有描述块里的 J 编号是 v6.1 的自有标签，与本组 V-J 编号互不相干。

describe("v7.1 结果优先", () => {
  it("V-J1 done 态第二轨读参数，工具输出不上收起行", async () => {
    await renderCard({
      ...RUNNING,
      status: "done",
      arguments: '{"command":"CMD-SENTINEL"}',
      result: "LLM-SENTINEL",
    });

    const line = slot("tool-line") as HTMLElement;
    expect(line.textContent).toContain("CMD-SENTINEL");
    expect(line.textContent).not.toContain("LLM-SENTINEL");
    expect(line.classList.contains("text-tool-args-fg")).toBe(true);
    expect(trigger().getAttribute("aria-label")).toBe(
      "shell，已完成，CMD-SENTINEL",
    );
  });

  it("V-J4 无候选键命中时退回参数原文，不误判为未记录", async () => {
    await renderCard({
      ...RUNNING,
      status: "done",
      arguments: '{"steps":["a"]}',
    });

    const line = slot("tool-line") as HTMLElement;
    expect(line.textContent).not.toBe("");
    expect(line.textContent).not.toBe("（参数未记录）");
    expect(line.textContent).toContain("steps");
    expect(line.classList.contains("text-tool-args-fg")).toBe(true);
    expect(trigger().getAttribute("aria-label")).toBe(
      'shell，已完成，{"steps":["a"]}',
    );
  });

  it("V-J6b 空壳参数一律判为未记录，不把 {} / [] / null 当信息", async () => {
    for (const blank of ["{}", "{ }", "[]", "null"]) {
      await renderCard({ ...RUNNING, status: "done", arguments: blank });

      const line = slot("tool-line") as HTMLElement;
      expect(line.textContent).toBe("（参数未记录）");
      expect(line.textContent).not.toBe(blank);
      expect(line.classList.contains("text-muted-foreground")).toBe(true);
      // 占位不是真实内容：标签只留「名字，状态」
      expect(trigger().getAttribute("aria-label")).toBe("shell，已完成");
    }
  });

  it("V-J7/V-J8 结果区是独立气泡，有过程时也不加顶部分隔线", async () => {
    await renderChatView([VIEW_MESSAGE]);

    const result = slot("assistant-result") as HTMLElement;
    expect(result.classList.contains("bg-bubble")).toBe(true);
    expect(result.classList.contains("bg-muted")).toBe(false);
    expect(result.classList.contains("border")).toBe(true);
    expect(result.classList.contains("border-border")).toBe(true);
    expect(result.classList.contains("border-t")).toBe(false);
    expect(result.classList.contains("pt-3")).toBe(false);
    expect(result.getAttribute("data-has-process")).toBe("true");

    expect(result.classList.contains("w-fit")).toBe(true);
    expect(result.classList.contains("w-full")).toBe(false);
    expect(result.classList.contains("max-w-[80%]")).toBe(true);
    expect(result.classList.contains("rounded-2xl")).toBe(true);
    expect(result.classList.contains("rounded-bl-md")).toBe(true);
    expect(result.classList.contains("px-4")).toBe(true);
    expect(result.classList.contains("py-2")).toBe(true);
  });

  it("V-J9 无过程时同样是气泡，不带边框", async () => {
    await renderChatView([
      { role: "assistant", content: "只有正文", turnIndex: 0 },
    ]);

    const result = slot("assistant-result") as HTMLElement;
    expect(result.classList.contains("border-t")).toBe(false);
    expect(result.classList.contains("pt-3")).toBe(false);
    expect(result.getAttribute("data-has-process")).toBeNull();
  });

  it("V-J10 正文只有换行时不渲染结果区", async () => {
    await renderChatView([
      { role: "user", content: "问", turnIndex: 0 },
      { role: "assistant", content: "\n", turnIndex: 0 },
    ]);

    expect(slot("assistant-result")).toBeNull();
  });

  it("V-J11/V-J12 过程组用 1.5 档间距并在结果区之前", async () => {
    await renderChatView([VIEW_MESSAGE]);

    const process = slot("assistant-process") as HTMLElement;
    expect(process.classList.contains("gap-1.5")).toBe(true);
    expect(
      process.querySelector('[data-slot="reasoning-fold"]'),
    ).not.toBeNull();
    expect(
      process.querySelector('[data-slot="tool-call-group"]'),
    ).not.toBeNull();

    const result = slot("assistant-result") as HTMLElement;
    expect(
      process.compareDocumentPosition(result) &
        Node.DOCUMENT_POSITION_FOLLOWING,
    ).toBeTruthy();
  });

  it("V-J13 无过程时结果区是列内唯一子元素", async () => {
    await renderChatView([
      { role: "assistant", content: "只有正文", turnIndex: 0 },
    ]);

    expect(slot("assistant-process")).toBeNull();
    expect(
      [...(slot("assistant-column") as HTMLElement).children].map((el) =>
        el.getAttribute("data-slot"),
      ),
    ).toEqual(["assistant-result"]);
  });

  it("V-J14 工具名降到标签档，让参数成为行内信息主体", async () => {
    await renderCard(DONE_WITH_TEXT);

    const name = slot("tool-name") as HTMLElement;
    expect(name.classList.contains("text-muted-foreground")).toBe(true);
    expect(name.classList.contains("text-tool-args-fg")).toBe(false);
  });

  it("V-J15 展开体输出保留正文色但不加字重", async () => {
    await renderCard(DONE_WITH_TEXT);
    await expand();

    const output = section("output") as HTMLElement;
    expect(output.classList.contains("text-foreground")).toBe(true);
    expect(output.classList.contains("font-medium")).toBe(false);
    expect(
      (section("arguments") as HTMLElement).classList.contains("font-medium"),
    ).toBe(false);
  });

  it("V-J17 空白正文且无过程时不渲染任何外壳", async () => {
    await renderChatView([{ role: "assistant", content: "\n", turnIndex: 0 }]);

    expect(slot("assistant-turn")).toBeNull();
    expect(slot("assistant-column")).toBeNull();
    expect(slot("assistant-result")).toBeNull();
  });
});
