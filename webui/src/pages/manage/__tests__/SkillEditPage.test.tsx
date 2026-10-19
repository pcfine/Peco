// SkillEditPage 测试 —— 面板常驻 / 空态 / 未保存标记 / Ctrl+S 提交 / 预览防抖

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { MemoryRouter, Route, Routes } from "react-router-dom";
import { SkillEditPage } from "../SkillEditPage";
import { getSkill, upsertSkill } from "@/api/skills";
import { toast } from "sonner";

vi.mock("@/api/skills", () => ({
  getSkill: vi.fn(),
  upsertSkill: vi.fn(),
  listSkills: vi.fn(),
  deleteSkill: vi.fn(),
  importSkill: vi.fn(),
  exportSkill: vi.fn(),
}));

vi.mock("sonner", () => ({
  toast: { success: vi.fn(), error: vi.fn() },
}));

// jsdom 未实现 ResizeObserver；容器宽也恒为 0，需要显式给出宽度档位。
class ResizeObserverStub {
  observe() {}
  unobserve() {}
  disconnect() {}
}

let containerWidth = 1200;

const GET_SKILL = vi.mocked(getSkill);
const UPSERT = vi.mocked(upsertSkill);

let container: HTMLDivElement;
let root: Root;

async function flush() {
  await act(async () => {
    await new Promise((resolve) => setTimeout(resolve, 0));
  });
}

async function renderPage() {
  await act(async () => {
    root.render(
      <MemoryRouter initialEntries={["/workspace/skills/demo/edit"]}>
        <Routes>
          <Route
            path="/workspace/skills/:skillName/edit"
            element={<SkillEditPage />}
          />
          <Route path="/workspace/skills" element={<div>list-page</div>} />
        </Routes>
      </MemoryRouter>,
    );
  });
  await flush();
}

function pane(name: "source" | "preview"): HTMLElement {
  const el = document.querySelector<HTMLElement>(`[data-pane="${name}"]`);
  if (!el) throw new Error(`pane not found: ${name}`);
  return el;
}

function textarea(): HTMLTextAreaElement {
  const el = pane("source").querySelector("textarea");
  if (!el) throw new Error("textarea not found");
  return el;
}

/** 绕过 React 的 value 追踪，触发一次真实的 onChange。 */
function typeInto(el: HTMLTextAreaElement, value: string) {
  const setter = Object.getOwnPropertyDescriptor(
    HTMLTextAreaElement.prototype,
    "value",
  )?.set;
  setter?.call(el, value);
  el.dispatchEvent(new Event("input", { bubbles: true }));
}

function buttonByTestId(testId: string): HTMLButtonElement {
  const el = document.querySelector<HTMLButtonElement>(
    `[data-testid="${testId}"]`,
  );
  if (!el) throw new Error(`button not found: ${testId}`);
  return el;
}

beforeEach(() => {
  (
    globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT: boolean }
  ).IS_REACT_ACT_ENVIRONMENT = true;
  globalThis.ResizeObserver ??=
    ResizeObserverStub as unknown as typeof ResizeObserver;
  containerWidth = 1200;
  vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockImplementation(
    () => ({ width: containerWidth, height: 800 }) as DOMRect,
  );

  GET_SKILL.mockReset().mockResolvedValue({
    name: "demo",
    content: "原始内容",
  });
  UPSERT.mockReset().mockResolvedValue({ success: true });
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

describe("Skill 编辑页", () => {
  it("两个面板常驻 DOM，切换视图只改 hidden 而不卸载 textarea", async () => {
    await renderPage();
    const before = textarea();

    expect(pane("source").hidden).toBe(false);
    expect(pane("preview").hidden).toBe(false);
    expect(buttonByTestId("mode-split").getAttribute("aria-pressed")).toBe(
      "true",
    );

    await act(async () => buttonByTestId("mode-preview").click());
    expect(pane("source").hidden).toBe(true);
    expect(pane("preview").hidden).toBe(false);
    expect(textarea()).toBe(before);

    await act(async () => buttonByTestId("mode-source").click());
    expect(pane("preview").hidden).toBe(true);
    expect(textarea()).toBe(before);
  });

  it("容器窄于双栏下限时隐藏预览面板并禁用分栏按钮", async () => {
    containerWidth = 900;
    await renderPage();

    expect(pane("preview").hidden).toBe(true);
    expect(buttonByTestId("mode-split").disabled).toBe(true);
    expect(buttonByTestId("mode-source").getAttribute("aria-pressed")).toBe(
      "true",
    );
  });

  it("内容为空时预览给空态文案且保存按钮不可点", async () => {
    GET_SKILL.mockResolvedValue({ name: "demo", content: "" });
    await renderPage();

    expect(container.textContent).toContain("还没有内容");
    const save = document.querySelector<HTMLButtonElement>(
      '[data-testid="action-bar"] button:last-child',
    );
    expect(save?.disabled).toBe(true);
  });

  it("改动后顶栏出现未保存，Ctrl+S 提交一次并跳转列表页", async () => {
    await renderPage();
    expect(container.textContent).not.toContain("未保存");

    await act(async () => typeInto(textarea(), "原始内容 + 新增"));
    expect(container.textContent).toContain("未保存");

    await act(async () => {
      window.dispatchEvent(
        new KeyboardEvent("keydown", { key: "s", ctrlKey: true }),
      );
    });
    await flush();

    expect(UPSERT).toHaveBeenCalledTimes(1);
    expect(UPSERT).toHaveBeenCalledWith("demo", "原始内容 + 新增");
    expect(container.textContent).toContain("list-page");
  });

  it("预览按 250ms 防抖刷新，输入期间保留上一帧", async () => {
    await renderPage();
    expect(pane("preview").textContent).toContain("原始内容");

    await act(async () => typeInto(textarea(), "原始内容 与 新增片段"));
    expect(pane("preview").textContent).toContain("原始内容");
    expect(pane("preview").textContent).not.toContain("新增片段");

    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 320));
    });
    expect(pane("preview").textContent).toContain("新增片段");
  });
});
