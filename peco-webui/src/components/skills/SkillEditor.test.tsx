// SkillEditor —— 从老 webui SkillEditPage 移植的核心用例：
// 面板常驻 / 空态 / 未保存标记 / Ctrl+S 提交 / 资源文件缓冲 / 预览防抖。

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, render, screen, waitFor } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { previewBounds, SkillEditor } from "@/components/skills/SkillEditor";
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

/**
 * 让外层容器与分栏行量出不同宽度：真实页面上常驻文件栏只占外层、不在分栏行里。
 * `this` 指向被测量的元素，所以只能用普通函数。
 */
function mockWidths(container: number, splitRow: number) {
  vi.mocked(HTMLElement.prototype.getBoundingClientRect).mockImplementation(
    function (this: HTMLElement) {
      const width =
        this.getAttribute("data-testid") === "split-row" ? splitRow : container;
      return { width, height: 800 } as DOMRect;
    },
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

describe("previewBounds", () => {
  it("1920 页面分栏行 1296：上限 870、默认仍是 560", () => {
    expect(previewBounds(1296)).toEqual({
      min: 320,
      max: 870,
      defaultWidth: 560,
    });
  });

  it("分栏行 1000：上限 574、默认 450", () => {
    expect(previewBounds(1000)).toEqual({
      min: 320,
      max: 574,
      defaultWidth: 450,
    });
  });

  it("极窄分栏行的上限不低于下限", () => {
    // 700 - 420 - 6 = 274 已经低于下限；上限必须兜到 320，否则 aria 的下限大于上限。
    expect(previewBounds(700)).toEqual({
      min: 320,
      max: 320,
      defaultWidth: 320,
    });
  });
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

  describe("预览里的 frontmatter", () => {
    /** 与真实 SKILL.md 同形：属性段在前，正文另起。 */
    const WITH_FRONTMATTER = [
      "---",
      "name: db-migration",
      "description: SQLite 迁移与数据完整性",
      'allowed-tools: ["shell"]',
      "---",
      "",
      "# SQLite 迁移铁律",
      "",
      "先备份再迁移。",
      "",
    ].join("\n");

    it("frontmatter 不再被渲染成分隔线与含 name: 的二级标题", async () => {
      GET_SKILL.mockResolvedValue({
        name: "demo",
        content: WITH_FRONTMATTER,
      });
      renderEditor();
      await screen.findByTestId("action-bar");

      const preview = pane("preview");
      expect(preview.querySelector("hr")).toBeNull();
      const titles = Array.from(preview.querySelectorAll("h2"));
      expect(titles.some((el) => el.textContent?.includes("name:"))).toBe(
        false,
      );
    });

    it("元信息块原样展示属性，正文照常渲染", async () => {
      GET_SKILL.mockResolvedValue({
        name: "demo",
        content: WITH_FRONTMATTER,
      });
      renderEditor();
      await screen.findByTestId("action-bar");

      const block = document.querySelector<HTMLElement>(
        '[data-testid="frontmatter-block"]',
      );
      expect(block).not.toBeNull();
      expect(pane("preview").contains(block!)).toBe(true);
      expect(block?.textContent).toContain("name:");
      expect(block?.textContent).toContain("allowed-tools:");

      expect(pane("preview").querySelector("h1")?.textContent).toContain(
        "迁移铁律",
      );
    });

    it("正文自带的 --- 仍渲染成人分隔线", async () => {
      GET_SKILL.mockResolvedValue({
        name: "demo",
        content: "# 标题\n\n第一段\n\n---\n\n第二段\n",
      });
      renderEditor();
      await screen.findByTestId("action-bar");

      expect(pane("preview").querySelector("hr")).not.toBeNull();
      expect(
        document.querySelector('[data-testid="frontmatter-block"]'),
      ).toBeNull();
    });

    it("只有元信息、没有正文时不显示空态", async () => {
      GET_SKILL.mockResolvedValue({
        name: "demo",
        content: "---\nname: x\n---\n",
      });
      renderEditor();
      await screen.findByTestId("action-bar");

      expect(
        document.querySelector('[data-testid="frontmatter-block"]'),
      ).not.toBeNull();
      expect(document.body.textContent).not.toContain("还没有内容");
    });
  });

  it("分栏按容器宽给预览定宽，独占预览铺满剩余宽度", async () => {
    renderEditor();
    await screen.findByTestId("action-bar");

    // 1200 容器：round(1200 * 0.45) = 540，仍落在默认区间的 380..560 内
    expect(pane("preview").style.width).toBe("540px");
    expect(pane("preview").className).toContain("shrink-0");

    await act(async () => buttonByTestId("mode-preview").click());

    expect(pane("preview").style.width).toBe("");
    expect(pane("preview").className).toContain("flex-1");
    expect(pane("preview").firstElementChild?.className).toContain(
      "max-w-[760px]",
    );
  });

  describe("分栏分隔线", () => {
    /** 上限 = 容器 1200 - 源码下限 420 - 分隔线热区 6。 */
    const MAX_WIDTH = 1200 - 420 - 6;

    function separator(): HTMLElement {
      const el = document.querySelector<HTMLElement>('[role="separator"]');
      if (!el) throw new Error("separator not found");
      return el;
    }

    async function press(key: string) {
      await act(async () => {
        separator().dispatchEvent(
          new KeyboardEvent("keydown", { key, bubbles: true }),
        );
      });
    }

    /** jsdom 没有 PointerEvent，用同一组 window 监听器接 MouseEvent。 */
    function pointer(type: string, clientX: number) {
      return new MouseEvent(type, {
        clientX,
        bubbles: true,
        cancelable: true,
      });
    }

    it("暴露 separator 语义与当前宽度", async () => {
      renderEditor();
      await screen.findByTestId("action-bar");

      const handle = separator();
      expect(handle.getAttribute("aria-orientation")).toBe("vertical");
      expect(handle.tabIndex).toBe(0);
      expect(handle.getAttribute("aria-label")).toBe("调整源码与预览宽度");
      expect(handle.getAttribute("aria-valuenow")).toBe("540");
      expect(handle.getAttribute("aria-valuemin")).toBe("320");
      expect(handle.getAttribute("aria-valuemax")).toBe(String(MAX_WIDTH));
      expect(handle.getAttribute("aria-valuetext")).toBe("预览栏 540px");
    });

    it("上下限量的是分栏行，不是含常驻文件栏的外层容器", async () => {
      // 1920 页面实测：外层 1536，文件栏吃掉 240，分栏行只有 1296。
      mockWidths(1536, 1296);
      renderEditor();
      await screen.findByTestId("action-bar");

      const bounds = previewBounds(1296);
      expect(separator().getAttribute("aria-valuemax")).toBe(
        String(bounds.max),
      );
      expect(separator().getAttribute("aria-valuemax")).not.toBe(
        String(previewBounds(1536).max),
      );
      // 拖到最右时预览停在 870，源码栏仍留够 420。
      expect(pane("preview").style.width).toBe(`${bounds.defaultWidth}px`);
      expect(bounds.defaultWidth).toBe(560);
    });

    it("分栏是否可用仍看外层容器宽，窄分栏行不放宽预览上限", async () => {
      // 分栏行被文件栏压到 700 时上限只剩 320，但「分栏」本身仍可用（容器 ≥860）。
      mockWidths(1200, 700);
      renderEditor();
      await screen.findByTestId("action-bar");

      expect(buttonByTestId("mode-split").disabled).toBe(false);
      expect(pane("preview").hidden).toBe(false);
      expect(separator().getAttribute("aria-valuemax")).toBe("320");
      expect(pane("preview").style.width).toBe("320px");
    });

    it("热区用伪元素向两侧扩到 30px，布局占位与视觉线不变", async () => {
      renderEditor();
      await screen.findByTestId("action-bar");

      const handle = separator();
      // jsdom 不算伪元素样式与命中测试，这里只能锁类名契约；实际命中范围在浏览器
      // 里用 elementFromPoint 量。w-1.5 = 6px 占位，伪元素各 12px 是热区。
      expect(handle.className).toContain("w-1.5");
      expect(handle.className).toContain("after:absolute");
      expect(handle.className).toContain("after:inset-y-0");
      expect(handle.className).toContain("after:-left-3");
      expect(handle.className).toContain("after:-right-3");
      expect(handle.className).toContain("after:content-['']");
    });

    it("键盘焦点走全站统一 outline，只额外染线", async () => {
      renderEditor();
      await screen.findByTestId("action-bar");

      const handle = separator();
      expect(handle.className).not.toContain("focus-visible:outline-none");
      expect(handle.firstElementChild?.className).toContain(
        "group-focus-visible:bg-primary",
      );
    });

    it("键盘 ←/→ 一次 16px，越界停在上下限，Home/End 跳到两端", async () => {
      renderEditor();
      await screen.findByTestId("action-bar");

      await press("ArrowLeft");
      expect(pane("preview").style.width).toBe("556px");
      await press("ArrowRight");
      expect(pane("preview").style.width).toBe("540px");

      for (let i = 0; i < 20; i += 1) await press("ArrowRight");
      expect(pane("preview").style.width).toBe("320px");
      expect(separator().getAttribute("aria-valuenow")).toBe("320");

      await press("End");
      expect(pane("preview").style.width).toBe(`${MAX_WIDTH}px`);
      await press("Home");
      expect(pane("preview").style.width).toBe("320px");
    });

    it("双击复位到默认宽度", async () => {
      renderEditor();
      await screen.findByTestId("action-bar");

      await press("End");
      expect(pane("preview").style.width).toBe(`${MAX_WIDTH}px`);

      await act(async () => {
        separator().dispatchEvent(
          new MouseEvent("dblclick", { bubbles: true }),
        );
      });
      expect(pane("preview").style.width).toBe("540px");
    });

    it("拖拽跟随指针，松手后停止跟随", async () => {
      renderEditor();
      await screen.findByTestId("action-bar");

      await act(async () => {
        separator().dispatchEvent(pointer("pointerdown", 900));
      });
      // 拖动时指针扫过正文不该顺手选中文字
      expect(document.body.style.userSelect).toBe("none");

      // 指针左移 40px → 预览加宽 40px
      await act(async () => {
        window.dispatchEvent(pointer("pointermove", 860));
      });
      expect(pane("preview").style.width).toBe("580px");

      // 一路拖到容器最左也只给到上限，不会把源码栏挤没
      await act(async () => {
        window.dispatchEvent(pointer("pointermove", 0));
      });
      expect(pane("preview").style.width).toBe(`${MAX_WIDTH}px`);

      await act(async () => {
        window.dispatchEvent(pointer("pointerup", 0));
      });
      expect(document.body.style.userSelect).toBe("");

      await act(async () => {
        window.dispatchEvent(pointer("pointermove", 300));
      });
      expect(pane("preview").style.width).toBe(`${MAX_WIDTH}px`);
    });
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
