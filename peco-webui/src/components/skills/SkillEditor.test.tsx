// SkillEditor —— 从老 webui SkillEditPage 移植的核心用例：
// 面板常驻 / 空态 / 未保存标记 / Ctrl+S 提交 / 资源文件缓冲 / 预览防抖。

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, render, screen, waitFor } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { SkillEditor } from "@/components/skills/SkillEditor";
import {
  getSkillMd,
  listSkillFiles,
  readSkillFile,
  saveSkill,
} from "@/api/skills";
import { type ResourceScope } from "@/lib/api";
import type { SkillFile } from "@/types/skill";

vi.mock("@/api/skills", () => ({
  getSkillMd: vi.fn(),
  listSkillFiles: vi.fn(),
  readSkillFile: vi.fn(),
  saveSkill: vi.fn(),
}));

vi.mock("sonner", () => ({
  toast: { success: vi.fn(), error: vi.fn() },
}));

const GET_SKILL = vi.mocked(getSkillMd);
const LIST_FILES = vi.mocked(listSkillFiles);
const READ_FILE = vi.mocked(readSkillFile);
const SAVE = vi.mocked(saveSkill);
/** 借道已打开空间：能力根与 `X-Peco-Space` 指向同一个团队。 */
const SCOPE: ResourceScope = { teamId: "t1", wsId: "ws_a" };

const FILES: SkillFile[] = [
  {
    path: "scripts/run.py",
    size: 12,
    mtime: "",
    kind: "code",
    writable: true,
  },
  {
    path: "scripts/__pycache__/run.pyc",
    size: 4,
    mtime: "",
    kind: "binary",
    writable: false,
  },
  {
    path: "references/notes.md",
    size: 8,
    mtime: "",
    kind: "markdown",
    writable: true,
  },
  { path: ".env", size: 13, mtime: "", kind: "env", writable: false },
];

let containerWidth = 1200;
let onBack: () => void;
let onBackMock: ReturnType<typeof vi.fn<() => void>>;

/** `vi.fn()` 的宽泛签名不能直接赋给 `() => void` 的 prop，包一层。 */
function backHandler() {
  onBackMock = vi.fn<() => void>();
  onBack = () => onBackMock();
  return onBack;
}

function renderEditor() {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  });
  return render(
    <QueryClientProvider client={client}>
      <SkillEditor scope={SCOPE} name="demo" onBack={backHandler()} />
    </QueryClientProvider>,
  );
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

async function openFile(path: string) {
  const row = document.querySelector<HTMLElement>(`[data-path="${path}"]`);
  if (!row) throw new Error(`file row not found: ${path}`);
  await act(async () => row.click());
}

beforeEach(() => {
  containerWidth = 1200;
  vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockImplementation(
    () => ({ width: containerWidth, height: 800 }) as DOMRect,
  );

  GET_SKILL.mockReset().mockResolvedValue({
    name: "demo",
    content: "原始内容",
  });
  SAVE.mockReset().mockResolvedValue({ success: true });
  LIST_FILES.mockReset().mockResolvedValue({ files: FILES });
  READ_FILE.mockReset().mockImplementation(async (_wsId, _name, path) => ({
    path,
    size: 12,
    mtime: "",
    kind: path.endsWith(".py") ? "code" : "markdown",
    content: path === "scripts/run.py" ? "print('hi')" : "# 笔记",
    truncated: false,
  }));
});

afterEach(() => {
  vi.restoreAllMocks();
});

