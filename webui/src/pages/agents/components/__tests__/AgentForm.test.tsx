// AgentForm 测试 —— 顶栏动作组 / chip 只留名字 / 已选计数 / Ctrl+S 提交

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { AgentForm } from "../AgentForm";
import { listSkills } from "@/api/skills";
import { listKnowledgeBases } from "@/api/knowledge";
import { getMcpConfig } from "@/api/mcp";
import { toast } from "sonner";

vi.mock("@/api/skills", () => ({ listSkills: vi.fn() }));
vi.mock("@/api/knowledge", () => ({ listKnowledgeBases: vi.fn() }));
vi.mock("@/api/mcp", () => ({ getMcpConfig: vi.fn() }));
vi.mock("@/api/client", () => ({ default: { post: vi.fn() } }));
vi.mock("sonner", () => ({
  toast: { success: vi.fn(), error: vi.fn() },
}));

// jsdom 未实现 ResizeObserver，radix 的 Select / Slider 直接引用它。
class ResizeObserverStub {
  observe() {}
  unobserve() {}
  disconnect() {}
}

const LIST_SKILLS = vi.mocked(listSkills);
const LIST_KBS = vi.mocked(listKnowledgeBases);
const GET_MCP = vi.mocked(getMcpConfig);

let container: HTMLDivElement;
let root: Root;

async function flush() {
  await act(async () => {
    await new Promise((resolve) => setTimeout(resolve, 0));
  });
}

async function renderForm(onSubmit = vi.fn().mockResolvedValue(undefined)) {
  await act(async () => {
    root.render(<AgentForm title="编辑 Agent" onSubmit={onSubmit} />);
  });
  await flush();
  await flush();
  return onSubmit;
}

function chips(): HTMLElement[] {
  return Array.from(document.querySelectorAll<HTMLElement>("[data-chip]"));
}

/** 绕过 React 的 value 追踪，触发一次真实的 onChange。 */
function typeInto(el: HTMLInputElement, value: string) {
  const setter = Object.getOwnPropertyDescriptor(
    HTMLInputElement.prototype,
    "value",
  )?.set;
  setter?.call(el, value);
  el.dispatchEvent(new Event("input", { bubbles: true }));
}

function submitButton(): HTMLButtonElement {
  const el = document.querySelector<HTMLButtonElement>(
    '[data-testid="action-bar"] button[type="submit"]',
  );
  if (!el) throw new Error("submit button not found");
  return el;
}

beforeEach(() => {
  (
    globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT: boolean }
  ).IS_REACT_ACT_ENVIRONMENT = true;
  globalThis.ResizeObserver ??=
    ResizeObserverStub as unknown as typeof ResizeObserver;

  LIST_SKILLS.mockReset().mockResolvedValue([
    {
      name: "frontend-design",
      description: "前端设计与实现规范，含布局、间距、色彩与可访问性基线。",
    },
  ]);
  LIST_KBS.mockReset().mockResolvedValue([]);
  GET_MCP.mockReset().mockResolvedValue({ mcpServers: {} });
  vi.mocked(toast.success).mockReset();
  vi.mocked(toast.error).mockReset();

  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
});

afterEach(() => {
  act(() => root.unmount());
  container.remove();
  vi.restoreAllMocks();
});

describe("Agent 编辑表单", () => {
  it("提交按钮落在顶栏动作组内，点击后提交一份完整载荷", async () => {
    const onSubmit = await renderForm();
    const nameInput = document.getElementById("name") as HTMLInputElement;
    await act(async () => typeInto(nameInput, "设计助手"));

    await act(async () => submitButton().click());
    await flush();

    expect(onSubmit).toHaveBeenCalledTimes(1);
    const payload = onSubmit.mock.calls[0][0] as { name: string };
    expect(payload.name).toBe("设计助手");
  });

  it("chip 只渲染名字，描述退到 title 属性", async () => {
    await renderForm();
    const skill = chips().find(
      (c) => c.textContent?.trim() === "frontend-design",
    );
    expect(skill).toBeDefined();
    expect(skill?.getAttribute("title")).toContain("前端设计与实现规范");
    expect(skill?.textContent?.trim()).toBe("frontend-design");
  });

  it("能力区带 data-section 标记并显示已选计数，勾选后计数跟着变", async () => {
    await renderForm();
    const section = document.querySelector('[data-section="capabilities"]');
    expect(section).not.toBeNull();
    expect(section?.textContent).toContain("已选 0/1");

    const skill = chips().find(
      (c) => c.textContent?.trim() === "frontend-design",
    );
    const checkbox = skill?.querySelector(
      'input[type="checkbox"]',
    ) as HTMLInputElement;
    await act(async () => checkbox.click());

    expect(section?.textContent).toContain("已选 1/1");
  });

  it("系统提示词面与检查器并列渲染，textarea 不写 rows", async () => {
    await renderForm();
    const canvas = document.querySelector('[data-testid="agent-canvas"]');
    const rail = document.querySelector('[data-testid="agent-rail"]');
    expect(canvas).not.toBeNull();
    expect(rail).not.toBeNull();

    const prompt = canvas?.querySelector("textarea");
    expect(prompt).not.toBeNull();
    expect(prompt?.hasAttribute("rows")).toBe(false);
    expect(prompt?.getAttribute("aria-label")).toBe("系统提示词");
  });

  it("改动字段后顶栏出现未保存，Ctrl+S 触发一次提交", async () => {
    const onSubmit = await renderForm();
    const nameInput = document.getElementById("name") as HTMLInputElement;

    await act(async () => typeInto(nameInput, "改过的名字"));
    expect(container.textContent).toContain("未保存");

    await act(async () => {
      window.dispatchEvent(
        new KeyboardEvent("keydown", { key: "s", metaKey: true }),
      );
    });
    await flush();

    expect(onSubmit).toHaveBeenCalledTimes(1);
  });
});
