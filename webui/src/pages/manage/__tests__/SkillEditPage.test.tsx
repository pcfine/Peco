// SkillEditPage 测试 —— 面板常驻 / 空态 / 未保存标记 / Ctrl+S 提交 / 预览防抖

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { MemoryRouter, Route, Routes } from "react-router-dom";
import { SkillEditPage } from "../SkillEditPage";
import { getSkill, listSkillFiles, readSkillFile, upsertSkill } from "@/api/skills";
import { toast } from "sonner";

vi.mock("@/api/skills", () => ({
  getSkill: vi.fn(),
  listSkillFiles: vi.fn(),
  readSkillFile: vi.fn(),
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
const LIST_FILES = vi.mocked(listSkillFiles);
const READ_FILE = vi.mocked(readSkillFile);
const UPSERT = vi.mocked(upsertSkill);

const FILES = [
  { path: "scripts/run.py", size: 12, mtime: "", kind: "code" as const, writable: true },
  { path: "scripts/__pycache__/run.pyc", size: 4, mtime: "", kind: "binary" as const, writable: false },
  { path: "references/notes.md", size: 8, mtime: "", kind: "markdown" as const, writable: true },
  { path: ".env", size: 13, mtime: "", kind: "env" as const, writable: false },
];

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
  LIST_FILES.mockReset().mockResolvedValue({ files: FILES });
  READ_FILE.mockReset().mockImplementation(async (_name: string, path: string) => ({
    path,
    size: 12,
    mtime: "",
    kind: path.endsWith(".py") ? "code" : "markdown",
    content: path === "scripts/run.py" ? "print('hi')" : "# 笔记",
    truncated: false,
  }));
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

  it("切回文件后继续输入，光标与滚动不被拉回旧快照", async () => {
    containerWidth = 1700;
    await renderPage();

    const skillMd = textarea();
    skillMd.scrollTop = 240;
    skillMd.setSelectionRange(2, 2);

    const py = document.querySelector<HTMLElement>('[data-path="scripts/run.py"]');
    await act(async () => py?.click());
    await flush();
    textarea().scrollTop = 300;

    await act(async () =>
      document
        .querySelector<HTMLElement>('[data-testid="file-row-skill-md"]')
        ?.click(),
    );
    await flush();

    // 用户重新滚动、移动光标后继续输入（先设值再定位光标，避免被 value 赋值推到末尾）
    const back = textarea();
    back.scrollTop = 900;
    const setter = Object.getOwnPropertyDescriptor(
      HTMLTextAreaElement.prototype,
      "value",
    )?.set;
    setter?.call(back, "原始内容2");
    back.setSelectionRange(4, 4);
    await act(async () =>
      back.dispatchEvent(new Event("input", { bubbles: true })),
    );
    await flush();

    expect(back.scrollTop).toBe(900);
    expect(back.selectionStart).toBe(4);
  });

  it("切回资源文件时恢复滚动与光标", async () => {
    containerWidth = 1700;
    await renderPage();

    const py = () =>
      document.querySelector<HTMLElement>('[data-path="scripts/run.py"]');
    await act(async () => py()?.click());
    await flush();

    const script = textarea();
    script.scrollTop = 333;
    script.setSelectionRange(3, 3);

    await act(async () =>
      document
        .querySelector<HTMLElement>('[data-path="references/notes.md"]')
        ?.click(),
    );
    await flush();
    await act(async () => py()?.click());
    await flush();

    expect(textarea().scrollTop).toBe(333);
    expect(textarea().selectionStart).toBe(3);
  });

  it("预览往返不把源码滚动位置清成 0", async () => {
    await renderPage();
    const el = textarea();
    el.scrollTop = 500;

    await act(async () => buttonByTestId("mode-preview").click());
    // 浏览器在 display:none 下会把 scrollTop 归零
    el.scrollTop = 0;
    await act(async () => buttonByTestId("mode-source").click());

    expect(textarea()).toBe(el);
    expect(el.scrollTop).toBe(500);
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

  it("改动后顶栏出现未保存，Ctrl+S 提交一次并留在本页", async () => {
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
    // 多文件编辑下保存不应跳走，否则会打断其它缓冲。
    expect(container.textContent).not.toContain("list-page");
    expect(container.textContent).not.toContain("未保存");
  });

  it("容器够宽时常驻文件栏，含固定 SKILL.md 行与分组计数", async () => {
    containerWidth = 1700;
    await renderPage();

    const tree = document.querySelector('[data-testid="file-tree"]');
    expect(tree).not.toBeNull();
    expect(document.querySelector('[data-testid="file-tree-toggle"]')).toBeNull();
    expect(tree?.textContent).toContain("SKILL.md");
    expect(tree?.textContent).toContain("scripts/");
    expect(tree?.textContent).toContain("其他");
    expect(tree?.textContent).toContain("只读");
    // 默认展开三个可写组，其他组收起
    const rows = document.querySelectorAll('[data-testid="file-row"]');
    expect(rows.length).toBe(2);
  });

  it("容器窄于 1600 时用按钮打开目录 Sheet", async () => {
    await renderPage();
    expect(document.querySelector('[data-testid="file-tree"]')).toBeNull();

    await act(async () => buttonByTestId("file-tree-toggle").click());
    expect(
      document.querySelector('[data-testid="file-tree-sheet"]'),
    ).not.toBeNull();
  });

  it("二进制行不可打开，文本行在主编辑区打开", async () => {
    containerWidth = 1700;
    await renderPage();

    const otherGroup = document.querySelector<HTMLElement>(
      '[data-testid="file-group"][data-group="other"]',
    );
    await act(async () =>
      (otherGroup?.firstElementChild as HTMLElement | null)?.click(),
    );
    const binary = document.querySelector<HTMLElement>(
      '[data-path="scripts/__pycache__/run.pyc"]',
    );
    expect(binary?.getAttribute("aria-disabled")).toBe("true");
    await act(async () => binary?.click());
    expect(READ_FILE).not.toHaveBeenCalled();

    const md = document.querySelector<HTMLElement>(
      '[data-path="references/notes.md"]',
    );
    await act(async () => md?.click());
    await flush();
    expect(READ_FILE).toHaveBeenCalledWith("demo", "references/notes.md");
    expect(textarea().value).toBe("# 笔记");
    expect(textarea().getAttribute("aria-label")).toBe("references/notes.md 内容");
  });

  it("打开脚本时语法高亮生效，且与 SKILL.md 缓冲互不覆盖", async () => {
    containerWidth = 1700;
    await renderPage();

    // 先在 SKILL.md 留下未保存改动
    await act(async () => typeInto(textarea(), "原始内容 + 草稿"));
    expect(container.textContent).toContain("未保存");

    const py = document.querySelector<HTMLElement>('[data-path="scripts/run.py"]');
    await act(async () => py?.click());
    await flush();

    expect(textarea().value).toBe("print('hi')");
    // 高亮层是独立 <pre>，只读时才允许保存
    const pre = pane("source").querySelector("pre");
    expect(pre?.querySelector(".hljs")?.innerHTML).toContain("hljs-built_in");
    // 当前文件（脚本）不脏，但 SKILL.md 仍有草稿 —— 顶栏必须继续提示。
    expect(container.textContent).toContain("当前文件已保存");
    // 文件树上 SKILL.md 行打脏点
    expect(
      document.querySelector('[data-testid="file-row-skill-md"] [aria-label="有未保存改动"]'),
    ).not.toBeNull();

    // 切回 SKILL.md，草稿仍在
    const docRow = document.querySelector<HTMLElement>(
      '[data-testid="file-row-skill-md"]',
    );
    await act(async () => docRow?.click());
    await flush();
    expect(textarea().value).toBe("原始内容 + 草稿");
    expect(container.textContent).toContain("未保存");
  });

  it("保存脚本时带上文件、SKILL.md 用磁盘版而非草稿", async () => {
    containerWidth = 1700;
    await renderPage();

    await act(async () => typeInto(textarea(), "原始内容 + 草稿"));

    const py = document.querySelector<HTMLElement>('[data-path="scripts/run.py"]');
    await act(async () => py?.click());
    await flush();

    await act(async () => typeInto(textarea(), "print('hi')  # edited"));
    expect(container.textContent).toContain("未保存");

    await act(async () => {
      window.dispatchEvent(
        new KeyboardEvent("keydown", { key: "s", ctrlKey: true }),
      );
    });
    await flush();

    expect(UPSERT).toHaveBeenCalledWith("demo", "原始内容", [
      { path: "scripts/run.py", content: "print('hi')  # edited" },
    ]);
  });

  it("超过 256 KB 被截断时编辑器只读", async () => {
    containerWidth = 1700;
    READ_FILE.mockImplementation(async (_name: string, path: string) => ({
      path, size: 300000, mtime: "", kind: "code",
      content: "print('truncated')", truncated: true,
    }));
    await renderPage();

    const py = document.querySelector<HTMLElement>('[data-path="scripts/run.py"]');
    await act(async () => py?.click());
    await flush();

    expect(textarea().getAttribute("readonly")).not.toBeNull();
    expect(container.textContent).toContain("只读");
  });

  it("只读文件（.env）不给保存入口", async () => {
    containerWidth = 1700;
    await renderPage();

    const otherGroup = document.querySelector<HTMLElement>(
      '[data-testid="file-group"][data-group="other"]',
    );
    await act(async () =>
      (otherGroup?.firstElementChild as HTMLElement | null)?.click(),
    );
    const env = document.querySelector<HTMLElement>('[data-path=".env"]');
    await act(async () => env?.click());
    await flush();

    expect(textarea().getAttribute("readonly")).not.toBeNull();
    expect(container.textContent).toContain("只读");
    const save = document.querySelector<HTMLButtonElement>(
      '[data-testid="action-bar"] button:last-child',
    );
    expect(save?.disabled).toBe(true);
  });

  it("键盘：文件行按 ← 先回组头再折叠，→ 可重新展开", async () => {
    containerWidth = 1700;
    await renderPage();

    const docRow = document.querySelector<HTMLElement>(
      '[data-testid="file-tree"] [aria-selected="true"]',
    );
    await act(async () => docRow?.focus());
    await act(async () => {
      docRow?.dispatchEvent(
        new KeyboardEvent("keydown", { key: "ArrowDown", bubbles: true }),
      );
    });
    const group = document.querySelector<HTMLElement>(
      '[data-testid="file-group"][data-group="scripts"]',
    );
    expect(document.activeElement).toBe(group);

    // 再下一行 → 文件行
    await act(async () => {
      document.activeElement?.dispatchEvent(
        new KeyboardEvent("keydown", { key: "ArrowDown", bubbles: true }),
      );
    });
    expect(
      (document.activeElement as HTMLElement).getAttribute("data-path"),
    ).toBe("scripts/run.py");

    // ← 只回到组头，不折叠
    await act(async () => {
      document.activeElement?.dispatchEvent(
        new KeyboardEvent("keydown", { key: "ArrowLeft", bubbles: true }),
      );
    });
    expect(document.activeElement).toBe(group);
    expect(group?.getAttribute("aria-expanded")).toBe("true");

    // 再 ← 折叠
    await act(async () => {
      document.activeElement?.dispatchEvent(
        new KeyboardEvent("keydown", { key: "ArrowLeft", bubbles: true }),
      );
    });
    expect(group?.getAttribute("aria-expanded")).toBe("false");

    // → 展开
    await act(async () => {
      document.activeElement?.dispatchEvent(
        new KeyboardEvent("keydown", { key: "ArrowRight", bubbles: true }),
      );
    });
    expect(group?.getAttribute("aria-expanded")).toBe("true");
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
