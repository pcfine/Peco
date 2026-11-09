// [浏览器] v3 验收台：把 v3 设计的两份验收清单（系统口径 + 三页工作台）落成真实量测。
//
// 结构沿用 ui.spec.ts：每条断言打印「期望 vs 实测」、落盘、末尾汇总表用 EXPECTED_IDS 兜底。
// 取证口径：尺寸读 `getBoundingClientRect()`（不读取整的 offsetWidth），字号/颜色/圆角读
// `getComputedStyle()`，容器像素上限与 ad-hoc 类名在源码上数。实测与规格不符就记 FAIL ——
// 这里只测量，不改期望值，也不改产品代码。
//
// 预置状态走 addInitScript + sessionStorage 中转：同一页里要换 rail / dock / 右栏存档时，
// 只改 sessionStorage 再 reload，不必为每一档新建 context。

import fs from "node:fs";
import path from "node:path";
import {
  test,
  expect,
  type APIRequestContext,
  type Browser,
  type BrowserContext,
  type Page,
} from "@playwright/test";

const BASE_URL = process.env.E2E_BASE_URL ?? "http://127.0.0.1:5174";

/** 左栏打开集：够验证壳层栅格，也让 /peco 的总览面板有数据。 */
const OPEN_IDS = ["ws_demo_legal", "ws_demo_finance", "ws_demo_ops"];
/** 种子里唯一带对话的空间：状态页「会话」tab 有真实转录，滚动与正文宽度都取自它。 */
const CHAT_WS = "ws_demo_legal";
/** 预置状态在 sessionStorage 里的中转键（reload 换档用）。 */
const PRESET_KEY = "e2e:v3-preset";
/** 单条对话里 agent 的固定名字，空间会话与状态页会话列表都挂在它上面。 */
const MASTER_AGENT = "@master";

const VIEW = {
  wide: { width: 1920, height: 1080 },
  trio: { width: 1440, height: 900 },
  dockBar: { width: 1280, height: 800 },
  narrow: { width: 900, height: 800 },
} as const;

/** 壳层宽度的六档视口：A12 在这六档上各测 rail 展开与收起两种状态。 */
const TIERS = [
  { width: 1920, height: 1080 },
  { width: 1440, height: 900 },
  { width: 1280, height: 800 },
  { width: 1200, height: 800 },
  { width: 1024, height: 800 },
  { width: 900, height: 800 },
];

type Verdict = "PASS" | "FAIL";

interface Row {
  id: string;
  expectation: string;
  actual: string;
  verdict: Verdict;
  note?: string;
}

/** 全部断言的 id：少一条就说明有断言没跑到（汇总表据此兜底）。 */
const EXPECTED_IDS = [
  "A1",
  "A2",
  "A3",
  "A4",
  "A5",
  "A6",
  "A7",
  "A8",
  "A9",
  "A10",
  "A11",
  "A12",
  "A13",
  "B2",
  "B3",
  "B4",
  "B5",
  "B6",
  "B7",
  "B9",
  "C1",
  "C2",
  "C3",
  "C5",
  "C6",
  "C8",
  "D1",
  "D2",
  "D3",
  "D5",
  "E2",
  "E3",
  "E4",
  "E5",
  "E6",
  "E7",
  "E8",
  "F1",
  "F2",
  "F3",
  "F4",
  "F5",
  "F7",
  "F8",
  "F9",
  "F10",
  "F11",
  "G1",
  "G2",
  "G3",
  "G4",
];

/**
 * 实测行落盘而不是留在内存：断言失败会让 Playwright 换一个 worker 重新导入本文件，
 * 模块里的数组活不过那一次重启。与 ui.spec.ts 各写一份 —— 两份汇总表互不串行。
 */
const ROWS_FILE = path.join(process.cwd(), "e2e", ".report", "v3-rows.jsonl");

function format(value: unknown): string {
  if (Array.isArray(value))
    return value.length === 0 ? "[]" : value.join(" / ");
  if (value === null) return "null";
  return String(value);
}

/**
 * 记一行实测并软断言：失败只让本条变红，后面的断言继续跑完 —— 测量台要的是全量实测值。
 * 断言消息里带实测值：回归时能直接看出漂移，不必翻日志。
 */
function check(
  id: string,
  expectation: string,
  actual: unknown,
  expected: unknown,
  note?: string,
): void {
  const same = JSON.stringify(actual) === JSON.stringify(expected);
  const row: Row = {
    id,
    expectation,
    actual: format(actual),
    verdict: same ? "PASS" : "FAIL",
    note,
  };
  fs.mkdirSync(path.dirname(ROWS_FILE), { recursive: true });
  fs.appendFileSync(ROWS_FILE, `${JSON.stringify(row)}\n`);
  console.log(
    `\n[${id}] 期望 ${expectation}\n     实测 ${format(actual)}` +
      (note ? `\n     补记 ${note}` : "") +
      `\n     判定 ${row.verdict}`,
  );
  expect
    .soft(
      same,
      `${id} 期望 ${expectation} / 实测 ${format(actual)}${note ? `（${note}）` : ""}`,
    )
    .toBe(true);
}

/** 尺寸断言的容差判据：设计写的是「±1」。 */
function near(
  actual: number | null | undefined,
  want: number,
  tol = 1,
): boolean {
  return typeof actual === "number" && Math.abs(actual - want) <= tol;
}

const POLL_MS = 100;

/** 轮询到谓词成立，返回最后一次实测值：等不到时消息里给的是测到的数，不是「超时」。 */
async function until<T>(
  probe: () => Promise<T>,
  accept: (value: T) => boolean,
  timeoutMs = 15_000,
): Promise<T> {
  const deadline = Date.now() + timeoutMs;
  let value = await probe();
  while (!accept(value) && Date.now() < deadline) {
    await new Promise((resolve) => setTimeout(resolve, POLL_MS));
    value = await probe();
  }
  return value;
}

interface PanelArchive {
  mode: string;
  collapsed: boolean;
}

/** 预置到 localStorage 的壳层状态；形状与各 store 的 persist 一致。 */
interface Preset {
  theme?: "dark" | "light";
  railOpen?: boolean;
  dockOpen?: boolean;
  openIds?: string[];
  activeWsId?: string | null;
  panels?: Record<string, PanelArchive>;
}

function basePreset(overrides: Preset = {}): Preset {
  return {
    theme: "dark",
    railOpen: true,
    dockOpen: true,
    openIds: OPEN_IDS,
    activeWsId: CHAT_WS,
    ...overrides,
  };
}

interface SessionOptions {
  viewport: { width: number; height: number };
  path: string;
  preset?: Preset;
  /** 首屏就绪的选择器：等到它出现，后续量测才有意义。 */
  ready: string;
  hasTouch?: boolean;
}

interface Session {
  context: BrowserContext;
  page: Page;
  dispose: () => Promise<void>;
}

/** 一个独立浏览器上下文：localStorage 预置 + 首屏就绪等待。 */
async function openSession(
  browser: Browser,
  options: SessionOptions,
): Promise<Session> {
  const context = await browser.newContext({
    viewport: options.viewport,
    baseURL: BASE_URL,
    hasTouch: options.hasTouch ?? false,
    reducedMotion: "no-preference",
  });
  await context.addInitScript(
    (payload: { key: string; json: string }) => {
      const raw = sessionStorage.getItem(payload.key);
      const preset = JSON.parse(raw ?? payload.json) as {
        theme?: string;
        railOpen?: boolean;
        dockOpen?: boolean;
        openIds?: string[];
        activeWsId?: string | null;
        panels?: Record<string, { mode: string; collapsed: boolean }>;
      };
      localStorage.setItem(
        "peco-webui:theme",
        JSON.stringify({
          state: { theme: preset.theme ?? "dark" },
          version: 0,
        }),
      );
      localStorage.setItem(
        "peco-webui:rail",
        JSON.stringify({
          state: { railOpen: preset.railOpen ?? true },
          version: 0,
        }),
      );
      localStorage.setItem(
        "peco-webui:dock",
        JSON.stringify({
          state: {
            open: preset.dockOpen ?? true,
            tab: "chat",
            conversationByWs: {},
          },
          version: 0,
        }),
      );
      localStorage.setItem(
        "peco-webui:ws",
        JSON.stringify({
          state: { currentWsId: preset.activeWsId ?? null },
          version: 0,
        }),
      );
      localStorage.setItem(
        "peco-webui:open-spaces",
        JSON.stringify({
          state: {
            openIds: preset.openIds ?? [],
            activeWsId: preset.activeWsId ?? null,
            seeded: true,
          },
          version: 0,
        }),
      );
      for (const [wsId, panel] of Object.entries(preset.panels ?? {})) {
        localStorage.setItem(
          `peco-webui:space-panel:${wsId}`,
          JSON.stringify(panel),
        );
      }
    },
    {
      key: PRESET_KEY,
      json: JSON.stringify(options.preset ?? basePreset()),
    },
  );

  const page = await context.newPage();
  await page.goto(options.path, { waitUntil: "load" });
  await page.waitForSelector(options.ready, { timeout: 20_000 });
  return { context, page, dispose: () => context.close() };
}

/** 换一套壳层存档并重载：addInitScript 每次导航都会重跑，sessionStorage 是它的入口。 */
async function reloadWithPreset(
  page: Page,
  preset: Preset,
  ready: string,
): Promise<void> {
  await page.evaluate(
    (payload: { key: string; json: string }) =>
      sessionStorage.setItem(payload.key, payload.json),
    { key: PRESET_KEY, json: JSON.stringify(basePreset(preset)) },
  );
  await page.reload({ waitUntil: "load" });
  await page.waitForSelector(ready, { timeout: 20_000 });
}

