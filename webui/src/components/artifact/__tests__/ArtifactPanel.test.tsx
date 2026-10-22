// 产物面板：空态 / 首载态 / 四类卡片 / 降级规则 / 分类过滤
//
// jsdom 缺 IntersectionObserver 与 ResizeObserver（radix ScrollArea 挂载即构造），
// 且只 stub `URL.createObjectURL` / `URL.revokeObjectURL` 两个静态方法 —— 取流 URL
// 构造内部用 `new URL(path, origin)`，整替 `URL` 会把构造器一起换掉。

import {
  afterEach,
  beforeAll,
  beforeEach,
  describe,
  expect,
  it,
  vi,
} from "vitest";
import { act, createRef, useState } from "react";
import type { RefObject } from "react";
import { createRoot, type Root } from "react-dom/client";
import { ArtifactPanel } from "../ArtifactPanel";
import { ArtifactPreviewDialog } from "../ArtifactPreviewDialog";
import api from "@/api/client";
import { __testCacheStats, releaseAll } from "@/api/artifactBlob";
import { listPecoArtifacts } from "@/api/peco";
import { usePecoArtifactStore } from "@/stores/pecoArtifactStore";
import type { ArtifactItem } from "@/types/artifact";

vi.mock("@/api/peco", () => ({ listPecoArtifacts: vi.fn() }));
vi.mock("sonner", () => ({ toast: { error: vi.fn(), success: vi.fn() } }));

const MiB = 1024 * 1024;

class ResizeObserverStub {
  observe() {}
  unobserve() {}
  disconnect() {}
}

/** 保存回调与观察目标，让用例显式触发"进入视口"，不依赖真实滚动。 */
class IntersectionObserverStub {
  static instances: IntersectionObserverStub[] = [];
  readonly root: Element | null = null;
  readonly rootMargin = "200px";
  readonly thresholds: number[] = [];
  private targets = new Set<Element>();

  constructor(private readonly callback: IntersectionObserverCallback) {
    IntersectionObserverStub.instances.push(this);
  }

  observe(target: Element) {
    this.targets.add(target);
  }

  unobserve(target: Element) {
    this.targets.delete(target);
  }

  disconnect() {
    this.targets.clear();
  }

  takeRecords(): IntersectionObserverEntry[] {
    return [];
  }

  emit(isIntersecting: boolean) {
    const entries = [...this.targets].map(
      (target) => ({ target, isIntersecting }) as IntersectionObserverEntry,
    );
    if (entries.length === 0) return;
    this.callback(entries, this as unknown as IntersectionObserver);
  }
}

let urlSeq = 0;

function item(overrides: Partial<ArtifactItem> = {}): ArtifactItem {
  urlSeq += 1;
  return {
    id: `a${urlSeq}`,
    conversation_id: "c1",
    kind: "image",
    mime: "image/png",
    title: `图${urlSeq}.png`,
    size_bytes: 1024,
    url: `/api/artifacts/a${urlSeq}/content`,
    download_url: `/api/artifacts/a${urlSeq}/download`,
    created_at: "2026-10-20 12:00:00",
    available: true,
    ...overrides,
  };
}

const getSpy = vi.spyOn(api, "get");
const listArtifacts = vi.mocked(listPecoArtifacts);

let container: HTMLDivElement;
let root: Root;

async function flush() {
  await act(async () => {
    await new Promise((resolve) => setTimeout(resolve, 0));
    await new Promise((resolve) => setTimeout(resolve, 0));
  });
}

async function render() {
  await act(async () => {
    root.render(<ArtifactPanel />);
  });
  await flush();
}

/** 卸载面板：抽屉关闭即卸载，用来验证状态是否活在组件之外。 */
async function unmountPanel() {
  await act(async () => {
    root.render(null);
  });
  await flush();
}

/** 让所有卡片同时进入视口（图片卡据此发起取流）。 */
async function enterViewport() {
  await act(async () => {
    for (const observer of IntersectionObserverStub.instances) {
      observer.emit(true);
    }
  });
  await flush();
}

function seed(items: ArtifactItem[], total = items.length) {
  usePecoArtifactStore.setState({
    ingested: [],
    pages: items,
    total,
    loaded: true,
    loading: false,
    loadingMore: false,
    error: null,
    nextOffset: items.length,
  });
}

function byLabel(label: string): HTMLElement | null {
  return container.querySelector<HTMLElement>(`[aria-label="${label}"]`);
}