describe("SkillEditor", () => {
  it("两个面板常驻 DOM，切换视图只改 hidden 而不卸载 textarea", async () => {
    renderEditor();
    await screen.findByTestId("action-bar");
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
    containerWidth = 1400;
    renderEditor();
    await screen.findByTestId("action-bar");

    const skillMd = textarea();
    skillMd.scrollTop = 240;
    skillMd.setSelectionRange(2, 2);

    await openFile("scripts/run.py");
    await waitFor(() => expect(textarea().value).toBe("print('hi')"));
    textarea().scrollTop = 300;

    await act(async () =>
      document
        .querySelector<HTMLElement>('[data-testid="file-row-skill-md"]')
        ?.click(),
    );
    await waitFor(() => expect(textarea().value).toBe("原始内容"));

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

    expect(textarea().scrollTop).toBe(900);
    expect(textarea().selectionStart).toBe(4);
  });

  it("切回资源文件时恢复滚动与光标", async () => {
    containerWidth = 1400;
    renderEditor();
    await screen.findByTestId("action-bar");

    await openFile("scripts/run.py");
    await waitFor(() => expect(textarea().value).toBe("print('hi')"));
    const script = textarea();
    script.scrollTop = 333;
    script.setSelectionRange(3, 3);

    await openFile("references/notes.md");
    await waitFor(() => expect(textarea().value).toBe("# 笔记"));
    await openFile("scripts/run.py");

    await waitFor(() => expect(textarea().value).toBe("print('hi')"));
    expect(textarea().scrollTop).toBe(333);
    expect(textarea().selectionStart).toBe(3);
  });

  it("预览往返不把源码滚动位置清成 0", async () => {
    renderEditor();
    await screen.findByTestId("action-bar");
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
    containerWidth = 800;
    renderEditor();
    await screen.findByTestId("action-bar");

    expect(pane("preview").hidden).toBe(true);
    expect(buttonByTestId("mode-split").disabled).toBe(true);
    expect(buttonByTestId("mode-source").getAttribute("aria-pressed")).toBe(
      "true",
    );
  });

  it("内容为空时预览给空态文案且保存按钮不可点", async () => {
    GET_SKILL.mockResolvedValue({ name: "demo", content: "" });
    renderEditor();
    await screen.findByTestId("action-bar");

    expect(document.body.textContent).toContain("还没有内容");
    const save = document.querySelector<HTMLButtonElement>(
      '[data-testid="action-bar"] button:last-child',
    );
    expect(save?.disabled).toBe(true);
  });

  it("改动后顶栏出现未保存，Ctrl+S 提交一次并留在本页", async () => {
    renderEditor();
    await screen.findByTestId("action-bar");
    expect(document.body.textContent).not.toContain("未保存");

    await act(async () => typeInto(textarea(), "原始内容 + 新增"));
    expect(document.body.textContent).toContain("未保存");

    await act(async () => {
      window.dispatchEvent(
        new KeyboardEvent("keydown", { key: "s", ctrlKey: true }),
      );
    });
    await waitFor(() => expect(SAVE).toHaveBeenCalledTimes(1));

    expect(SAVE).toHaveBeenCalledWith(SCOPE, "demo", "原始内容 + 新增");
    // 多文件编辑下保存不应跳走，否则会打断其它缓冲。
    expect(onBackMock).not.toHaveBeenCalled();
    await waitFor(() =>
      expect(document.body.textContent).not.toContain("未保存"),
    );
  });

  it("容器够宽时常驻文件栏，含固定 SKILL.md 行与分组计数", async () => {
    containerWidth = 1400;
    renderEditor();
    await screen.findByTestId("action-bar");

    const tree = document.querySelector('[data-testid="file-tree"]');
    expect(tree).not.toBeNull();
    expect(
      document.querySelector('[data-testid="file-tree-toggle"]'),
    ).toBeNull();
    expect(tree?.textContent).toContain("SKILL.md");
    expect(tree?.textContent).toContain("scripts/");
    expect(tree?.textContent).toContain("其他");
    expect(tree?.textContent).toContain("只读");
    // 默认展开三个可写组，其他组收起
    const rows = document.querySelectorAll('[data-testid="file-row"]');
    expect(rows.length).toBe(2);
  });

  it("容器窄于文件栏阈值时用按钮打开目录 Sheet", async () => {
    renderEditor();
    await screen.findByTestId("action-bar");
    expect(document.querySelector('[data-testid="file-tree"]')).toBeNull();

    await act(async () => buttonByTestId("file-tree-toggle").click());
    expect(
      document.querySelector('[data-testid="file-tree-sheet"]'),
    ).not.toBeNull();
  });

  it("二进制行不可打开，文本行在主编辑区打开", async () => {
    containerWidth = 1400;
    renderEditor();
    await screen.findByTestId("action-bar");

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

    await openFile("references/notes.md");
    await waitFor(() => expect(READ_FILE).toHaveBeenCalled());
    expect(READ_FILE).toHaveBeenCalledWith(
      SCOPE,
      "demo",
      "references/notes.md",
    );
    expect(textarea().value).toBe("# 笔记");
    expect(textarea().getAttribute("aria-label")).toBe(
      "references/notes.md 内容",
    );
  });

  it("打开脚本时语法高亮生效，且与 SKILL.md 缓冲互不覆盖", async () => {
    containerWidth = 1400;
    renderEditor();
    await screen.findByTestId("action-bar");

    // 先在 SKILL.md 留下未保存改动
    await act(async () => typeInto(textarea(), "原始内容 + 草稿"));
    expect(document.body.textContent).toContain("未保存");

    await openFile("scripts/run.py");
    await waitFor(() => expect(textarea().value).toBe("print('hi')"));

    // 高亮层是独立 <pre>
    const pre = pane("source").querySelector("pre");
    expect(pre?.querySelector(".hljs")?.innerHTML).toContain("hljs-built_in");
    // 当前文件（脚本）不脏，但 SKILL.md 仍有草稿 —— 顶栏必须继续提示。
    expect(document.body.textContent).toContain("当前文件已保存");
    // 文件树上 SKILL.md 行打脏点
    expect(
      document.querySelector(
        '[data-testid="file-row-skill-md"] [aria-label="有未保存改动"]',
      ),
    ).not.toBeNull();

    // 切回 SKILL.md，草稿仍在
    await act(async () =>
      document
        .querySelector<HTMLElement>('[data-testid="file-row-skill-md"]')
        ?.click(),
    );
    await waitFor(() => expect(textarea().value).toBe("原始内容 + 草稿"));
    expect(document.body.textContent).toContain("未保存");
  });

  it("保存脚本时带上文件、SKILL.md 用磁盘版而非草稿", async () => {
    containerWidth = 1400;
    renderEditor();
    await screen.findByTestId("action-bar");

    await act(async () => typeInto(textarea(), "原始内容 + 草稿"));

    await openFile("scripts/run.py");
    await waitFor(() => expect(textarea().value).toBe("print('hi')"));

    await act(async () => typeInto(textarea(), "print('hi')  # edited"));
    expect(document.body.textContent).toContain("未保存");

    await act(async () => {
      window.dispatchEvent(
        new KeyboardEvent("keydown", { key: "s", ctrlKey: true }),
      );
    });
    await waitFor(() => expect(SAVE).toHaveBeenCalledTimes(1));

    expect(SAVE).toHaveBeenCalledWith(SCOPE, "demo", "原始内容", [
      { path: "scripts/run.py", content: "print('hi')  # edited" },
    ]);
  });

  it("超过 256 KB 被截断时编辑器只读", async () => {
    containerWidth = 1400;
    READ_FILE.mockImplementation(async (_wsId, _name, path) => ({
      path,
      size: 300000,
      mtime: "",
      kind: "code",
      content: "print('truncated')",
      truncated: true,
    }));
    renderEditor();
    await screen.findByTestId("action-bar");

    await openFile("scripts/run.py");
    await waitFor(() => expect(textarea().value).toBe("print('truncated')"));

    expect(textarea().getAttribute("readonly")).not.toBeNull();
    expect(document.body.textContent).toContain("只读");
  });

  it("只读文件（.env）不给保存入口", async () => {
    containerWidth = 1400;
    renderEditor();
    await screen.findByTestId("action-bar");

    const otherGroup = document.querySelector<HTMLElement>(
      '[data-testid="file-group"][data-group="other"]',
    );
    await act(async () =>
      (otherGroup?.firstElementChild as HTMLElement | null)?.click(),
    );
    await openFile(".env");
    await waitFor(() => expect(textarea().value).toBe("# 笔记"));

    expect(textarea().getAttribute("readonly")).not.toBeNull();
    expect(document.body.textContent).toContain("只读");
    const save = document.querySelector<HTMLButtonElement>(
      '[data-testid="action-bar"] button:last-child',
    );
    expect(save?.disabled).toBe(true);
  });

  it("键盘：文件行按 ← 先回组头再折叠，→ 可重新展开", async () => {
    containerWidth = 1400;
    renderEditor();
    await screen.findByTestId("action-bar");

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
    renderEditor();
    await screen.findByTestId("action-bar");
    expect(pane("preview").textContent).toContain("原始内容");

    await act(async () => typeInto(textarea(), "原始内容 与 新增片段"));
    expect(pane("preview").textContent).toContain("原始内容");
    expect(pane("preview").textContent).not.toContain("新增片段");

    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 320));
    });
    expect(pane("preview").textContent).toContain("新增片段");
  });

  it("干净状态点返回直接回列表，不弹确认", async () => {
    renderEditor();
    await screen.findByTestId("action-bar");

    await act(async () =>
      screen.getByRole("button", { name: "返回列表" }).click(),
    );
    expect(onBackMock).toHaveBeenCalledTimes(1);
  });
});