/** 路由 → 一屏就绪选择器；`teamId` / `emptySessionWs` 由 beforeAll 求出。 */
interface Route {
  id: string;
  path: string;
  ready: string;
}

let teamId = "";
let emptySessionWs = "";

function allRoutes(): Route[] {
  return [
    { id: "peco", path: "/peco", ready: '[data-testid="peco-panel"]' },
    {
      id: "peco-config",
      path: "/peco?face=config",
      ready: '[data-testid="peco-face-config"]',
    },
    { id: "spaces", path: "/spaces", ready: "main h1" },
    { id: "spaces-new", path: "/spaces/new", ready: "main h1" },
    {
      id: "space",
      path: `/spaces/${CHAT_WS}`,
      ready: '[data-testid="space-chat-shell"]',
    },
    {
      id: "space-status",
      path: `/spaces/${CHAT_WS}/status`,
      ready: '[role="tablist"]',
    },
    { id: "teams", path: "/teams", ready: "main h1" },
    { id: "team", path: `/teams/${teamId}`, ready: "main h1" },
    { id: "results", path: "/results", ready: "main h1" },
    { id: "settings", path: "/settings", ready: "main h1" },
    { id: "schedules", path: "/schedules", ready: "main h1" },
  ];
}

function routesOf(ids: string[]): Route[] {
  const table = new Map(allRoutes().map((route) => [route.id, route]));
  return ids.map((id) => {
    const route = table.get(id);
    if (!route) throw new Error(`未登记的路由 ${id}`);
    return route;
  });
}

/** 数据查询落定的等待：骨架与终态的字号/边框/命中区都不一样，量早了测的是骨架。 */
async function settle(page: Page, ms = 700): Promise<void> {
  await page.waitForTimeout(ms);
}

interface Bar {
  width: number;
  height: number;
  left: number;
  right: number;
  scrollWidth: number;
  clientWidth: number;
}

interface ShellFacts {
  rail: Bar | null;
  chat: Bar | null;
  panel: Bar | null;
  /** Peco 抽屉：常驻挂载，关着时靠 translateX 移出视口。 */
  dock: Bar | null;
  /** 抽屉的水平位移（px，关着为 −抽屉宽）。 */
  dockTranslate: number | null;
  dockBottom: Bar | null;
  dockSheet: Bar | null;
  drawer: Bar | null;
  main: Bar | null;
  documentOverflow: number;
  expandRail: number;
}

/** 壳层几何：rail / 聊天列 / 右栏 / Peco 抽屉 / 右栏抽屉 / main，一次读齐。 */
async function shellFacts(page: Page): Promise<ShellFacts> {
  return page.evaluate(() => {
    const bar = (element: Element | null) => {
      if (!element) return null;
      const rect = element.getBoundingClientRect();
      const node = element as HTMLElement;
      return {
        width: Math.round(rect.width),
        height: Math.round(rect.height),
        left: Math.round(rect.left),
        right: Math.round(rect.right),
        scrollWidth: node.scrollWidth,
        clientWidth: node.clientWidth,
      };
    };
    /**
     * 读水平位移：Tailwind v4 的 `translate-x-*` 落在 CSS `translate` 属性上，
     * 不是 `transform`（后者恒为 none，读它会把「移出视口」误判成 0）。
     * `none` 记 0；百分比按元素自身宽度折成 px。
     */
    const translateX = (element: Element | null) => {
      if (!element) return null;
      const raw = getComputedStyle(element).translate;
      if (!raw || raw === "none") return 0;
      const first = raw.trim().split(/\s+/)[0];
      const value = Number.parseFloat(first);
      if (Number.isNaN(value)) return null;
      return Math.round(
        first.endsWith("%")
          ? (value / 100) * element.getBoundingClientRect().width
          : value,
      );
    };
    return {
      rail: bar(document.querySelector("aside:not([id]):not([data-testid])")),
      chat: bar(
        document.querySelector(
          '[data-testid="space-chat-shell"][data-visible="true"]',
        ),
      ),
      panel: bar(document.querySelector('[data-testid="space-panel"]')),
      dock: bar(document.querySelector("#peco-dock")),
      dockTranslate: translateX(document.querySelector("#peco-dock")),
      dockBottom: bar(document.querySelector("#peco-dock-bottom")),
      dockSheet: bar(document.querySelector("#peco-dock-sheet")),
      drawer: bar(
        document.querySelector(
          '[data-slot="sheet-content"][aria-label="空间右栏"]',
        ),
      ),
      main: bar(document.getElementById("main")),
      documentOverflow:
        document.documentElement.scrollWidth -
        document.documentElement.clientWidth,
      expandRail: document.querySelectorAll('button[aria-label="展开左栏"]')
        .length,
    };
  });
}

interface FontFacts {
  sizes: number[];
  /** 11px 节点里文本不是纯 ASCII 的：中文最小 12px，11px 只放 ASCII。 */
  micro: string[];
  /** 13px 节点出现过的 lineHeight（正文档只允许 20px）。 */
  bodyLines: string[];
  /** 含 CJK 且字重不在 400/500/600 的样例。 */
  weights: string[];
}

/** 字号 / 行高 / 字重：只看「自己带文本」且可见的节点，与 audit 同口径。 */
async function fontFacts(page: Page): Promise<FontFacts> {
  return page.evaluate(() => {
    const visible = (element: Element) => {
      const rect = element.getBoundingClientRect();
      const style = getComputedStyle(element);
      return (
        rect.width > 0 &&
        rect.height > 0 &&
        style.visibility !== "hidden" &&
        style.display !== "none" &&
        Number(style.opacity) > 0.05
      );
    };
    const sizes = new Set<number>();
    const micro = new Set<string>();
    const bodyLines = new Set<string>();
    const weights = new Set<string>();
    for (const element of Array.from(document.querySelectorAll("body *"))) {
      const ownsText = Array.from(element.childNodes).some(
        (node) =>
          node.nodeType === 3 && (node.textContent ?? "").trim().length > 1,
      );
      if (!ownsText || !visible(element)) continue;
      const style = getComputedStyle(element);
      const size = Math.round(Number.parseFloat(style.fontSize));
      sizes.add(size);
      const text = (element.textContent ?? "").trim();
      if (size === 11 && !/^[\x20-\x7E]*$/.test(text)) {
        // 带上字号/字重类名：11px 的中文要能一眼定位到组件，否则只报文案等于没说根因。
        const hint =
          (element.getAttribute("class") ?? "")
            .split(" ")
            .filter((token) => /^(text-|font-|leading-)/.test(token))
            .join(".") || element.tagName;
        micro.add(`${text.slice(0, 24)}〔${hint}〕`);
      }
      if (size === 13) bodyLines.add(style.lineHeight);
      if (
        /[\u4e00-\u9fff]/.test(text) &&
        !["400", "500", "600"].includes(style.fontWeight)
      )
        weights.add(`${style.fontWeight} · ${text.slice(0, 16)}`);
    }
    return {
      sizes: [...sizes].sort((a, b) => a - b),
      micro: [...micro],
      bodyLines: [...bodyLines].sort(),
      weights: [...weights],
    };
  });
}

interface BorderFacts {
  /** 可见描边元素数：任一边 width>0、style≠none 且该边 color 不透明。 */
  stroked: number;
  /** 排除 select 触发器与选中态后，仍带可见描边的 button（附文案）。 */
  buttonStroked: string[];
  /** 全站四角圆角计算值去重。 */
  radii: string[];
}

async function borderFacts(page: Page): Promise<BorderFacts> {
  return page.evaluate(() => {
    const visible = (element: Element) => {
      const rect = element.getBoundingClientRect();
      const style = getComputedStyle(element);
      return (
        rect.width > 0 &&
        rect.height > 0 &&
        style.visibility !== "hidden" &&
        style.display !== "none"
      );
    };
    const alphaOf = (color: string) => {
      if (color === "transparent") return 0;
      const match = color.match(/rgba?\(([^)]+)\)/);
      if (!match) return 1;
      const parts = match[1]
        .split(/[,\s/]+/)
        .filter(Boolean)
        .map(Number);
      return parts.length > 3 ? parts[3] : 1;
    };
    const sides = ["top", "right", "bottom", "left"];
    const corners = [
      "border-top-left-radius",
      "border-top-right-radius",
      "border-bottom-right-radius",
      "border-bottom-left-radius",
    ];
    let stroked = 0;
    const buttonStroked: string[] = [];
    const radii = new Set<string>();
    for (const element of Array.from(document.querySelectorAll("body *"))) {
      if (!visible(element)) continue;
      const style = getComputedStyle(element);
      const bordered = sides.some((side) => {
        const width = Number.parseFloat(
          style.getPropertyValue(`border-${side}-width`),
        );
        const line = style.getPropertyValue(`border-${side}-style`);
        const color = style.getPropertyValue(`border-${side}-color`);
        return width > 0 && line !== "none" && alphaOf(color) > 0;
      });
      if (bordered) {
        stroked += 1;
        // 选中/展开态的圆角矩形描边是记号不是装饰：`aria-pressed` / `aria-current` /
        // `data-state=on` / `aria-expanded=true` 任一成立就放行，其余仍按 0 描边判。
        // 左栏品牌行既是 Peco 抽屉的开关，也是它的选中态（抽屉开着 ⇒ aria-expanded）。
        const selected =
          element.getAttribute("aria-pressed") === "true" ||
          element.hasAttribute("aria-current") ||
          element.getAttribute("data-state") === "on" ||
          element.getAttribute("aria-expanded") === "true";
        if (
          element.tagName === "BUTTON" &&
          !selected &&
          !element.matches('[data-slot="select-trigger"]')
        )
          buttonStroked.push(
            (element.getAttribute("aria-label") ?? element.textContent ?? "")
              .trim()
              .slice(0, 16) || "（无文案）",
          );
      }
      for (const corner of corners) radii.add(style.getPropertyValue(corner));
    }
    return {
      stroked,
      buttonStroked,
      radii: [...radii].sort(),
    };
  });
}