function downloadButtons(): HTMLElement[] {
  return Array.from(
    container.querySelectorAll<HTMLElement>('[aria-label^="下载 "]'),
  );
}

function text(): string {
  return container.textContent ?? "";
}

/**
 * jsdom 不实现 Tab 键移动焦点：按文档顺序在可聚焦元素间走一格。
 * `querySelectorAll` 返回文档序，因此这就是浏览器里的 Tab 顺序。
 */
function tab() {
  const focusable = Array.from(
    document.querySelectorAll<HTMLElement>(
      'button:not([disabled]), a[href], input:not([disabled]), [tabindex]:not([tabindex="-1"])',
    ),
  );
  const current = document.activeElement as HTMLElement | null;
  const next = focusable[focusable.indexOf(current as HTMLElement) + 1];
  if (!next) throw new Error("没有下一个可聚焦元素");
  next.focus();
}

/** 点预览按钮：浏览器里这次点按同时把焦点交给按钮，jsdom 不会，显式补上。 */
async function openPreview(button: HTMLElement) {
  button.focus();
  await act(async () => {
    button.dispatchEvent(new MouseEvent("click", { bubbles: true }));
  });
  await flush();
}

/**
 * 关闭查看器：焦点归还发生在 Radix FocusScope 卸载后的 setTimeout(0) 里，
 * 故必须再 flush 一轮才能断言 activeElement。
 */
async function closeDialog() {
  const close = document.querySelector<HTMLElement>(
    '[data-slot="dialog-content"] [data-slot="dialog-close"]',
  );
  if (!close) throw new Error("dialog close button not found");
  await act(async () => {
    close.dispatchEvent(new MouseEvent("click", { bubbles: true }));
  });
  await flush();
}

/** 面板根容器：触发按钮不可聚焦时的兜底焦点落点。 */
function panelRoot(): HTMLElement {
  const element = container.firstElementChild;
  if (!(element instanceof HTMLElement))
    throw new Error("panel root not found");
  return element;
}

/**
 * 直接渲染查看器并让它能自己关闭（`onClose` 后真的卸载，才谈得上焦点归还）。
 * 面板卸载时查看器会跟着卸载，走的是"触发按钮已脱离文档"那条路，到不了兜底分支，
 * 所以这个分支只能在组件层喂一个空 ref 来覆盖。
 */
function PreviewHost({
  target,
  triggerRef,
  fallbackRef,
}: {
  target: ArtifactItem;
  triggerRef: RefObject<HTMLButtonElement | null>;
  fallbackRef: RefObject<HTMLElement | null>;
}) {
  const [open, setOpen] = useState(true);
  if (!open) return null;
  return (
    <ArtifactPreviewDialog
      item={target}
      triggerElRef={triggerRef}
      fallbackFocusRef={fallbackRef}
      onClose={() => setOpen(false)}
    />
  );
}

/** radix TabsTrigger 在 mousedown（button=0）上激活，click 不切 tab。 */
async function activateTab(label: string) {
  const tab = Array.from(
    container.querySelectorAll<HTMLButtonElement>('[role="tab"]'),
  ).find((element) => element.textContent?.includes(label));
  if (!tab) throw new Error(`tab not found: ${label}`);
  await act(async () => {
    tab.dispatchEvent(
      new MouseEvent("mousedown", { bubbles: true, button: 0 }),
    );
  });
  await flush();
}

beforeAll(() => {
  URL.createObjectURL = vi.fn(() => `blob:mock-${(urlSeq += 1)}`);
  URL.revokeObjectURL = vi.fn();
});

let realLoadMore: () => Promise<void>;

beforeEach(() => {
  (
    globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT: boolean }
  ).IS_REACT_ACT_ENVIRONMENT = true;
  realLoadMore = usePecoArtifactStore.getState().loadMore;
  globalThis.ResizeObserver ??=
    ResizeObserverStub as unknown as typeof ResizeObserver;
  globalThis.IntersectionObserver =
    IntersectionObserverStub as unknown as typeof IntersectionObserver;
  IntersectionObserverStub.instances = [];

  releaseAll();
  usePecoArtifactStore.getState().__resetForTests();
  listArtifacts.mockReset();
  listArtifacts.mockReturnValue(new Promise(() => {}));
  getSpy.mockReset();
  getSpy.mockResolvedValue({
    data: { size: 1024, type: "image/png" } as unknown as Blob,
    headers: { "content-type": "image/png" },
  } as never);

  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
});

