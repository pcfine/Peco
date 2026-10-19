// AgentForm 测试 —— 双栏等宽 / 能力项只在弹窗里出全量 / 已选 chip 增删 / 提交

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

type FormDefaults = Parameters<typeof AgentForm>[0]["defaultValues"];

async function renderForm(
  onSubmit = vi.fn().mockResolvedValue(undefined),
  defaultValues?: FormDefaults,
) {
  await act(async () => {
    root.render(
      <AgentForm
        title="编辑 Agent"
        onSubmit={onSubmit}
        defaultValues={defaultValues}
      />,
    );
  });
  await flush();
  await flush();
  return onSubmit;
}

function chips(picker?: string): HTMLElement[] {
  const scope = picker
    ? document.querySelector<HTMLElement>(`[data-picker="${picker}"]`)
    : document;
  return Array.from(scope!.querySelectorAll<HTMLElement>("[data-chip]"));
}

function picker(title: string): HTMLElement {
  const el = document.querySelector<HTMLElement>(`[data-picker="${title}"]`);
  if (!el) throw new Error(`picker ${title} not found`);
  return el;
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
    { name: "code-review", description: "评审代码改动" },
  ]);
  LIST_KBS.mockReset().mockResolvedValue([]);
  GET_MCP.mockReset().mockResolvedValue({
    mcpServers: { filesystem: { command: "npx", transport: "stdio" } },
  });
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

  it("身份区只留头像：点头像选文件，骰子换一张随机像素图", async () => {
    const onSubmit = await renderForm();

    // 旧版独立的「图标」「背景」两行已删除，不再有文本输入。
    expect(document.getElementById("icon")).toBeNull();
    expect(document.getElementById("bgColor")).toBeNull();

    const avatar = document.querySelector<HTMLButtonElement>(
      '[data-testid="icon-upload"]',
    )!;
    const fileInput = container.querySelector<HTMLInputElement>(
      'input[type="file"]',
    )!;
    const clickSpy = vi.spyOn(fileInput, "click");
    await act(async () => avatar.click());
    expect(clickSpy).toHaveBeenCalledTimes(1);

    const before = avatar.querySelector("svg")?.outerHTML ?? "";
    expect(before).not.toBe("");

    const shuffle = document.querySelector<HTMLButtonElement>(
      '[data-testid="icon-shuffle"]',
    )!;
    await act(async () => shuffle.click());
    expect(avatar.querySelector("svg")?.outerHTML).not.toBe(before);
    // 换图标必须点亮「未保存」，否则用户以为没改动。
    expect(
      document.querySelector('[data-testid="action-bar"]')?.textContent,
    ).toContain("未保存");

    await act(async () =>
      typeInto(document.getElementById("name") as HTMLInputElement, "像素助手"),
    );
    await act(async () => submitButton().click());
    await flush();
    const payload = onSubmit.mock.calls[0][0] as { icon: string };
    expect(payload.icon).toMatch(/^pixel:[0-9a-f]{8}$/);
  });

  it("配置与提示词两块并列渲染，提示词 textarea 不写 rows", async () => {
    await renderForm();
    const config = document.querySelector('[data-testid="agent-config"]');
    const prompt = document.querySelector('[data-testid="agent-prompt"]');
    expect(config).not.toBeNull();
    expect(prompt).not.toBeNull();

    const textarea = prompt?.querySelector("textarea");
    expect(textarea).not.toBeNull();
    expect(textarea?.hasAttribute("rows")).toBe(false);
    expect(textarea?.getAttribute("aria-label")).toBe("提示词");
  });

  it("身份区：名称与描述都用「标签 + 控件」同行结构，描述占两行", async () => {
    await renderForm();
    const nameLabel = document.querySelector('label[for="name"]');
    const nameInput = document.getElementById("name");
    expect(nameLabel?.parentElement).toBe(nameInput?.parentElement);

    const descLabel = document.querySelector('label[for="desc"]');
    const desc = document.getElementById("desc");
    expect(descLabel?.parentElement).toBe(desc?.parentElement);
    expect(desc?.tagName).toBe("TEXTAREA");
    expect(desc?.getAttribute("rows")).toBe("2");

    // 两个标签同宽，控件左边界才能对齐。
    for (const label of [nameLabel, descLabel]) {
      expect(label?.classList.contains("w-10")).toBe(true);
      expect(label?.classList.contains("shrink-0")).toBe(true);
    }
  });

  it("能力区标题为 Tools / Skill / KnowledgeBase / MCP，模型属性用 Model", async () => {
    await renderForm();
    expect(
      Array.from(document.querySelectorAll("[data-picker]")).map(
        (el) => (el as HTMLElement).dataset.picker,
      ),
    ).toEqual(["Tools", "Skill", "KnowledgeBase", "MCP"]);

    expect(
      Array.from(document.querySelectorAll("label")).map((l) =>
        l.textContent?.trim(),
      ),
    ).toContain("Model");
  });

  it("Provider 排在 Model 之前，先选 provider 再选模型", async () => {
    await renderForm();
    const labels = Array.from(document.querySelectorAll("label")).map((l) =>
      l.textContent?.trim(),
    );
    expect(labels.indexOf("Provider")).toBeGreaterThan(-1);
    expect(labels.indexOf("Provider")).toBeLessThan(labels.indexOf("Model"));
  });

  it("旧别名归一为规范模型名：显示与保存都写 deepseek-flash", async () => {
    const onSubmit = await renderForm(undefined, {
      name: "旧 Agent",
      model: "deepseek-v4-flash",
      provider: "deepseek",
    } as FormDefaults);

    const modelRow = Array.from(document.querySelectorAll("label"))
      .find((l) => l.textContent?.trim() === "Model")!
      .parentElement!;
    expect(modelRow.textContent).toContain("DeepSeek V4.1 Flash");

    await act(async () => submitButton().click());
    await flush();
    const payload = onSubmit.mock.calls[0][0] as { model: string };
    expect(payload.model).toBe("deepseek-flash");
  });

  it("能力区只渲染已选项，全量候选留在弹窗里", async () => {
    await renderForm();
    const skill = picker("Skill");
    expect(skill.textContent).toContain("已选 0");
    expect(skill.textContent).not.toContain("frontend-design");
    expect(chips("Skill")).toHaveLength(0);

    const add = skill.querySelector<HTMLButtonElement>(
      'button[aria-label="添加 Skill"]',
    );
    await act(async () => add!.click());
    await flush();

    const dialog = document.querySelector('[role="dialog"]');
    expect(dialog).not.toBeNull();
    expect(dialog?.textContent).toContain("frontend-design");
    expect(dialog?.textContent).toContain("code-review");
  });

  it("弹窗公选后写入已选，chip 可再移除", async () => {
    await renderForm();
    const skill = picker("Skill");

    await act(async () =>
      skill
        .querySelector<HTMLButtonElement>('button[aria-label="添加 Skill"]')!
        .click(),
    );
    await flush();

    const option = document.querySelector<HTMLInputElement>(
      '[data-option="frontend-design"] input[type="checkbox"]',
    );
    await act(async () => option!.click());

    const confirm = Array.from(
      document.querySelectorAll<HTMLButtonElement>('[role="dialog"] button'),
    ).find((b) => b.textContent?.trim() === "确定");
    await act(async () => confirm!.click());
    await flush();

    expect(chips("Skill").map((c) => c.textContent?.trim())).toEqual([
      "frontend-design",
    ]);
    expect(picker("Skill").textContent).toContain("已选 1");

    const remove = chips("Skill")[0].querySelector<HTMLButtonElement>(
      'button[aria-label="移除 frontend-design"]',
    );
    await act(async () => remove!.click());

    expect(chips("Skill")).toHaveLength(0);
    expect(picker("Skill").textContent).toContain("已选 0");
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