/** 圆角是否落在允许的三档（0 / 1px 状态点 / 4 / 6 / full 的钳制值）。 */
function allowedRadius(value: string): boolean {
  const px = Number.parseFloat(value);
  if (Number.isNaN(px)) return false;
  return px === 0 || px === 1 || px === 4 || px === 6 || px >= 9999;
}

interface Target {
  label: string;
  tag: string;
  height: number;
  width: number;
}

/** 可点元素里高度 <24 的项（跳过 sr-only 的「跳到内容」）。 */
async function shortTargets(page: Page): Promise<Target[]> {
  return page.evaluate(() => {
    const visible = (element: Element) => {
      const rect = element.getBoundingClientRect();
      const style = getComputedStyle(element);
      return (
        rect.width > 0 &&
        rect.height > 0 &&
        style.visibility !== "hidden" &&
        style.display !== "none"
      );
    };
    const short: {
      label: string;
      tag: string;
      height: number;
      width: number;
    }[] = [];
    const nodes = Array.from(
      document.querySelectorAll(
        'button, a[href], [role="button"], [role="tab"]',
      ),
    );
    for (const element of nodes) {
      if (element.classList.contains("sr-only") || !visible(element)) continue;
      const rect = element.getBoundingClientRect();
      if (rect.height >= 24) continue;
      short.push({
        label:
          (element.getAttribute("aria-label") ?? element.textContent ?? "")
            .trim()
            .slice(0, 20) || "（无文案）",
        tag: element.tagName,
        height: Math.round(rect.height),
        width: Math.round(rect.width),
      });
    }
    return short;
  });
}

interface ContrastFacts {
  fails: number;
  worst: { text: string; ratio: number; fg: string; bg: string }[];
}

/** 对比度：算法照抄 audit.spec.ts（同一条基线才可比）。 */
async function contrastFacts(page: Page): Promise<ContrastFacts> {
  return page.evaluate(() => {
    const visible = (element: Element) => {
      const rect = element.getBoundingClientRect();
      const style = getComputedStyle(element);
      return (
        rect.width > 0 &&
        rect.height > 0 &&
        style.visibility !== "hidden" &&
        style.display !== "none" &&
        Number(style.opacity) > 0.05
      );
    };
    type Rgba = { r: number; g: number; b: number; a: number };
    const parse = (color: string): Rgba | null => {
      const match = color.match(/rgba?\(([^)]+)\)/);
      if (!match) return null;
      const parts = match[1]
        .split(/[,\s/]+/)
        .filter(Boolean)
        .map(Number);
      return {
        r: parts[0],
        g: parts[1],
        b: parts[2],
        a: parts.length > 3 ? parts[3] : 1,
      };
    };
    const blend = (top: Rgba, bottom: Rgba): Rgba => ({
      r: top.r * top.a + bottom.r * (1 - top.a),
      g: top.g * top.a + bottom.g * (1 - top.a),
      b: top.b * top.a + bottom.b * (1 - top.a),
      a: 1,
    });
    const bgOf = (element: Element): Rgba => {
      let acc: Rgba | null = null;
      let current: Element | null = element;
      while (current) {
        const color = parse(getComputedStyle(current).backgroundColor);
        if (color && color.a > 0) acc = acc ? blend(acc, color) : color;
        if (acc && acc.a >= 0.999) break;
        current = current.parentElement;
      }
      return acc ?? { r: 255, g: 255, b: 255, a: 1 };
    };
    const lum = (color: Rgba) => {
      const channel = (value: number) => {
        const scaled = value / 255;
        return scaled <= 0.03928
          ? scaled / 12.92
          : Math.pow((scaled + 0.055) / 1.055, 2.4);
      };
      return (
        0.2126 * channel(color.r) +
        0.7152 * channel(color.g) +
        0.0722 * channel(color.b)
      );
    };
    const ratio = (a: Rgba, b: Rgba) => {
      const first = lum(a);
      const second = lum(b);
      return (
        (Math.max(first, second) + 0.05) / (Math.min(first, second) + 0.05)
      );
    };
    const worst: {
      text: string;
      ratio: number;
      fg: string;
      bg: string;
    }[] = [];
    let fails = 0;
    for (const element of Array.from(document.querySelectorAll("body *"))) {
      const ownsText = Array.from(element.childNodes).some(
        (node) =>
          node.nodeType === 3 && (node.textContent ?? "").trim().length > 1,
      );
      if (!ownsText || !visible(element)) continue;
      const style = getComputedStyle(element);
      const size = Number.parseFloat(style.fontSize);
      const fg = parse(style.color);
      if (!fg || fg.a <= 0.3) continue;
      const bg = bgOf(element);
      const value = ratio(fg, bg);
      const big =
        size >= 18.66 || (size >= 14 && Number(style.fontWeight) >= 700);
      if (value < (big ? 3 : 4.5)) {
        fails += 1;
        worst.push({
          text: (element.textContent ?? "").trim().slice(0, 20),
          ratio: Number(value.toFixed(2)),
          fg: style.color,
          bg: `rgb(${Math.round(bg.r)},${Math.round(bg.g)},${Math.round(bg.b)})`,
        });
      }
    }
    worst.sort((a, b) => a.ratio - b.ratio);
    return { fails, worst: worst.slice(0, 4) };
  });
}

interface NamedTarget {
  label: string;
  /** 哪条路由上量：一次进页面尽量量完。 */
  path: string;
  ready: string;
  role: "button" | "link" | "tab";
  name: string | RegExp;
  exact?: boolean;
}

/** 按 accessible name 逐个量高：量不到的记 null，让汇总表直接看出「没这个元素」。 */
async function namedHeights(
  page: Page,
  targets: NamedTarget[],
): Promise<Record<string, number | null>> {
  const heights: Record<string, number | null> = {};
  const byPath = new Map<string, NamedTarget[]>();
  for (const target of targets) {
    const bucket = byPath.get(target.path) ?? [];
    bucket.push(target);
    byPath.set(target.path, bucket);
  }
  for (const [path, items] of byPath) {
    await page.goto(path, { waitUntil: "load" });
    await page.waitForSelector(items[0].ready, { timeout: 20_000 });
    await settle(page);
    for (const item of items) {
      const box = await page
        .getByRole(item.role, {
          name: item.name,
          exact: item.exact ?? true,
        })
        .first()
        .boundingBox({ timeout: 3_000 })
        .catch(() => null);
      heights[item.label] = box ? Math.round(box.height) : null;
    }
  }
  return heights;
}

// ── 数据准备 ───────────────────────────────────────────────────────────────

async function openSpace(
  request: APIRequestContext,
  wsId: string,
): Promise<void> {
  const response = await request.get("/api/spaces");
  if (!response.ok())
    throw new Error(`GET /api/spaces 失败：${response.status()}`);
  const spaces = (await response.json()) as {
    ws_id: string;
    work_dir: string;
    team_id: string;
    status: string;
    reserved: boolean;
  }[];
  const space = spaces.find((item) => item.ws_id === wsId);
  if (!space) throw new Error(`空间 ${wsId} 不在 /api/spaces 里`);
  if (space.status === "open") return;
  const opened = await request.post("/api/spaces", {
    data: { work_dir: space.work_dir, team_id: space.team_id },
  });
  if (!opened.ok())
    throw new Error(
      `打开 ${wsId} 失败：${opened.status()} ${await opened.text()}`,
    );
}

/** 该空间的 agent 级会话数：状态页「会话」tab 空态的判据。 */
async function conversationCount(
  request: APIRequestContext,
  wsId: string,
): Promise<number> {
  const response = await request.get(
    `/api/chat/${encodeURIComponent(MASTER_AGENT)}/conversations`,
    { headers: { "X-Peco-Space": wsId } },
  );
  if (!response.ok())
    throw new Error(
      `GET 会话列表失败（${wsId}）：${response.status()} ${await response.text()}`,
    );
  return ((await response.json()) as unknown[]).length;
}

/**
 * 找一个一条会话都没有的空间：「开始对话」只在空会话态出现，
 * 而哪个空间空着是种子的事实 —— 照会话接口逐个问，不在这里抄一份。
 */
async function findSpaceWithoutConversations(
  request: APIRequestContext,
): Promise<string> {
  const response = await request.get("/api/spaces");
  if (!response.ok())
    throw new Error(`GET /api/spaces 失败：${response.status()}`);
  const spaces = (await response.json()) as {
    ws_id: string;
    reserved: boolean;
  }[];
  for (const space of spaces) {
    if (space.reserved) continue;
    if ((await conversationCount(request, space.ws_id)) === 0)
      return space.ws_id;
  }
  throw new Error("种子里的每个空间都已有会话：空会话态没有空数据可量");
}