afterEach(async () => {
  await act(async () => root.unmount());
  usePecoArtifactStore.setState({ loadMore: realLoadMore });
  container.remove();
  releaseAll();
  usePecoArtifactStore.getState().__resetForTests();
  IntersectionObserverStub.instances = [];
});

describe("ArtifactPanel", () => {
  it("空态：还没有产物，且没有骨架", async () => {
    seed([], 0);
    await render();

    expect(text()).toContain("还没有产物");
    expect(
      container.querySelectorAll('[data-testid="artifact-skeleton"]').length,
    ).toBe(0);
  });

  it("首载中：渲染 3 个骨架卡片", async () => {
    // loaded=false 会让面板真的去拉第一页；让请求悬着以停在首载态
    usePecoArtifactStore.setState({ loading: true, loaded: false });
    await render();

    expect(
      container.querySelectorAll('[data-testid="artifact-skeleton"]').length,
    ).toBe(3);
  });

  it("四类卡片：图片有 alt、文档无 img、每卡都有下载按钮", async () => {
    const image = item({ kind: "image", title: "照片.png" });
    const video = item({ kind: "video", mime: "video/mp4", title: "录像.mp4" });
    const audio = item({
      kind: "audio",
      mime: "audio/mpeg",
      title: "录音.mp3",
    });
    const document = item({
      kind: "document",
      mime: "application/pdf",
      title: "报告.pdf",
    });
    seed([image, video, audio, document]);
    await render();
    await enterViewport();

    // 只有图片卡内联渲染 <img>，且 alt 取标题
    const images = Array.from(container.querySelectorAll("img"));
    expect(images.length).toBe(1);
    expect(images[0].getAttribute("alt")).toBe("照片.png");

    for (const artifact of [image, video, audio, document]) {
      expect(byLabel(`下载 ${artifact.title}`)).not.toBeNull();
    }
    expect(byLabel("预览 照片.png")).not.toBeNull();
    expect(byLabel("预览 录像.mp4")).not.toBeNull();
    expect(byLabel("预览 录音.mp3")).not.toBeNull();
    // 文档只有下载：没有预览按钮，也没有内联图片
    expect(byLabel("预览 报告.pdf")).toBeNull();
    expect(text()).toContain("PDF");
  });

  it("图片预览：Dialog 底栏带下载入口", async () => {
    const image = item({ kind: "image", title: "照片.png" });
    seed([image]);
    await render();
    await enterViewport();

    const preview = byLabel("预览 照片.png");
    if (!preview) throw new Error("preview button not found");
    await act(async () => {
      preview.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    });
    await flush();

    // Dialog 走 portal 挂到 body，卡片自己的同名下载按钮仍在 panel 里，须限定在 Dialog 内查
    const dialog = document.querySelector('[data-slot="dialog-content"]');
    expect(dialog).not.toBeNull();
    expect(
      dialog?.querySelector(`[aria-label="下载 ${image.title}"]`),
    ).not.toBeNull();
  });

  it("未知 kind：卡片不空白，展示原始 kind 文本", async () => {
    seed([item({ kind: "hologram", title: "全息图" })]);
    await render();

    expect(text()).toContain("hologram");
    expect(byLabel("下载 全息图")).not.toBeNull();
    expect(byLabel("预览 全息图")).toBeNull();
    expect(getSpy).not.toHaveBeenCalled();
  });

  it("available=false：两个按钮都禁用且不取流", async () => {
    seed([item({ kind: "image", title: "没了.png", available: false })]);
    await render();
    await enterViewport();

    expect(byLabel("预览 没了.png")).toBeDisabled();
    expect(byLabel("下载 没了.png")).toBeDisabled();
    expect(text()).toContain("文件已过期");
    expect(getSpy).not.toHaveBeenCalled();
  });

  it("全不可用：顶部提示条出现，列表仍渲染全部卡片", async () => {
    const expired = [
      item({ kind: "image", title: "a.png", available: false }),
      item({ kind: "video", title: "b.mp4", available: false }),
      item({ kind: "audio", title: "c.mp3", available: false }),
      item({ kind: "document", title: "d.pdf", available: false }),
    ];
    seed(expired);
    await render();

    expect(text()).toContain("已加载的产物中，这些文件已被清理");
    expect(downloadButtons().length).toBe(4);
  });

  it("分类过滤：点「图片」后只剩图片卡", async () => {
    const image = item({ kind: "image", title: "照片.png" });
    const video = item({ kind: "video", title: "录像.mp4" });
    seed([image, video]);
    await render();
    await enterViewport();
    expect(downloadButtons().length).toBe(2);

    await activateTab("图片");

    expect(downloadButtons().length).toBe(1);
    expect(byLabel("下载 录像.mp4")).toBeNull();
    expect(text()).not.toContain("录像.mp4");
  });

  it("切到「图片」后卸载重挂载：筛选仍是图片，不会退回全部", async () => {
    seed([
      item({ kind: "image", title: "照片.png" }),
      item({ kind: "document", mime: "application/pdf", title: "报告.pdf" }),
    ]);
    await render();
    await activateTab("图片");
    expect(byLabel("下载 报告.pdf")).toBeNull();

    await unmountPanel();
    await render();

    expect(byLabel("下载 照片.png")).not.toBeNull();
    expect(byLabel("下载 报告.pdf")).toBeNull();
  });

  it("50MiB 档视频：预览按钮禁用并提示下载后播放，下载仍可用", async () => {
    seed([item({ kind: "video", title: "大视频.mp4", size_bytes: 60 * MiB })]);
    await render();

    expect(byLabel("预览 大视频.mp4")).toBeDisabled();
    expect(text()).toContain("文件较大（60 MB），请下载后播放");
    expect(byLabel("下载 大视频.mp4")).not.toBeDisabled();
    expect(getSpy).not.toHaveBeenCalled();
  });

  it("超过 200MiB：预览与下载都禁用并给出上限文案", async () => {
    seed([item({ kind: "video", title: "超大.mp4", size_bytes: 201 * MiB })]);
    await render();

    expect(byLabel("预览 超大.mp4")).toBeDisabled();
    expect(byLabel("下载 超大.mp4")).toBeDisabled();
    expect(text()).toContain("文件超过 200 MB，暂不支持浏览器预览或下载");
    expect(getSpy).not.toHaveBeenCalled();
  });

  it("超过 200MiB 的图片/文档只提下载上限（本来就没有预览）", async () => {
    seed([
      item({ kind: "image", title: "超大.png", size_bytes: 201 * MiB }),
      item({ kind: "document", title: "超大.pdf", size_bytes: 201 * MiB }),
    ]);
    await render();

    expect(text()).toContain("文件超过 200 MB，暂不支持浏览器下载");
    expect(text()).not.toContain("暂不支持浏览器预览或下载");
  });

  it("超过 200MiB 的图片：预览区是静态占位，不是永久骨架", async () => {
    seed([item({ kind: "image", title: "超大.png", size_bytes: 201 * MiB })]);
    await render();
    await enterViewport();

    // 变异点：不处理 overHardLimit → 取流被禁用、状态停在 idle → 预览区一直是骨架
    const card = byLabel("下载 超大.png")?.closest(".rounded-lg");
    expect(card?.querySelector(".animate-pulse")).toBeNull();
    expect(text()).toContain("文件过大");
    expect(getSpy).not.toHaveBeenCalled();
  });

  it("Dialog 取流 404：把该项写回为已过期", async () => {
    const image = item({ kind: "image", title: "丢了.png" });
    seed([image]);
    // 不 enterViewport：卡片不取流，唯一请求来自 Dialog
    await render();
    getSpy.mockRejectedValueOnce(
      Object.assign(new Error("Not Found"), { response: { status: 404 } }),
    );

    const preview = byLabel("预览 丢了.png");
    if (!preview) throw new Error("preview button not found");
    await act(async () => {
      preview.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    });
    await flush();

    // 变异点：失败分支只 setState 不写回 store → 视频/音频（只走 Dialog）永不置灰
    const stored = usePecoArtifactStore
      .getState()
      .pages.find((artifact) => artifact.id === image.id);
    expect(stored?.available).toBe(false);
    // unavailable 不重试：一次请求即收尾
    expect(getSpy).toHaveBeenCalledTimes(1);
  });

  it("响应降级成 octet-stream 的图片：不渲染 img、显示无法预览、下载仍可用", async () => {
    getSpy.mockResolvedValue({
      data: { size: 1024, type: "application/octet-stream" } as unknown as Blob,
      headers: { "content-type": "application/octet-stream" },
    } as never);
    seed([item({ kind: "image", title: "伪装.png" })]);
    await render();
    await enterViewport();

    // 变异点：不校验 classifyPreview → blob 直接进 <img>（同源裂图，用户看不到原因）
    expect(container.querySelector("img")).toBeNull();
    expect(text()).toContain("无法预览");
    expect(byLabel("预览 伪装.png")).toBeDisabled();
    expect(byLabel("下载 伪装.png")).not.toBeDisabled();
  });

  it("视频卡进入视口既不 retain 也不取流", async () => {
    seed([item({ kind: "video", mime: "video/mp4", title: "录像.mp4" })]);
    await render();
    await enterViewport();

    // 变异点：恢复"所有 kind 都挂 IO" → 视频卡被 retain，缓存里留下它的条目
    expect(IntersectionObserverStub.instances.length).toBe(0);
    expect(__testCacheStats().refs).toBe(0);
    expect(__testCacheStats().entries).toBe(0);
    expect(getSpy).not.toHaveBeenCalled();
  });

  it("ingest 后面板直接出现新卡，不需要重新拉列表", async () => {
    seed([], 0);
    await render();
    expect(text()).toContain("还没有产物");

    await act(async () => {
      usePecoArtifactStore
        .getState()
        .ingest([item({ kind: "document", title: "新报告.pdf" })]);
    });
    await flush();

    expect(text()).toContain("新报告.pdf");
    // 增量的唯一来源是 SSE，面板不该为它再拉一次列表
    expect(listArtifacts).not.toHaveBeenCalled();
  });

  it("卡片内 Tab 依次落在预览与下载按钮", async () => {
    const image = item({ kind: "image", title: "顺序.png" });
    seed([image]);
    await render();
    await enterViewport();

    const visited: string[] = [];
    for (let i = 0; i < 12; i += 1) {
      tab();
      const label = document.activeElement?.getAttribute("aria-label") ?? "";
      if (label === `预览 ${image.title}`) {
        visited.push(label);
        tab();
        visited.push(document.activeElement?.getAttribute("aria-label") ?? "");
        break;
      }
    }

    // 变异点：把下载按钮挪到预览按钮之前 → 序列变成预览之后无元素 / 顺序反了
    expect(visited).toEqual([`预览 ${image.title}`, `下载 ${image.title}`]);
  });

  it("nextOffset < total 时渲染「加载更多」，点击调用 loadMore", async () => {
    const loadMoreSpy = vi.fn(async () => {});
    usePecoArtifactStore.setState({ loadMore: loadMoreSpy });
    seed(
      [
        item({ kind: "document", title: "d1" }),
        item({ kind: "document", title: "d2" }),
        item({ kind: "document", title: "d3" }),
        item({ kind: "document", title: "d4" }),
      ],
      5,
    );
    await render();

    const button = Array.from(container.querySelectorAll("button")).find(
      (element) => element.textContent?.includes("加载更多"),
    );
    expect(button).toBeDefined();

    await act(async () => {
      button?.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    });
    await flush();

    // 变异点：把 `nextOffset < total` 写成恒 false → 按钮不渲染 → 拿不到 button
    expect(loadMoreSpy).toHaveBeenCalledTimes(1);
  });

  it("nextOffset 追上 total 时不渲染「加载更多」", async () => {
    seed(
      [
        item({ kind: "document", title: "e1" }),
        item({ kind: "document", title: "e2" }),
      ],
      2,
    );
    await render();

    expect(text()).not.toContain("加载更多");
  });
});

describe("ArtifactPanel 查看器的焦点归还", () => {
  it("关闭预览后焦点回到该卡的预览按钮", async () => {
    seed([item({ kind: "image", title: "照片.png" })]);
    await render();

    const previewButton = byLabel("预览 照片.png");
    if (!previewButton) throw new Error("preview button not found");
    await openPreview(previewButton);
    // 打开后焦点进了查看器，才有"归还"可言
    expect(document.activeElement).not.toBe(previewButton);

    await closeDialog();

    // 变异点：DialogContent 不接管 onCloseAutoFocus → Radix 没有 triggerRef，
    // 默认归还 preventDefault 掉 → 焦点落 body
    expect(document.activeElement).toBe(previewButton);
  });

  it("先后预览两张卡：焦点回到最后预览的那张卡的按钮", async () => {
    seed([
      item({ kind: "image", title: "甲.png" }),
      item({ kind: "image", title: "乙.png" }),
    ]);
    await render();

    const firstButton = byLabel("预览 甲.png");
    const secondButton = byLabel("预览 乙.png");
    if (!firstButton || !secondButton)
      throw new Error("preview button missing");

    await openPreview(firstButton);
    await closeDialog();
    expect(document.activeElement).toBe(firstButton);

    await openPreview(secondButton);
    await closeDialog();

    // 变异点：面板层只在 ref 为空时写入（或记住首张卡）→ 焦点回到甲卡 → 失败
    expect(document.activeElement).toBe(secondButton);
  });

  it("关闭期间触发按钮被卸载：不把焦点交给脱离文档的元素", async () => {
    const image = item({ kind: "image", title: "会消失.png" });
    seed([image]);
    await render();

    const previewButton = byLabel("预览 会消失.png");
    if (!previewButton) throw new Error("preview button not found");
    await openPreview(previewButton);

    // 查看器开着时列表刷新，原卡片连同按钮一起卸载
    await act(async () => {
      seed([item({ kind: "document", title: "新产物.pdf" })]);
    });
    await flush();
    expect(previewButton.isConnected).toBe(false);

    const detachedFocus = vi.spyOn(previewButton, "focus");
    await closeDialog();

    // 变异点：去掉 isConnected 校验 → 对已卸载的元素调 focus（jsdom 里不抛错，
    // 真浏览器里焦点凭空消失），这里盯住"有没有去碰它"
    expect(detachedFocus).not.toHaveBeenCalled();
    expect(document.activeElement).toBe(document.body);
  });

  it("预览期间卡片被置灰：焦点落到面板容器而不是 body", async () => {
    // 视频卡不在视口取流，唯一一次请求来自查看器；404 把该项写回为已过期，
    // 卡片随之把预览按钮置灰（这正是"取流才发现文件没了"的真实路径）
    seed([item({ kind: "video", mime: "video/mp4", title: "录像.mp4" })]);
    await render();
    getSpy.mockRejectedValueOnce(
      Object.assign(new Error("Not Found"), { response: { status: 404 } }),
    );

    const previewButton = byLabel("预览 录像.mp4");
    if (!previewButton) throw new Error("preview button not found");
    await openPreview(previewButton);
    await flush();
    // 按钮仍在文档里，但对 disabled 元素调 focus() 是空操作
    expect(previewButton.isConnected).toBe(true);
    expect(previewButton).toBeDisabled();

    await closeDialog();

    // 变异点：disabled 时仍只 trigger.focus() → 焦点落 body（读屏回到页面开头）
    expect(document.activeElement).toBe(panelRoot());

    // 落点自己要有名字，否则无名字的 group 容器上播报内容由 AT 自行决定
    // 变异点：删 role / 删 aria-label / 改错名字文本 → 三个变异共命中下面两条断言
    // （删 role 红第一条；删 aria-label 与改名字文本都红第二条）
    const landing = document.activeElement as HTMLElement;
    expect(landing).toHaveAttribute("role", "group");
    expect(landing).toHaveAttribute("aria-label", "对话产物");
  });

  it("兜底容器也拿不到时：不抛错，也不把焦点交给任何元素", async () => {
    const trigger = document.createElement("button");
    trigger.disabled = true;
    document.body.appendChild(trigger);
    const triggerRef = createRef<HTMLButtonElement>();
    triggerRef.current = trigger;
    const fallbackRef = createRef<HTMLDivElement>();

    await act(async () => {
      root.render(
        <PreviewHost
          target={item({ kind: "image", title: "孤儿.png" })}
          triggerRef={triggerRef}
          fallbackRef={fallbackRef}
        />,
      );
    });
    await flush();
    expect(fallbackRef.current).toBeNull();

    const windowErrors: unknown[] = [];
    const onError = (event: ErrorEvent) => windowErrors.push(event.error);
    window.addEventListener("error", onError);
    await closeDialog();
    window.removeEventListener("error", onError);

    // 变异点：兜底分支写成 fallbackFocusRef.current!.focus() → null 上取方法抛错
    expect(windowErrors).toEqual([]);
    expect(triggerRef.current).toBe(trigger);
    expect(document.activeElement).toBe(document.body);
    trigger.remove();
  });
});