/** 首个非保留团队：`/teams/:teamId` 需要一个真实 id。 */
async function firstTeamId(request: APIRequestContext): Promise<string> {
  const response = await request.get("/api/teams");
  if (!response.ok())
    throw new Error(`GET /api/teams 失败：${response.status()}`);
  const teams = (await response.json()) as { team_id: string }[];
  const team = teams.find((item) => item.team_id !== "peco") ?? teams[0];
  if (!team) throw new Error("GET /api/teams 没有返回团队：seed 是否跑过？");
  return team.team_id;
}

test.beforeAll(async ({ request }) => {
  // 右栏面板（文件 / 任务 / 记忆）只在空间打开后才有真实数据；未打开时它们走 409 分支，
  // 量到的是错误态而不是默认布局。
  for (const wsId of OPEN_IDS) await openSpace(request, wsId);
  teamId = await firstTeamId(request);
  emptySessionWs = await findSpaceWithoutConversations(request);
  await openSpace(request, emptySessionWs);
});

// ── 量测小工具 ─────────────────────────────────────────────────────────────

/** `main` 里页面根容器的宽度（容器类挂在 `@container/work` 的内层 div 上）。 */
async function containerWidth(page: Page): Promise<number | null> {
  return page.evaluate(() => {
    const root = document.querySelector("main > div > *");
    return root ? Math.round(root.getBoundingClientRect().width) : null;
  });
}

async function scrollTopOf(page: Page, selector: string): Promise<number> {
  return page.evaluate((sel) => {
    const node = document.querySelector(sel);
    return node ? Math.round(node.scrollTop) : -1;
  }, selector);
}

/** 设滚动位置并回读实际值（超出可滚范围时浏览器会钳制，回读值才是基线）。 */
async function scrollTo(
  page: Page,
  selector: string,
  top: number,
): Promise<number> {
  return page.evaluate(
    (payload: { sel: string; top: number }) => {
      const node = document.querySelector(payload.sel);
      if (!node) return -1;
      node.scrollTop = payload.top;
      return Math.round(node.scrollTop);
    },
    { sel: selector, top },
  );
}

interface PrimaryColors {
  fg: string;
  bg: string;
  ratio: number;
}

/** 主按钮（`bg-primary`）的白字对比度：取 `main` 里第一个 default 变体按钮。 */
async function primaryContrast(page: Page): Promise<PrimaryColors | null> {
  return page.evaluate(() => {
    const node =
      document.querySelector(
        'main [data-slot="button"][data-variant="default"]',
      ) ??
      document.querySelector('[data-slot="button"][data-variant="default"]');
    if (!node) return null;
    const style = getComputedStyle(node);
    const parse = (color: string) => {
      const match = color.match(/rgba?\(([^)]+)\)/);
      if (!match) return null;
      const parts = match[1]
        .split(/[,\s/]+/)
        .filter(Boolean)
        .map(Number);
      return { r: parts[0], g: parts[1], b: parts[2] };
    };
    const channel = (value: number) => {
      const scaled = value / 255;
      return scaled <= 0.03928
        ? scaled / 12.92
        : Math.pow((scaled + 0.055) / 1.055, 2.4);
    };
    const lum = (color: { r: number; g: number; b: number }) =>
      0.2126 * channel(color.r) +
      0.7152 * channel(color.g) +
      0.0722 * channel(color.b);
    const fg = parse(style.color);
    const bg = parse(style.backgroundColor);
    const normalize = (color: string) =>
      color.replace(/\s+/g, "").replace(/^rgba\(/, "rgb(");
    if (!fg || !bg) return null;
    const first = lum(fg);
    const second = lum(bg);
    return {
      fg: normalize(style.color),
      bg: normalize(style.backgroundColor),
      ratio:
        Math.round(
          ((Math.max(first, second) + 0.05) /
            (Math.min(first, second) + 0.05)) *
            100,
        ) / 100,
    };
  });
}

/** 源码里 `max-w-[<n>px]` 的命中数：ad-hoc 像素容器只允许少数几处。 */
function adHocContainers(root: string): string[] {
  const hits: string[] = [];
  const walk = (dir: string) => {
    for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
      const full = path.join(dir, entry.name);
      if (entry.isDirectory()) {
        walk(full);
        continue;
      }
      if (!/\.(ts|tsx)$/.test(entry.name)) continue;
      const text = fs.readFileSync(full, "utf8");
      for (const match of text.matchAll(/max-w-\[\d+px\]/g))
        hits.push(`${path.relative(root, full)}: ${match[0]}`);
    }
  };
  walk(root);
  return hits;
}

// ── 断言 ───────────────────────────────────────────────────────────────────

test("壳层宽度：1920 / 1440（A1–A4）", async ({ browser }) => {
  const session = await openSession(browser, {
    viewport: VIEW.wide,
    path: `/spaces/${CHAT_WS}`,
    ready: '[data-testid="space-chat-shell"]',
    preset: { dockOpen: true, railOpen: true },
  });
  const { page } = session;
  try {
    const wide = await until(
      () => shellFacts(page),
      (facts) => near(facts.chat?.width ?? 0, 824),
    );
    check(
      "A1",
      "1920：双开 50/50，聊天列 856 / 聊天壳 824（±1）",
      near(wide.chat?.width, 824),
      true,
      `实测 ${wide.chat?.width}px（rail ${wide.rail?.width}px / 抽屉 ${wide.dock?.width}px / 右栏 ${wide.panel?.width}px）`,
    );

    await page.setViewportSize(VIEW.trio);
    // 抽屉滑入是 120ms 过渡：位移归 0 再量，否则会采到 -61% 这种中间帧。
    const trio = await until(
      () => shellFacts(page),
      (facts) =>
        near(facts.chat?.width ?? 0, 584) &&
        facts.dock?.width === 400 &&
        facts.dockTranslate === 0 &&
        facts.panel !== null,
    );
    check(
      "A2",
      "1440：双开 50/50，聊天壳 584（±1）",
      near(trio.chat?.width, 584),
      true,
      `实测 ${trio.chat?.width}px`,
    );
    check(
      "A3",
      "1440：#peco-dock 宽 400、left = rail 208、translateX 0",
      [trio.dock?.width ?? null, trio.dock?.left ?? null, trio.dockTranslate],
      [400, 208, 0],
      `实测 宽 ${trio.dock?.width}px、left ${trio.dock?.left}px、translateX ${trio.dockTranslate}px`,
    );
    check(
      "A4",
      "1440：右栏占一半 616（含 border-l 落 617）",
      trio.panel?.width ?? null,
      617,
      `实测 ${trio.panel?.width}px`,
    );
  } finally {
    await session.dispose();
  }
});

test("壳层宽度：抽屉收起与 rail 图标轨（A5–A10）", async ({ browser }) => {
  const session = await openSession(browser, {
    viewport: VIEW.dockBar,
    path: `/spaces/${CHAT_WS}`,
    ready: '[data-testid="space-chat-shell"]',
    preset: { dockOpen: false, railOpen: true },
  });
  const { page } = session;
  try {
    const bar = await until(
      () => shellFacts(page),
      (facts) =>
        near(facts.chat?.width ?? 0, 504) &&
        facts.dock !== null &&
        facts.dockTranslate === -400,
    );
    check(
      "A5",
      "1280：抽屉宽 400 且整块移出视口（translateX -400）、#peco-dock-bottom 与 #peco-dock-sheet 都不存在",
      [
        bar.dock?.width ?? null,
        bar.dockTranslate,
        bar.dockBottom,
        bar.dockSheet,
      ],
      [400, -400, null, null],
      `实测 抽屉 ${bar.dock?.width}px、translateX ${bar.dockTranslate}px、底部条 ${bar.dockBottom === null ? "null" : "存在"}、覆盖 Sheet ${bar.dockSheet === null ? "null" : "存在"}`,
    );
    check(
      "A6",
      "1280：聊天壳 504（±1）",
      near(bar.chat?.width, 504),
      true,
      `实测 ${bar.chat?.width}px`,
    );

    await page.setViewportSize({ width: 1200, height: 800 });
    const mid = await until(
      () => shellFacts(page),
      (facts) => near(facts.chat?.width ?? 0, 464),
    );
    check(
      "A7",
      "1200：聊天壳 464（±1）",
      near(mid.chat?.width, 464),
      true,
      `实测 ${mid.chat?.width}px`,
    );

    await page.setViewportSize({ width: 1024, height: 800 });
    const icon = await until(
      () => shellFacts(page),
      (facts) => facts.rail?.width === 64 && near(facts.chat?.width ?? 0, 448),
    );
    check(
      "A8",
      "1024：rail 宽 64 且文档里无「展开左栏」按钮",
      [icon.rail?.width ?? null, icon.expandRail],
      [64, 0],
      `实测 rail ${icon.rail?.width}px、「展开左栏」按钮 ${icon.expandRail} 个`,
    );
    check(
      "A9",
      "1024：聊天壳 448（±1）",
      near(icon.chat?.width, 448),
      true,
      `实测 ${icon.chat?.width}px`,
    );

    await page.setViewportSize(VIEW.narrow);
    const narrow = await until(
      () => shellFacts(page),
      (facts) => near(facts.chat?.width ?? 0, 386),
    );
    check(
      "A10",
      "900：聊天壳 386（±1）",
      near(narrow.chat?.width, 386),
      true,
      `实测 ${narrow.chat?.width}px`,
    );
  } finally {
    await session.dispose();
  }
});

test("820：两块互斥且聊天优先（A11 / F7 / F8）", async ({ browser }) => {
  const session = await openSession(browser, {
    viewport: { width: 820, height: 800 },
    path: `/spaces/${CHAT_WS}`,
    ready: '[data-testid="space-chat-shell"]',
    preset: { dockOpen: false, railOpen: true },
  });
  const { page } = session;
  const focusLabel = () =>
    page.evaluate(
      () =>
        (document.activeElement as HTMLElement | null)?.getAttribute(
          "aria-label",
        ) ??
        (document.activeElement as HTMLElement | null)?.tagName ??
        "（无）",
    );
  const readingWidth = () =>
    page.evaluate(() => {
      const scroller = document.querySelector(
        '[data-testid="space-chat-shell"][data-visible="true"] [data-slot="message-scroller"]',
      );
      const column = scroller?.parentElement ?? null;
      return column ? Math.round(column.getBoundingClientRect().width) : null;
    });
  try {
    const onlyChat = await until(
      () => shellFacts(page),
      (facts) => facts.panel === null && (facts.chat?.width ?? 0) > 0,
    );
    const chatReading = await readingWidth();

    await page.getByRole("button", { name: "展开右栏" }).click();
    const onlyPanel = await until(
      () => shellFacts(page),
      (facts) => facts.panel !== null && (facts.chat?.width ?? 0) === 0,
    );
    const focusOnExpandChat = await focusLabel();

    check(
      "A11",
      "820：默认只有聊天；点「展开右栏」→ 聊天让位、右栏独占整宽（聊天壳 display:none 不占宽）",
      [
        onlyChat.panel,
        onlyChat.chat?.width ?? null,
        onlyPanel.panel?.width ?? null,
        onlyPanel.chat?.width ?? 0,
      ],
      [null, 788, 820, 0],
      `关着时 space-panel ${onlyChat.panel === null ? "null" : "存在"}、聊天壳 ${onlyChat.chat?.width}px；展开后右栏 ${onlyPanel.panel?.width}px、聊天壳 ${onlyPanel.chat?.width ?? 0}px`,
    );
    check(
      "F7",
      "折叠把被点的那枚按钮一起带走：点「展开右栏」后焦点落在对面头部的「展开对话」",
      focusOnExpandChat,
      "展开对话",
    );
    check(
      "F8",
      "820：聊天独占时正文列吃满上限 760（±1）",
      near(chatReading, 760),
      true,
      `实测 ${chatReading}px（列 788 减去 px-4 后仍是 756→上限 760 生效）`,
    );

    await page.getByRole("button", { name: "展开对话" }).click();
    const backToChat = await until(
      () => shellFacts(page),
      (facts) => facts.panel === null && (facts.chat?.width ?? 0) > 0,
    );
    const focusOnExpandPanel = await focusLabel();
    check(
      "F9",
      "820：点「展开对话」反向 —— 右栏收起、聊天独占，且焦点落在「展开右栏」",
      [backToChat.panel, backToChat.chat?.width ?? null, focusOnExpandPanel],
      [null, 788, "展开右栏"],
      `实测 documentOverflowX ${backToChat.documentOverflow}`,
    );
  } finally {
    await session.dispose();
  }
});

test("rail 两态 × 六档：聊天壳恒 ≥380（A12 / A13）", async ({ browser }) => {
  test.setTimeout(180_000);
  const session = await openSession(browser, {
    viewport: VIEW.wide,
    path: `/spaces/${CHAT_WS}`,
    ready: '[data-testid="space-chat-shell"]',
    preset: { dockOpen: true, railOpen: true },
  });
  const { page } = session;
  const measured: { tier: string; rail: string; chat: number | null }[] = [];
  const overflow: string[] = [];
  try {
    for (const tier of TIERS) {
      for (const railOpen of [true, false]) {
        const label = `${tier.width}/${railOpen ? "展开" : "收起"}`;
        await reloadWithPreset(
          page,
          { dockOpen: true, railOpen },
          '[data-testid="space-chat-shell"]',
        );
        await page.setViewportSize(tier);
        const facts = await until(
          () => shellFacts(page),
          (next) => (next.chat?.width ?? 0) > 0 && next.main !== null,
        );
        measured.push({
          tier: `${tier.width}`,
          rail: railOpen ? "展开" : "收起",
          chat: facts.chat?.width ?? null,
        });
        const chat = facts.chat;
        const main = facts.main;
        // 三件事一起判：只看 scrollWidth 会被 flex `min-width:auto` 骗过。
        const clean =
          chat !== null &&
          main !== null &&
          chat.right <= main.right &&
          chat.scrollWidth <= chat.clientWidth + 1 &&
          facts.documentOverflow <= 0;
        if (!clean) overflow.push(label);
      }
    }
  } finally {
    await session.dispose();
  }
  const below = measured
    .filter((row) => (row.chat ?? 0) < 380)
    .map((row) => `${row.tier}${row.rail}=${row.chat}`);
  check(
    "A12",
    "六档 × rail 展开/收起：聊天壳恒 ≥380（列 ≥412）",
    below,
    [],
    `实测 ${measured.map((row) => `${row.tier}${row.rail}:${row.chat}`).join("，")}`,
  );
  check(
    "A13",
    "各档聊天列 right ≤ main.right 且无横向溢出",
    overflow,
    [],
    overflow.length > 0
      ? `越界档位 ${overflow.join("、")}`
      : "12 组量测全部成立",
  );
});

test("字号与排版（B2 / B3 / B4 / B9）", async ({ browser }) => {
  const routes = routesOf([
    "peco",
    "spaces",
    "space",
    "space-status",
    "teams",
    "results",
  ]);
  const session = await openSession(browser, {
    viewport: VIEW.trio,
    path: routes[0].path,
    ready: routes[0].ready,
    preset: { dockOpen: true },
  });
  const { page } = session;
  const sizes = new Set<number>();
  const micro: string[] = [];
  const bodyLines = new Set<string>();
  const weights: string[] = [];
  try {
    for (const route of routes) {
      await page.goto(route.path, { waitUntil: "load" });
      await page.waitForSelector(route.ready, { timeout: 20_000 });
      await settle(page);
      const facts = await fontFacts(page);
      for (const size of facts.sizes) sizes.add(size);
      micro.push(...facts.micro.map((text) => `${route.id}: ${text}`));
      for (const line of facts.bodyLines) bodyLines.add(line);
      weights.push(...facts.weights.map((text) => `${route.id}: ${text}`));
    }
  } finally {
    await session.dispose();
  }
  const list = [...sizes].sort((a, b) => a - b);
  const unexpected = list.filter(
    (size) => ![11, 12, 13, 14, 16, 20].includes(size),
  );
  check(
    "B2",
    "六路由字号集合 ⊆ {11,12,13,14,16,20}（14 是阅读档），无 9/10",
    unexpected,
    [],
    `实测集合 ${list.join(" / ")}`,
  );
  check(
    "B3",
    "11px 节点文本全匹配 ^[\\x20-\\x7E]*$（无 CJK）",
    micro,
    [],
    micro.length > 0 ? micro.join(" | ") : "0 处违规",
  );
  check(
    "B4",
    "13px 正文 line-height = 20px",
    [...bodyLines].sort(),
    ["20px"],
    "只统计 13px 且自带文本的节点",
  );
  check(
    "B9",
    "含 CJK 的节点 fontWeight ∈ {400,500,600}",
    weights,
    [],
    weights.length > 0 ? weights.join(" | ") : "0 处越界",
  );
});

test("h1 唯一性与顶栏标题（B5 / B6 / B7）", async ({ browser }) => {
  const routes = routesOf([
    "peco",
    "peco-config",
    "spaces",
    "spaces-new",
    "space",
    "space-status",
    "teams",
    "team",
    "results",
    "settings",
  ]);
  const session = await openSession(browser, {
    viewport: VIEW.trio,
    path: routes[0].path,
    ready: routes[0].ready,
    preset: { dockOpen: true },
  });
  const { page } = session;
  const counts: Record<string, number> = {};
  /** 1440 时 shell 里没有 header：顶栏只在 <1100 存在。 */
  let bannerAtWide: number | null = null;
  let bannerH1: number | null = null;
  let bannerParagraphs: { text: string; size: string }[] = [];
  let detailH1: string[] | null = null;
  try {
    for (const route of routes) {
      await page.goto(route.path, { waitUntil: "load" });
      await page.waitForSelector(route.ready, { timeout: 20_000 });
      await settle(page, 500);
      counts[route.id] = await page.evaluate(
        () => document.querySelectorAll("h1").length,
      );
      if (route.id === "spaces") {
        bannerAtWide = await page.evaluate(
          () => document.querySelectorAll('[data-slot="topbar"]').length,
        );
        // 窄档才有顶栏：换到 1024 量它的内部构成，量完换回来。
        await page.setViewportSize({ width: 1024, height: 800 });
        await settle(page, 400);
        const header = await page.evaluate(() => {
          const node = document.querySelector('[data-slot="topbar"]');
          return {
            h1: node ? node.querySelectorAll("h1").length : -1,
            paragraphs: node
              ? Array.from(node.querySelectorAll("p")).map((item) => ({
                  text: (item.textContent ?? "").trim().slice(0, 12),
                  size: getComputedStyle(item).fontSize,
                }))
              : [],
          };
        });
        bannerH1 = header.h1;
        bannerParagraphs = header.paragraphs;
        await page.setViewportSize(VIEW.trio);
        await settle(page, 400);
      }
      if (route.id === "space")
        detailH1 = await page.evaluate(() => {
          const node = document.querySelector("h1");
          if (!node) return null;
          const style = getComputedStyle(node);
          return [style.fontSize, style.lineHeight, style.fontWeight];
        });
    }
  } finally {
    await session.dispose();
  }
  const wrong = Object.entries(counts)
    .filter(([, count]) => count !== 1)
    .map(([id, count]) => `${id}=${count}`);
  check(
    "B5",
    "十路由全页 h1 数量 = 1",
    wrong,
    [],
    `实测 ${Object.entries(counts)
      .map(([id, count]) => `${id}:${count}`)
      .join("，")}`,
  );
  const captions = bannerParagraphs.filter((item) => item.size === "12px");
  check(
    "B6",
    "1440 无顶栏；1024 的顶栏内无 h1，标题节点是 12px 的 p",
    [bannerAtWide, bannerH1, captions.length > 0],
    [0, 0, true],
    `实测 header 内 p：${bannerParagraphs
      .map((item) => `${item.text}(${item.size})`)
      .join("，")}`,
  );
  check("B7", "详情页 h1 = 20px / 28px / 600", detailH1, [
    "20px",
    "28px",
    "600",
  ]);
});

test("边框与圆角（C1 / C2 / C3 / C5 / C6 / C8）", async ({ browser }) => {
  const routes = routesOf([
    "peco",
    "spaces",
    "space",
    "space-status",
    "teams",
    "results",
  ]);
  const session = await openSession(browser, {
    viewport: VIEW.trio,
    path: routes[0].path,
    ready: routes[0].ready,
    preset: { dockOpen: true },
  });
  const { page } = session;
  const buttons: string[] = [];
  const radii = new Set<string>();
  let pecoStroked: number | null = null;
  let spacesStroked: number | null = null;
  let bubbleCorners: string[] | null = null;
  let bubbleRadius: string | null = null;
  try {
    for (const route of routes) {
      await page.goto(route.path, { waitUntil: "load" });
      await page.waitForSelector(route.ready, { timeout: 20_000 });
      await settle(page);
      const facts = await borderFacts(page);
      if (route.id === "peco") pecoStroked = facts.stroked;
      if (route.id === "spaces") spacesStroked = facts.stroked;
      buttons.push(
        ...facts.buttonStroked.map((text) => `${route.id}: ${text}`),
      );
      for (const radius of facts.radii) radii.add(radius);
    }
    // 消息气泡：状态页「会话」tab 是种子里唯一有真实转录的地方。
    await page.goto(`/spaces/${CHAT_WS}/status`, { waitUntil: "load" });
    await page.waitForSelector('[data-slot="user-bubble"]', {
      timeout: 20_000,
    });
    await settle(page, 400);
    const found = await page.evaluate(() => {
      const node = document.querySelector('[data-slot="user-bubble"]');
      if (!node) return null;
      const style = getComputedStyle(node);
      return {
        corners: [
          "border-top-left-radius",
          "border-top-right-radius",
          "border-bottom-right-radius",
          "border-bottom-left-radius",
        ].map((name) => style.getPropertyValue(name)),
        uniform: style.borderRadius,
      };
    });
    bubbleCorners = found ? found.corners : null;
    bubbleRadius = found ? found.uniform : null;
  } finally {
    await session.dispose();
  }
  check(
    "C1",
    "/peco 可见描边元素数 ≤45",
    pecoStroked !== null && pecoStroked <= 45,
    true,
    `实测 ${pecoStroked ?? "null"} 处（口径：任一边 width>0、style≠none 且该边颜色不透明）`,
  );
  check(
    "C2",
    "/spaces 可见描边元素数 ≤45",
    spacesStroked !== null && spacesStroked <= 45,
    true,
    `实测 ${spacesStroked ?? "null"} 处（列表行 border-b 与状态链是合法分区线）`,
  );
  check(
    "C3",
    "非选中态 button 上可见描边数 = 0（排除 select 触发器；选中/展开态圆角矩形描边允许）",
    buttons,
    [],
    buttons.length > 0
      ? buttons.join(" | ")
      : "六路由非选中态 0 处（选中态圆角矩形描边允许）",
  );
  const unexpected = [...radii].filter((radius) => !allowedRadius(radius));
  check(
    "C5",
    "全站 borderRadius 去重 ⊆ {0px,1px,4px,6px,full}",
    unexpected,
    [],
    `实测 ${[...radii].sort().join(" / ")}`,
  );
  check(
    "C6",
    "无 8px / 16px 圆角",
    [...radii].filter((radius) => radius === "8px" || radius === "16px"),
    [],
  );
  check(
    "C8",
    "消息气泡 borderRadius = 6px 且四角一致",
    bubbleCorners,
    ["6px", "6px", "6px", "6px"],
    `computed borderRadius=${bubbleRadius ?? "null"}`,
  );
});

test("点击区：九路由无 <24 命中项（D1）", async ({ browser }) => {
  const routes = routesOf([
    "peco",
    "spaces",
    "spaces-new",
    "space",
    "space-status",
    "teams",
    "team",
    "results",
    "settings",
  ]);
  const session = await openSession(browser, {
    viewport: VIEW.trio,
    path: routes[0].path,
    ready: routes[0].ready,
    preset: { dockOpen: true },
  });
  const { page } = session;
  const violations: string[] = [];
  try {
    for (const route of routes) {
      await page.goto(route.path, { waitUntil: "load" });
      await page.waitForSelector(route.ready, { timeout: 20_000 });
      await settle(page);
      const short = await shortTargets(page);
      violations.push(
        ...short.map(
          (item) => `${route.id}: ${item.label}（${item.height}px 高）`,
        ),
      );
    }
  } finally {
    await session.dispose();
  }
  check(
    "D1",
    "九路由 button/a[href]/[role=button]/[role=tab] 可见元素高 ≥24，违规数 = 0",
    violations,
    [],
    violations.length > 0
      ? violations.join(" | ")
      : "九路由 0 处（跳过 sr-only 的「跳到内容」）",
  );
});

test("点击区：历史违规项逐个复测（D2 / D3）", async ({ browser }) => {
  const session = await openSession(browser, {
    viewport: VIEW.trio,
    path: `/spaces/${CHAT_WS}`,
    ready: '[data-testid="space-chat-shell"]',
    // 选中另一个空间：页头才会出现「非当前空间 / 设为当前」两枚按钮。
    preset: { dockOpen: true, activeWsId: "ws_demo_finance" },
  });
  const { page } = session;
  let heights: Record<string, number | null> = {};
  try {
    heights = await namedHeights(page, [
      {
        label: "查看全部",
        path: "/peco",
        ready: '[data-testid="peco-panel"]',
        role: "link",
        name: /^查看全部/,
      },
      {
        label: "全部团队",
        path: "/peco",
        ready: '[data-testid="peco-panel"]',
        role: "link",
        name: /^全部团队/,
      },
      {
        label: "根目录",
        path: `/spaces/${CHAT_WS}/status?tab=files`,
        ready: '[role="tablist"]',
        role: "button",
        name: "根目录",
      },
      {
        label: "进入聊天",
        path: `/spaces/${CHAT_WS}/status?tab=files`,
        ready: '[role="tablist"]',
        role: "link",
        name: "进入聊天",
      },
      {
        label: "复制",
        path: `/spaces/${CHAT_WS}/status`,
        ready: '[role="tablist"]',
        role: "button",
        name: "复制",
      },
      {
        label: "团队 id",
        // 聊天页头部只剩图标动作，带「团队 <id>」文案的链接在状态页那一份页头里。
        path: `/spaces/${CHAT_WS}/status`,
        ready: '[role="tablist"]',
        role: "link",
        // 左栏那行的可访问名是「团队 3」（计数拼进去），只认团队 id 的小写字母。
        name: /^团队 [a-z]+$/,
      },
      {
        label: "非当前空间",
        path: `/spaces/${CHAT_WS}/status`,
        ready: '[role="tablist"]',
        role: "button",
        name: "非当前空间",
      },
      {
        label: "设为当前",
        path: `/spaces/${CHAT_WS}/status`,
        ready: '[role="tablist"]',
        role: "button",
        name: "设为当前",
      },
      {
        label: "新建空间",
        path: "/spaces",
        ready: "main h1",
        role: "link",
        name: "新建空间",
      },
      {
        label: "开始对话",
        path: `/spaces/${emptySessionWs}/status`,
        ready: '[role="tablist"]',
        role: "button",
        name: "开始对话",
      },
    ]);
  } finally {
    await session.dispose();
  }
  const short = Object.entries(heights)
    .filter(([, height]) => height === null || height < 24)
    .map(
      ([label, height]) =>
        `${label}=${height === null ? "未找到" : `${height}px`}`,
    );
  check(
    "D3",
    "10 处历史违规项 rect.height ≥24",
    short,
    [],
    `实测 ${Object.entries(heights)
      .map(([label, height]) => `${label}:${height ?? "null"}px`)
      .join("，")}`,
  );
  const primary = ["新建空间", "开始对话"]
    .filter((label) => (heights[label] ?? 0) < 32)
    .map((label) => `${label}=${heights[label] ?? "未找到"}`);
  check(
    "D2",
    "主操作 rect.height ≥32（/spaces 的「新建空间」与空会话的「开始对话」）",
    primary,
    [],
    `实测 新建空间 ${heights["新建空间"] ?? "null"}px、开始对话 ${heights["开始对话"] ?? "null"}px`,
  );
});

test("点击区：触摸档命中区 ≥44（D5）", async ({ browser }) => {
  const session = await openSession(browser, {
    viewport: VIEW.trio,
    path: `/spaces/${CHAT_WS}`,
    ready: '[data-testid="space-chat-shell"]',
    hasTouch: true,
    preset: { dockOpen: true, activeWsId: "ws_demo_finance" },
  });
  const { page } = session;
  let heights: Record<string, number | null> = {};
  let coarse = false;
  try {
    coarse = await page.evaluate(
      () => window.matchMedia("(pointer: coarse)").matches,
    );
    heights = await namedHeights(page, [
      {
        label: "查看全部",
        path: "/peco",
        ready: '[data-testid="peco-panel"]',
        role: "link",
        name: /^查看全部/,
      },
      {
        label: "全部团队",
        path: "/peco",
        ready: '[data-testid="peco-panel"]',
        role: "link",
        name: /^全部团队/,
      },
      {
        label: "根目录",
        path: `/spaces/${CHAT_WS}/status?tab=files`,
        ready: '[role="tablist"]',
        role: "button",
        name: "根目录",
      },
      {
        label: "进入聊天",
        path: `/spaces/${CHAT_WS}/status?tab=files`,
        ready: '[role="tablist"]',
        role: "link",
        name: "进入聊天",
      },
      {
        label: "复制",
        path: `/spaces/${CHAT_WS}/status`,
        ready: '[role="tablist"]',
        role: "button",
        name: "复制",
      },
      {
        label: "团队 id",
        // 聊天页头部只剩图标动作，带「团队 <id>」文案的链接在状态页那一份页头里。
        path: `/spaces/${CHAT_WS}/status`,
        ready: '[role="tablist"]',
        role: "link",
        // 左栏那行的可访问名是「团队 3」（计数拼进去），只认团队 id 的小写字母。
        name: /^团队 [a-z]+$/,
      },
      {
        label: "非当前空间",
        path: `/spaces/${CHAT_WS}/status`,
        ready: '[role="tablist"]',
        role: "button",
        name: "非当前空间",
      },
      {
        label: "设为当前",
        path: `/spaces/${CHAT_WS}/status`,
        ready: '[role="tablist"]',
        role: "button",
        name: "设为当前",
      },
      {
        label: "新建空间",
        path: "/spaces",
        ready: "main h1",
        role: "link",
        name: "新建空间",
      },
      {
        label: "开始对话",
        path: `/spaces/${emptySessionWs}/status`,
        ready: '[role="tablist"]',
        role: "button",
        name: "开始对话",
      },
    ]);
  } finally {
    await session.dispose();
  }
  const short = Object.entries(heights)
    .filter(([, height]) => height === null || height < 44)
    .map(
      ([label, height]) =>
        `${label}=${height === null ? "未找到" : `${height}px`}`,
    );
  check(
    "D5",
    "hasTouch context 下同一批元素 rect.height ≥44",
    short,
    [],
    `pointer:coarse=${coarse}；实测 ${Object.entries(heights)
      .map(([label, height]) => `${label}:${height ?? "null"}px`)
      .join("，")}`,
  );
});

test("容器宽度（E2–E8）", async ({ browser }) => {
  const sourceRoot = path.join(process.cwd(), "src");
  const adHoc = adHocContainers(sourceRoot);
  check(
    "E2",
    "源码 max-w-[<n>px] 命中 ≤8（max-h 不计）",
    adHoc.length <= 8,
    true,
    `实测 ${adHoc.length} 处：${adHoc.join("，")}`,
  );

  const session = await openSession(browser, {
    viewport: VIEW.wide,
    path: "/settings",
    ready: "main h1",
    preset: { dockOpen: true },
  });
  const { page } = session;
  try {
    await settle(page);
    const settings = await containerWidth(page);
    check(
      "E3",
      "/settings 内容宽 880（±1）",
      near(settings, 880),
      true,
      `实测 ${settings}px`,
    );

    await page.goto("/spaces/new", { waitUntil: "load" });
    await page.waitForSelector("main h1", { timeout: 20_000 });
    await settle(page);
    const created = await page.evaluate(() => {
      const root = document.querySelector("main > div > *");
      const paragraph = document.querySelector("main p");
      return {
        container: root ? Math.round(root.getBoundingClientRect().width) : null,
        paragraph: paragraph
          ? Math.round(paragraph.getBoundingClientRect().width)
          : null,
      };
    });
    check(
      "E4",
      "/spaces/new 容器 880（±1）且正文段 520（±1）",
      [near(created.container, 880), near(created.paragraph, 520)],
      [true, true],
      `实测 容器 ${created.container}px、正文段 ${created.paragraph}px`,
    );

    const wide: Record<string, number | null> = {};
    for (const route of routesOf(["results", "spaces", "teams"])) {
      await page.goto(route.path, { waitUntil: "load" });
      await page.waitForSelector(route.ready, { timeout: 20_000 });
      await settle(page);
      wide[route.id] = await containerWidth(page);
    }
    check(
      "E5",
      "/results 容器 @1920 = 1200（±1）",
      near(wide.results, 1200),
      true,
      `实测 ${wide.results}px`,
    );
    check(
      "E6",
      "/spaces 与 /teams 容器 @1920 = 1200（±1）",
      [near(wide.spaces, 1200), near(wide.teams, 1200)],
      [true, true],
      `实测 /spaces ${wide.spaces}px、/teams ${wide.teams}px`,
    );

    await page.goto(`/spaces/${CHAT_WS}`, { waitUntil: "load" });
    await page.waitForSelector('[data-slot="message-scroller"]', {
      timeout: 20_000,
    });
    await settle(page);
    const reading = await page.evaluate(() => {
      const scroller = document.querySelector(
        '[data-testid="space-chat-shell"][data-visible="true"] [data-slot="message-scroller"]',
      );
      const column = scroller?.parentElement ?? null;
      return column ? Math.round(column.getBoundingClientRect().width) : null;
    });
    check(
      "E7",
      "聊天正文容器 @1920 = 760（±1）",
      near(reading, 760),
      true,
      `实测 ${reading}px（[data-slot=message-scroller] 的父节点，即 max-w-chat 阅读列）`,
    );

    await page.goto("/schedules", { waitUntil: "load" });
    await page.waitForSelector("main h1", { timeout: 20_000 });
    await settle(page);
    const paragraphs = await page.evaluate(() =>
      Array.from(document.querySelectorAll("main p")).map((node) =>
        Math.round(node.getBoundingClientRect().width),
      ),
    );
    check(
      "E8",
      "空态段落宽 ≤520",
      paragraphs.filter((width) => width > 520),
      [],
      `实测 /schedules 段落宽 ${paragraphs.join(" / ")}px`,
    );
  } finally {
    await session.dispose();
  }
});

test("右栏模式进 URL 且 URL 优先于存档（F1 / F2）", async ({ browser }) => {
  const panelSelector = '[data-testid="space-panel"]';
  const session = await openSession(browser, {
    viewport: VIEW.trio,
    path: `/spaces/${CHAT_WS}`,
    ready: panelSelector,
    preset: {
      dockOpen: true,
      panels: { [CHAT_WS]: { mode: "files", collapsed: false } },
    },
  });
  const { page } = session;
  try {
    await page.getByRole("tab", { name: "任务", exact: true }).click();
    const search = await until(
      () => page.evaluate(() => location.search),
      (value) => value.includes("panel=tasks"),
    );
    const clicked = await page
      .getByTestId("space-panel")
      .getAttribute("data-mode");

    await page.goto(`/spaces/${CHAT_WS}?panel=memory`, { waitUntil: "load" });
    await page.waitForSelector(panelSelector, { timeout: 20_000 });
    const memory = await until(
      () => page.getByTestId("space-panel").getAttribute("data-mode"),
      (value) => value === "memory",
    );
    check(
      "F1",
      "点「任务」后 location.search 含 panel=tasks；直连 ?panel=memory 右栏为记忆",
      [search.includes("panel=tasks"), clicked, memory],
      [true, "tasks", "memory"],
      `点击后 search=${search}`,
    );

    await reloadWithPreset(
      page,
      {
        dockOpen: true,
        panels: { [CHAT_WS]: { mode: "files", collapsed: false } },
      },
      '[data-testid="space-chat-shell"]',
    );
    await page.goto(`/spaces/${CHAT_WS}?panel=tasks`, { waitUntil: "load" });
    await page.waitForSelector(panelSelector, { timeout: 20_000 });
    const fromUrl = await until(
      () => page.getByTestId("space-panel").getAttribute("data-mode"),
      (value) => value === "tasks",
    );
    const stored = await page.evaluate(
      (key) => localStorage.getItem(key),
      `peco-webui:space-panel:${CHAT_WS}`,
    );
    check(
      "F2",
      "localStorage 存 files + URL 给 tasks → 渲染任务（URL 赢）",
      fromUrl,
      "tasks",
      `存档 ${stored}`,
    );
  } finally {
    await session.dispose();
  }
});

test("滚动位置保持（F3 / F4）", async ({ browser }) => {
  const session = await openSession(browser, {
    // 空间会话是 ensure 新建的空会话（种子的转录挂在 agent 级会话上），
    // 矮视口把欢迎块变成可滚的，基线才是真的滚出来的位置：360 时正文比可视区高 59px。
    viewport: { width: VIEW.trio.width, height: 360 },
    path: `/spaces/${CHAT_WS}`,
    ready: '[data-slot="message-scroller"]',
    preset: { dockOpen: true, railOpen: true },
  });
  const { page } = session;
  const chatScroller =
    '[data-testid="space-chat-shell"][data-visible="true"] [data-slot="message-scroller"]';
  const statusScroller = 'main [data-slot="message-scroller"]';
  try {
    const base = await scrollTo(page, chatScroller, 40);
    await page.getByRole("tab", { name: "记忆", exact: true }).click();
    await page.waitForTimeout(300);
    const afterMode = await scrollTopOf(page, chatScroller);
    check(
      "F3",
      "切右栏模式前后聊天滚动位置差 0",
      [afterMode - base, base > 0],
      [0, true],
      `操作前 ${base}px、操作后 ${afterMode}px（口径：[data-slot=message-scroller]，基线须 >0 才算差测成立）`,
    );

    // F4 用状态页的「会话」tab：那里有种子转录，能滚到 600。
    await page.setViewportSize({ width: VIEW.trio.width, height: 600 });
    await page.goto(`/spaces/${CHAT_WS}/status`, { waitUntil: "load" });
    await page.waitForSelector('[data-slot="user-bubble"]', {
      timeout: 20_000,
    });
    await settle(page, 400);
    const before = await scrollTo(page, statusScroller, 600);
    // 宽屏的抽屉开关是左栏品牌行那枚按钮（顶栏在 ≥1100 已不存在）。
    await page.getByRole("button", { name: "Peco 对话", exact: true }).click();
    await page.waitForTimeout(300);
    const afterDock = await scrollTopOf(page, statusScroller);
    check(
      "F4",
      "收起 Peco 抽屉前后聊天滚动位置差 0",
      [afterDock - before, before >= 300],
      [0, true],
      `操作前 ${before}px、操作后 ${afterDock}px（口径：main 内的 [data-slot=message-scroller]）`,
    );
  } finally {
    await session.dispose();
  }
});

test("抽屉覆盖不占布局（F5）", async ({ browser }) => {
  const session = await openSession(browser, {
    viewport: VIEW.dockBar,
    path: `/spaces/${CHAT_WS}`,
    ready: '[data-testid="space-chat-shell"]',
    preset: { dockOpen: false, railOpen: true },
  });
  const { page } = session;
  try {
    const closed = await until(
      () => shellFacts(page),
      (facts) => facts.dock !== null && facts.main !== null,
    );
    await page.getByRole("button", { name: "Peco 对话", exact: true }).click();
    const opened = await until(
      () => shellFacts(page),
      (facts) => facts.dockTranslate === 0,
    );
    const delta = Math.abs(
      (opened.main?.height ?? 0) - (closed.main?.height ?? 0),
    );
    check(
      "F5",
      "1280：抽屉开合后 #main 高度不变（覆盖层，不占布局）",
      delta,
      0,
      `实测 main ${closed.main?.height}px → ${opened.main?.height}px（差 ${delta}px），抽屉 translateX ${closed.dockTranslate}px → ${opened.dockTranslate}px`,
    );
  } finally {
    await session.dispose();
  }
});

test("焦点环与跳过导航（F10 / F11）", async ({ browser }) => {
  const session = await openSession(browser, {
    viewport: VIEW.trio,
    path: "/spaces",
    ready: "main h1",
    preset: { dockOpen: true },
  });
  const { page } = session;
  try {
    const skip = await page.evaluate(() => {
      const link = document.querySelector('a[href="#main"]');
      const main = document.getElementById("main");
      return {
        link: Boolean(link),
        tabindex: main?.getAttribute("tabindex") ?? null,
      };
    });
    check(
      "F11",
      '存在 href="#main" 的 skip link，且 #main 有 tabindex="-1"',
      [skip.link, skip.tabindex],
      [true, "-1"],
    );

    let tag = "";
    for (let step = 0; step < 40; step += 1) {
      await page.keyboard.press("Tab");
      tag = await page.evaluate(() => document.activeElement?.tagName ?? "");
      if (tag === "BUTTON") break;
    }
    const ring = await page.evaluate(() => {
      const active = document.activeElement as HTMLElement | null;
      if (!active) return null;
      const style = getComputedStyle(active);
      return {
        tag: active.tagName,
        label: (active.getAttribute("aria-label") ?? active.textContent ?? "")
          .trim()
          .slice(0, 14),
        width: style.outlineWidth,
        offset: style.outlineOffset,
        color: style.outlineColor,
      };
    });
    check(
      "F10",
      "Tab 到按钮时 outline-width = 2px、outline-offset = 2px",
      [ring?.width ?? null, ring?.offset ?? null],
      ["2px", "2px"],
      `焦点在 ${ring?.tag ?? "null"} ${ring?.label ?? ""}，outline ${ring?.width} ${ring?.color} / offset ${ring?.offset}`,
    );
  } finally {
    await session.dispose();
  }
});

test("对比度与主题（G1–G4）", async ({ browser }) => {
  const routes = routesOf([
    "peco",
    "spaces",
    "space",
    "space-status",
    "teams",
    "results",
  ]);
  const dark = await openSession(browser, {
    viewport: VIEW.trio,
    path: "/spaces",
    ready: "main h1",
    preset: { dockOpen: true, theme: "dark" },
  });
  let primaryDark: PrimaryColors | null = null;
  let scheme: { colorScheme: string; themeColor: string | null } | null = null;
  const fails: string[] = [];
  try {
    await settle(dark.page);
    primaryDark = await primaryContrast(dark.page);
    scheme = await dark.page.evaluate(() => ({
      colorScheme: getComputedStyle(document.documentElement).colorScheme,
      themeColor:
        document
          .querySelector('meta[name="theme-color"]')
          ?.getAttribute("content") ?? null,
    }));
    for (const route of routes) {
      await dark.page.goto(route.path, { waitUntil: "load" });
      await dark.page.waitForSelector(route.ready, { timeout: 20_000 });
      await settle(dark.page);
      const facts = await contrastFacts(dark.page);
      if (facts.fails > 0)
        fails.push(
          `${route.id}: ${facts.fails} 处（最差「${facts.worst[0]?.text}」${facts.worst[0]?.ratio}）`,
        );
    }
  } finally {
    await dark.dispose();
  }

  const light = await openSession(browser, {
    viewport: VIEW.trio,
    path: "/spaces",
    ready: "main h1",
    preset: { dockOpen: true, theme: "light" },
  });
  let primaryLight: PrimaryColors | null = null;
  try {
    await settle(light.page);
    primaryLight = await primaryContrast(light.page);
  } finally {
    await light.dispose();
  }

  check(
    "G1",
    "主按钮白字对比度：暗色 ≥5.1、亮色 ≥6.0",
    [
      primaryDark ? primaryDark.ratio >= 5.1 : null,
      primaryLight ? primaryLight.ratio >= 6.0 : null,
    ],
    [true, true],
    `暗 ${primaryDark?.fg} on ${primaryDark?.bg} = ${primaryDark?.ratio}；亮 ${primaryLight?.fg} on ${primaryLight?.bg} = ${primaryLight?.ratio}`,
  );
  check(
    "G2",
    "bg-primary 暗色计算色 = rgb(90,94,221)",
    primaryDark?.bg ?? null,
    "rgb(90,94,221)",
    `实测 ${primaryDark?.bg ?? "null"}（对比度 ${primaryDark?.ratio}）`,
  );
  check(
    "G3",
    "六路由对比度失败项 = 0（沿用 audit 的算法）",
    fails,
    [],
    fails.length > 0 ? fails.join(" | ") : "六路由 0 处",
  );
  check(
    "G4",
    "html computed color-scheme = dark 且 meta[name=theme-color] = #090a0f",
    [scheme?.colorScheme ?? null, scheme?.themeColor ?? null],
    ["dark", "#090a0f"],
    `实测 color-scheme=${scheme?.colorScheme ?? "null"}、theme-color=${scheme?.themeColor ?? "null"}`,
  );
});

/** 汇总放最后一条 test：读落盘的行而不是内存数组 —— 断言失败会换 worker 重新导入本文件。 */
test("汇总表", () => {
  const collected: Row[] = fs.existsSync(ROWS_FILE)
    ? fs
        .readFileSync(ROWS_FILE, "utf8")
        .split("\n")
        .filter(Boolean)
        .map((line) => JSON.parse(line) as Row)
    : [];
  const byId = new Map<string, Row>();
  for (const row of collected) byId.set(row.id, row);
  const extraIds = [...byId.keys()].filter((id) => !EXPECTED_IDS.includes(id));
  const ordered = [...EXPECTED_IDS, ...extraIds]
    .map((id) => byId.get(id))
    .filter((row): row is Row => row !== undefined);

  console.log("\n===== 实测汇总 =====");
  console.log("| # | 期望 | 实测 | 判定 |");
  console.log("|---|---|---|---|");
  for (const row of ordered) {
    const note = row.note ? `（${row.note}）` : "";
    console.log(
      `| ${row.id} | ${row.expectation} | ${row.actual} | ${row.verdict}${note} |`,
    );
  }
  const failed = ordered
    .filter((row) => row.verdict === "FAIL")
    .map((row) => row.id);
  const missing = EXPECTED_IDS.filter((id) => !byId.has(id));
  console.log(
    `\n合计 ${ordered.length} 条：PASS ${ordered.length - failed.length} / FAIL ${failed.length}` +
      (failed.length > 0 ? `；FAIL ${failed.join("、")}` : "") +
      (missing.length > 0 ? `；缺少 ${missing.join("、")}` : ""),
  );
  expect(missing, "有断言没产出实测值").toEqual([]);
  expect(failed, "实测与规格不符的行").toEqual([]);
});
