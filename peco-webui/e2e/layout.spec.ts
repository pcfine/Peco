// [浏览器] 档断言：全部在真实 Chromium（headless:false，X 下）里量尺寸 / 颜色 / 滚动。
//
// 每条断言都打印「期望 vs 实测」，并在末尾汇总成表。测量口径固定为：1920×1080、
// 暗色主题、rail 展开；dock 的开合是断言的一部分，用 localStorage 预置
// （首次进入时 1920 视口默认开 dock，无法在不渲染的情况下点掉它）。
//
// 这里只测量，不改期望值：实测不符就记 FAIL，交给规格裁决。

import fs from "node:fs";
import path from "node:path";
import { test, expect, type Page } from "@playwright/test";

const SESSION_WS = "ws_demo_legal";
const BOARD_READY = ".board-grid";

type Verdict = "PASS" | "FAIL" | "SKIPPED";

interface Row {
  id: string;
  expectation: string;
  actual: string;
  verdict: Verdict;
  note?: string;
}

const rows: Row[] = [];

/** 每个 worker 组各有自己的模块实例（`test.use` 会切组），行要落盘才能跨组汇总。 */
const ROWS_FILE = path.join(process.cwd(), "e2e", ".report", "rows.jsonl");

function record(row: Row): void {
  rows.push(row);
  fs.mkdirSync(path.dirname(ROWS_FILE), { recursive: true });
  fs.appendFileSync(ROWS_FILE, `${JSON.stringify(row)}\n`);
  const note = row.note ? ` | ${row.note}` : "";
  console.log(
    `\n[${row.id}] 期望 ${row.expectation}\n     实测 ${row.actual}\n     判定 ${row.verdict}${note}`,
  );
}

/** 断言失败即「测量得到偏差」，与规格门禁无关：用它让 Playwright 的 pass/fail 与判定一致。 */
function assertVerdict(row: Row): void {
  expect
    .soft(row.verdict, `${row.id} 期望 ${row.expectation} / 实测 ${row.actual}`)
    .toBe("PASS");
}

interface BootOptions {
  dock: "open" | "closed";
  /** 走 `prefers-reduced-motion: reduce` 的上下文只有 A23 用。 */
  reduceMotion?: boolean;
}

/** 每次导航前预置 UI 态，并收齐白屏类故障（产品崩溃在这里表现为空 body）。 */
async function boot(
  page: Page,
  options: BootOptions,
  path = "/",
): Promise<string[]> {
  const errors: string[] = [];
  page.on("pageerror", (error) => errors.push(error.message.split("\n")[0]));
  await page.addInitScript(
    ({ dock }) => {
      localStorage.setItem(
        "peco-webui:dock",
        JSON.stringify({
          state: { open: dock === "open", tab: "chat", conversationByWs: {} },
          version: 0,
        }),
      );
      localStorage.setItem(
        "peco-webui:rail",
        JSON.stringify({ state: { railOpen: true }, version: 0 }),
      );
      // 暗色是默认值，显式落盘一次，避免机器上的偏好影响底色。
      localStorage.setItem(
        "peco-webui:theme",
        JSON.stringify({ state: { theme: "dark" }, version: 0 }),
      );
    },
    { dock: options.dock },
  );
  await page.goto(path, { waitUntil: "load" });
  return errors;
}

async function waitForBoard(page: Page): Promise<boolean> {
  try {
    await page.waitForSelector(BOARD_READY, { timeout: 15_000 });
    return true;
  } catch {
    return false;
  }
}

/** 白屏是异步发生的（dock 先把对话 id 拉回来再挂 ChatView），点击/导航后要再确认一次。 */
async function stillMounted(page: Page): Promise<boolean> {
  await page.waitForTimeout(1200);
  return page.evaluate(() =>
    Boolean(document.querySelector("main")?.firstElementChild),
  );
}

/** 一屏之内的几何与计算样式：断言之间共享同一次 evaluate，避免读两次不同帧。 */
interface Snapshot {
  rail: Box | null;
  main: Box;
  inner: Box;
  innerScroll: {
    scrollWidth: number;
    clientWidth: number;
    scrollHeight: number;
    clientHeight: number;
  };
  dock: Box | null;
  grid: { box: Box; columns: string[]; borderRadius: string };
  panels: PanelSnapshot[];
  bodyBg: string;
  viewport: { width: number; height: number };
}

interface Box {
  x: number;
  y: number;
  width: number;
  height: number;
  right: number;
  bottom: number;
}

interface PanelSnapshot {
  id: string;
  box: Box;
  surface: { boxShadow: string; borderRadius: string; backgroundColor: string };
  gridColumnStart: string;
  gridColumnEnd: string;
  /** 由几何反推的起始列号：计算值可能是 `span 7`（没有显式起点），断言要看落点。 */
  resolvedColumn: number;
}

async function snapshot(page: Page): Promise<Snapshot | null> {
  return page.evaluate(() => {
    const box = (element: Element): Box => {
      const rect = element.getBoundingClientRect();
      return {
        x: rect.x,
        y: rect.y,
        width: rect.width,
        height: rect.height,
        right: rect.right,
        bottom: rect.bottom,
      };
    };
    const main = document.querySelector("main");
    const gridElement = document.querySelector(".board-grid");
    if (!main || !main.firstElementChild || !gridElement) return null;
    const inner = main.firstElementChild as HTMLElement;
    const gridStyle = getComputedStyle(gridElement);
    const innerBox = box(inner);
    // board-grid 有 1px border，内容起点即第一列起点。
    const trackStart = innerBox.x + 1;
    const trackWidth = parseFloat(gridStyle.gridTemplateColumns.split(" ")[0]);
    const panels: PanelSnapshot[] = Array.from(
      document.querySelectorAll('[data-testid^="panel-"]'),
    ).map((element) => {
      const surfaceElement = element.querySelector("section") ?? element;
      const surfaceStyle = getComputedStyle(surfaceElement);
      const elementStyle = getComputedStyle(element);
      const panelBox = box(element);
      return {
        id: (element as HTMLElement).dataset.testid ?? "",
        box: panelBox,
        surface: {
          boxShadow: surfaceStyle.boxShadow,
          borderRadius: surfaceStyle.borderRadius,
          backgroundColor: surfaceStyle.backgroundColor,
        },
        gridColumnStart: elementStyle.gridColumnStart,
        gridColumnEnd: elementStyle.gridColumnEnd,
        resolvedColumn:
          Math.round((panelBox.x - trackStart) / (trackWidth + 1)) + 1,
      };
    });
    const dock = document.querySelector("#peco-dock");
    const rail = document.querySelector("aside");
    return {
      rail: rail ? box(rail) : null,
      main: box(main),
      inner: innerBox,
      innerScroll: {
        scrollWidth: inner.scrollWidth,
        clientWidth: inner.clientWidth,
        scrollHeight: inner.scrollHeight,
        clientHeight: inner.clientHeight,
      },
      dock: dock ? box(dock) : null,
      grid: {
        box: box(gridElement),
        columns: gridStyle.gridTemplateColumns.split(" "),
        borderRadius: gridStyle.borderRadius,
      },
      panels,
      bodyBg: getComputedStyle(document.body).backgroundColor,
      viewport: { width: window.innerWidth, height: window.innerHeight },
    };
  });
}

function readColor(rgb: string): string {
  const match = rgb.match(/rgba?\(([^)]+)\)/);
  if (!match) return rgb;
  const [r, g, b] = match[1].split(",").map((part) => Number(part.trim()));
  return `#${[r, g, b]
    .map((value) => value.toString(16).padStart(2, "0"))
    .join("")
    .toUpperCase()}`;
}

const SKIP = "白屏：dock 里的 ChatView 渲染即抛错，整棵 React 树被卸载";

test.describe("1920×1080，dock 关，rail 展开", () => {
  test("A1 main-inner 宽度", async ({ page }) => {
    const errors = await boot(page, { dock: "closed" });
    const ok = await waitForBoard(page);
    const row: Row = {
      id: "A1",
      expectation: "main-inner 宽度 1616（±1）",
      actual: ok ? "" : `页面未渲染（${errors.join(" / ") || "无 main"}）`,
      verdict: "FAIL",
    };
    if (ok) {
      const shot = await snapshot(page);
      const delta = Math.abs(shot.inner.width - 1616);
      row.actual = `${shot.inner.width.toFixed(1)}（rail ${shot.rail?.width.toFixed(1)}，main ${shot.main.width.toFixed(1)}，padding ${((shot.main.width - shot.inner.width) / 2).toFixed(0)}/侧）`;
      row.verdict = delta <= 1 ? "PASS" : "FAIL";
      row.note =
        delta <= 1
          ? undefined
          : `实测比规格宽 ${(shot.inner.width - 1616).toFixed(1)}：rail 实测 ${shot.rail?.width.toFixed(1)} ≠ 规格 256`;
    }
    record(row);
    assertVerdict(row);
  });

  test("A3 看板列数（dock 关）", async ({ page }) => {
    const errors = await boot(page, { dock: "closed" });
    const ok = await waitForBoard(page);
    const row: Row = {
      id: "A3(dock 关)",
      expectation: "gridTemplateColumns 列数 12",
      actual: ok ? "" : `页面未渲染（${errors.join(" / ") || "无 main"}）`,
      verdict: "FAIL",
    };
    if (ok) {
      const shot = await snapshot(page);
      row.actual = `${shot.grid.columns.length} 列（main-inner ${shot.inner.width.toFixed(1)}）`;
      row.verdict = shot.grid.columns.length === 12 ? "PASS" : "FAIL";
    }
    record(row);
    assertVerdict(row);
  });

  test("A4 今日任务面板跨列（dock 关）", async ({ page }) => {
    const errors = await boot(page, { dock: "closed" });
    const ok = await waitForBoard(page);
    const row: Row = {
      id: "A4(dock 关)",
      expectation: "今日任务 gridColumnStart=1、span 7",
      actual: ok ? "" : `页面未渲染（${errors.join(" / ") || "无 main"}）`,
      verdict: "FAIL",
    };
    if (ok) {
      const shot = await snapshot(page);
      const today = shot.panels.find((panel) => panel.id === "panel-today");
      const span = today?.gridColumnStart ?? "";
      row.actual = today
        ? `gridColumnStart=${today.gridColumnStart}、gridColumnEnd=${today.gridColumnEnd}、几何落点第 ${today.resolvedColumn} 列`
        : "找不到 panel-today";
      row.verdict =
        span === "span 7" && today?.resolvedColumn === 1 ? "PASS" : "FAIL";
      row.note =
        today && today.gridColumnStart !== "1"
          ? "计算值不是「1」而是 span 写法（未显式声明起点），按几何落点判"
          : undefined;
    }
    record(row);
    assertVerdict(row);
  });

  test("A5 相邻面板共享 1px 线", async ({ page }) => {
    const errors = await boot(page, { dock: "closed" });
    const ok = await waitForBoard(page);
    const row: Row = {
      id: "A5",
      expectation: "今日任务.right − 空间状态.left = −1（±0.5）",
      actual: ok ? "" : `页面未渲染（${errors.join(" / ") || "无 main"}）`,
      verdict: "FAIL",
    };
    if (ok) {
      const shot = await snapshot(page);
      const today = shot.panels.find((panel) => panel.id === "panel-today");
      const spaces = shot.panels.find((panel) => panel.id === "panel-spaces");
      if (today && spaces) {
        const gap = today.box.right - spaces.box.x;
        row.actual = `${gap.toFixed(1)}（${today.box.right.toFixed(1)} − ${spaces.box.x.toFixed(1)}）`;
        row.verdict = Math.abs(gap + 1) <= 0.5 ? "PASS" : "FAIL";
      } else {
        row.actual = "找不到 panel-today / panel-spaces";
      }
    }
    record(row);
    assertVerdict(row);
  });

  test("A6 面板无阴影无圆角", async ({ page }) => {
    const errors = await boot(page, { dock: "closed" });
    const ok = await waitForBoard(page);
    const row: Row = {
      id: "A6",
      expectation:
        "面板 boxShadow=none、borderRadius=0px；圆角只在看板容器 8px",
      actual: ok ? "" : `页面未渲染（${errors.join(" / ") || "无 main"}）`,
      verdict: "FAIL",
    };
    if (ok) {
      const shot = await snapshot(page);
      const bad = shot.panels.filter(
        (panel) =>
          panel.surface.boxShadow !== "none" ||
          panel.surface.borderRadius !== "0px",
      );
      row.actual = `面板 ${shot.panels.length} 块：boxShadow=${[...new Set(shot.panels.map((panel) => panel.surface.boxShadow))].join("/")}、borderRadius=${[...new Set(shot.panels.map((panel) => panel.surface.borderRadius))].join("/")}；看板容器 ${shot.grid.borderRadius}`;
      row.verdict =
        bad.length === 0 &&
        shot.grid.borderRadius === "8px" &&
        shot.panels.length === 4
          ? "PASS"
          : "FAIL";
      row.note =
        bad.length > 0
          ? `不合规面板：${bad.map((panel) => panel.id).join(",")}`
          : undefined;
    }
    record(row);
    assertVerdict(row);
  });

  test("A7 暗色底色", async ({ page }) => {
    const errors = await boot(page, { dock: "closed" });
    const ok = await waitForBoard(page);
    const row: Row = {
      id: "A7",
      expectation:
        "canvas rgb(9,10,15)；面板 rgb(18,19,26)；dock rgb(26,27,35)",
      actual: ok ? "" : `页面未渲染（${errors.join(" / ") || "无 main"}）`,
      verdict: "FAIL",
    };
    if (ok) {
      const before = await snapshot(page);
      const panelBg = before.panels[0]?.surface.backgroundColor ?? "";
      const parts = [
        `canvas ${before.bodyBg}（${readColor(before.bodyBg)}）`,
        `面板 ${panelBg}（${readColor(panelBg)}）`,
      ];
      // dock 底色只能在 dock 打开时量：点顶栏按钮走用户路径，白屏则如实记为量不到。
      await page.getByRole("button", { name: "Peco 对话" }).click();
      await page.waitForTimeout(600);
      const dockElement = page.locator("#peco-dock");
      const dockColor =
        (await dockElement.count()) > 0
          ? await dockElement
              .first()
              .evaluate((element) => getComputedStyle(element).backgroundColor)
          : null;
      parts.push(
        dockColor
          ? `dock ${dockColor}（${readColor(dockColor)}）`
          : `dock 无法测量（${errors.join(" / ") || "dock 未渲染"}）`,
      );
      row.actual = parts.join("；");
      row.verdict =
        before.bodyBg === "rgb(9, 10, 15)" &&
        panelBg === "rgb(18, 19, 26)" &&
        dockColor === "rgb(26, 27, 35)"
          ? "PASS"
          : "FAIL";
    }
    record(row);
    assertVerdict(row);
  });

  test("A12 无横向溢出", async ({ page }) => {
    const errors = await boot(page, { dock: "closed" });
    const ok = await waitForBoard(page);
    const row: Row = {
      id: "A12",
      expectation:
        "main-inner.scrollWidth − clientWidth = 0；每个面板 right ≤ main-inner.right + 0.5",
      actual: ok ? "" : `页面未渲染（${errors.join(" / ") || "无 main"}）`,
      verdict: "FAIL",
    };
    if (ok) {
      const shot = await snapshot(page);
      const overflow =
        shot.innerScroll.scrollWidth - shot.innerScroll.clientWidth;
      const spill = shot.panels.filter(
        (panel) => panel.box.right > shot.inner.right + 0.5,
      );
      row.actual = `横向差 ${overflow}；面板越界 ${spill.length} 个（main-inner.right ${shot.inner.right.toFixed(1)}，最右面板 ${Math.max(...shot.panels.map((panel) => panel.box.right)).toFixed(1)}）`;
      row.verdict = overflow === 0 && spill.length === 0 ? "PASS" : "FAIL";
      row.note =
        spill.length > 0
          ? `越界面板：${spill.map((panel) => panel.id).join(",")}`
          : undefined;
    }
    record(row);
    assertVerdict(row);
  });
});

test.describe("dock 开（1920×1080）", () => {
  test("A2 main-inner 宽度（dock 开）", async ({ page }) => {
    const errors = await boot(page, { dock: "open" });
    const ok = (await waitForBoard(page)) && (await stillMounted(page));
    const row: Row = {
      id: "A2",
      expectation: "main-inner 宽度 1196（±1）",
      actual: "",
      verdict: "FAIL",
    };
    if (!ok) {
      row.actual = `无法测量：${errors.join(" / ") || "页面未渲染"}`;
      row.note = SKIP;
    } else {
      const shot = await snapshot(page);
      if (!shot) {
        row.actual = `无法测量：白屏（${errors.join(" / ") || "无 main"}）`;
      } else {
        const delta = Math.abs(shot.inner.width - 1196);
        row.actual = `${shot.inner.width.toFixed(1)}（dock ${shot.dock?.width.toFixed(1) ?? "缺失"}，rail ${shot.rail?.width.toFixed(1)}）`;
        row.verdict = delta <= 1 ? "PASS" : "FAIL";
        row.note =
          delta <= 1
            ? undefined
            : `实测比规格宽 ${(shot.inner.width - 1196).toFixed(1)}：与 A1 同源，rail 实测 ${shot.rail?.width.toFixed(1)} ≠ 规格 256`;
      }
    }
    record(row);
    assertVerdict(row);
  });

  test("A3 看板列数（dock 开）", async ({ page }) => {
    const errors = await boot(page, { dock: "open" });
    const ok = (await waitForBoard(page)) && (await stillMounted(page));
    const row: Row = {
      id: "A3(dock 开)",
      expectation: "gridTemplateColumns 列数 6",
      actual: ok ? "" : `无法测量：${errors.join(" / ") || "页面未渲染"}`,
      verdict: "FAIL",
    };
    if (ok) {
      const shot = await snapshot(page);
      if (!shot) {
        row.actual = `无法测量：白屏（${errors.join(" / ") || "无 main"}）`;
      } else {
        row.actual = `${shot.grid.columns.length} 列（main-inner ${shot.inner.width.toFixed(1)}）`;
        row.verdict = shot.grid.columns.length === 6 ? "PASS" : "FAIL";
      }
    } else {
      row.note = SKIP;
    }
    record(row);
    assertVerdict(row);
  });

  test("A4 今日任务面板跨列（dock 开）", async ({ page }) => {
    const errors = await boot(page, { dock: "open" });
    const ok = (await waitForBoard(page)) && (await stillMounted(page));
    const row: Row = {
      id: "A4(dock 开)",
      expectation: "今日任务 span 6",
      actual: ok ? "" : `无法测量：${errors.join(" / ") || "页面未渲染"}`,
      verdict: "FAIL",
    };
    if (ok) {
      const shot = await snapshot(page);
      if (!shot) {
        row.actual = `无法测量：白屏（${errors.join(" / ") || "无 main"}）`;
      } else {
        const today = shot.panels.find((panel) => panel.id === "panel-today");
        row.actual = today
          ? `gridColumnStart=${today.gridColumnStart}、gridColumnEnd=${today.gridColumnEnd}`
          : "找不到 panel-today";
        row.verdict = today?.gridColumnStart === "span 6" ? "PASS" : "FAIL";
      }
    } else {
      row.note = SKIP;
    }
    record(row);
    assertVerdict(row);
  });
});

test.describe("列表行高与滚动", () => {
  test("A13 列表行高", async ({ page }) => {
    const errors = await boot(page, { dock: "closed" }, "/spaces");
    const loaded = await page
      .waitForSelector("li[data-ws-id]", { timeout: 15_000 })
      .then(
        () => true,
        () => false,
      );
    const row: Row = {
      id: "A13",
      expectation: "/spaces 行 40（c≥900）/ 56 / 64；台账行 36；rail 空间行 32",
      actual: loaded
        ? ""
        : `无法测量：${errors.join(" / ") || "/spaces 没有行"}`,
      verdict: "FAIL",
    };
    if (loaded) {
      const parts: string[] = [];
      let pass = true;
      const rowHeight = async () =>
        page.evaluate(() => {
          const row = document.querySelector("li[data-ws-id]") as HTMLElement;
          const inner = document.querySelector("main")
            ?.firstElementChild as HTMLElement;
          return {
            height: row.getBoundingClientRect().height,
            container: inner.getBoundingClientRect().width,
          };
        });

      // 档位看的是容器宽度 c，不是视口：视口按 (rail + dock + 内边距 + 滚动条) 反推，
      // 而 rail 宽度不是固定值，所以这里按实测量出来的 c 迭代几次凑到目标档位。
      const measure = async (targetContainer: number) => {
        for (let attempt = 0; attempt < 4; attempt += 1) {
          const current = await rowHeight();
          if (Math.abs(current.container - targetContainer) <= 30)
            return current;
          const viewport = await page.evaluate(() => window.innerWidth);
          await page.setViewportSize({
            width: Math.max(
              360,
              Math.round(viewport + (targetContainer - current.container)),
            ),
            height: 1080,
          });
          await page.waitForTimeout(500);
        }
        return rowHeight();
      };

      const wide = await measure(1616);
      parts.push(`/spaces 行 ${wide.height}（c=${wide.container.toFixed(0)}）`);
      pass &&= wide.height === 40 && wide.container >= 900;

      const mid = await measure(800);
      parts.push(`/spaces 行 ${mid.height}（c=${mid.container.toFixed(0)}）`);
      pass &&= mid.height === 56;

      const narrow = await measure(600);
      parts.push(
        `/spaces 行 ${narrow.height}（c=${narrow.container.toFixed(0)}）`,
      );
      pass &&= narrow.height === 64;

      await page.setViewportSize({ width: 1920, height: 1080 });
      await page.waitForTimeout(600);
      const railRows = await page.$$eval("[data-space-row]", (rows) =>
        rows.map((element) => element.getBoundingClientRect().height),
      );
      parts.push(
        `rail 空间行 ${[...new Set(railRows)].join("/")}（${railRows.length} 行）`,
      );
      pass &&= railRows.length > 0 && railRows.every((height) => height === 32);

      await page.goto(`/spaces/${SESSION_WS}?tab=tasks`, { waitUntil: "load" });
      const tasksLoaded = await page
        .waitForSelector("li > button[aria-expanded]", { timeout: 15_000 })
        .then(
          () => true,
          () => false,
        );
      if (!tasksLoaded) {
        parts.push("台账行 无法测量（台账没有行）");
        pass = false;
      } else {
        const ledger = await page.$$eval(
          "li > button[aria-expanded]",
          (buttons) =>
            buttons.map((button) => button.getBoundingClientRect().height),
        );
        parts.push(
          `台账行 ${[...new Set(ledger)].join("/")}（${ledger.length} 行）`,
        );
        pass &&= ledger.length > 0 && ledger.every((height) => height === 36);
      }

      row.actual = parts.join("；");
      row.verdict = pass ? "PASS" : "FAIL";
    }
    record(row);
    assertVerdict(row);
  });

  test("A21 切空间后的滚动位置", async ({ page }) => {
    const errors = await boot(page, { dock: "closed" }, "/spaces");
    const loaded = await page
      .waitForSelector("li[data-ws-id]", { timeout: 15_000 })
      .then(
        () => true,
        () => false,
      );
    const row: Row = {
      id: "A21",
      expectation: "/spaces 切换后 main.scrollTop 保持（±2px）；/ 切换后 = 0",
      actual: loaded
        ? ""
        : `无法测量：${errors.join(" / ") || "/spaces 没有行"}`,
      verdict: "FAIL",
    };
    if (loaded) {
      const scrollState = () =>
        page.evaluate(() => {
          const main = document.querySelector("main") as HTMLElement;
          return {
            top: main.scrollTop,
            scrollable: main.scrollHeight - main.clientHeight,
          };
        });

      const before = await scrollState();
      await page.evaluate(() => {
        const main = document.querySelector("main") as HTMLElement;
        main.scrollTop = 300;
      });
      await page.waitForTimeout(200);
      const scrolled = await scrollState();
      const target = page.locator("[data-space-row]").nth(2);
      await target.click();
      await page.waitForTimeout(1200);
      const after = await scrollState();

      const listPass = Math.abs(after.top - scrolled.top) <= 2;

      await page.goto("/", { waitUntil: "load" });
      await waitForBoard(page);
      await page.locator("[data-space-row]").nth(1).click();
      await page.waitForTimeout(1200);
      const board = await scrollState();

      row.actual = `/spaces：滚动余量 ${before.scrollable}px，切前 ${scrolled.top}，切后 ${after.top}；/：滚动余量 ${board.scrollable}px，切后 ${board.top}`;
      row.verdict = listPass && board.top === 0 ? "PASS" : "FAIL";
      row.note =
        board.scrollable === 0
          ? "/ 在看板高度不足一屏时不可滚，`scrollTop === 0` 恒真，这条只有 /spaces 一侧非空"
          : undefined;
    }
    record(row);
    assertVerdict(row);
  });
});

test.describe("会话 tab（正文行宽 / 垂直贴合）", () => {
  test("A10 正文行宽 / A11 垂直贴合", async ({ page }) => {
    const errors = await boot(
      page,
      { dock: "closed" },
      `/spaces/${SESSION_WS}?tab=sessions`,
    );
    const rendered = await page
      .waitForSelector("[data-slot='message-scroller']", { timeout: 15_000 })
      .then(
        () => true,
        () => false,
      );

    const a10: Row = {
      id: "A10",
      expectation:
        "markdown 段落 clientWidth ≤ 640 且 clientWidth/fontSize ≤ 45.8",
      actual: rendered
        ? ""
        : `无法测量：${errors.join(" / ") || "会话 tab 未渲染 ChatView"}`,
      verdict: "SKIPPED",
    };
    const a11: Row = {
      id: "A11",
      expectation: "ChatView 根 bottom 与 main-inner.bottom 差 ≤ 1px",
      actual: rendered
        ? ""
        : `无法测量：${errors.join(" / ") || "会话 tab 未渲染 ChatView"}`,
      verdict: "SKIPPED",
    };

    if (!rendered) {
      a10.note = SKIP;
      a11.note = SKIP;
      // 会话 tab 渲染即白屏，属产品缺陷而非「数据不具备」，故判 FAIL 而不是 SKIPPED。
      a10.verdict = "FAIL";
      a11.verdict = "FAIL";
      record(a10);
      record(a11);
      assertVerdict(a10);
      return;
    }

    const measured = await page.evaluate(() => {
      const scroller = document.querySelector(
        "[data-slot='message-scroller']",
      ) as HTMLElement;
      const paragraphs = Array.from(scroller.querySelectorAll("p"));
      const inner = document.querySelector("main")
        ?.firstElementChild as HTMLElement;
      const chatRoot = scroller.parentElement as HTMLElement;
      const widest = paragraphs
        .map((paragraph) => ({
          width: paragraph.clientWidth,
          fontSize: parseFloat(getComputedStyle(paragraph).fontSize),
          text: (paragraph.textContent ?? "").slice(0, 20),
        }))
        .sort((a, b) => b.width - a.width)[0];
      return {
        paragraphs: paragraphs.length,
        widest,
        innerBottom: inner.getBoundingClientRect().bottom,
        chatRootBottom: chatRoot.getBoundingClientRect().bottom,
        chatRootClass: chatRoot.className,
      };
    });

    if (!measured.widest || measured.paragraphs === 0) {
      a10.verdict = "SKIPPED";
      a10.actual = `会话里没有 <p>（段落数 ${measured.paragraphs}），无正文行宽可量`;
      a10.note = "seed 的会话消息没渲染成 markdown 段落";
    } else {
      const ratio = measured.widest.width / measured.widest.fontSize;
      a10.actual = `最宽段落 ${measured.widest.width}px / 字号 ${measured.widest.fontSize}px = ${ratio.toFixed(2)}em（${measured.paragraphs} 段）`;
      a10.verdict =
        measured.widest.width <= 640 && ratio <= 45.8 ? "PASS" : "FAIL";
    }

    const gap = Math.abs(measured.innerBottom - measured.chatRootBottom);
    a11.actual = `${gap.toFixed(1)}px（ChatView 根 bottom ${measured.chatRootBottom.toFixed(1)}，main-inner.bottom ${measured.innerBottom.toFixed(1)}）`;
    a11.verdict = gap <= 1 ? "PASS" : "FAIL";

    record(a10);
    record(a11);
    assertVerdict(a10);
    assertVerdict(a11);
  });
});

test.describe("prefers-reduced-motion: reduce", () => {
  test.use({ reducedMotion: "reduce" });

  test("A23 活动条动效降级", async ({ page }) => {
    const errors = await boot(page, { dock: "closed", reduceMotion: true });
    const ok = await waitForBoard(page);
    const row: Row = {
      id: "A23",
      expectation:
        "活动条 transitionDuration ∈ {0s, 0.01ms}；animationName 不含 pulse",
      actual: ok ? "" : `页面未渲染（${errors.join(" / ") || "无 main"}）`,
      verdict: "FAIL",
    };
    if (ok) {
      const bar = page.locator("[data-testid='rail-active-bar']").first();
      const style = await bar.evaluate((element) => {
        const computed = getComputedStyle(element);
        return {
          transitionDuration: computed.transitionDuration,
          animationName: computed.animationName,
          mediaMatches: window.matchMedia("(prefers-reduced-motion: reduce)")
            .matches,
        };
      });
      // Chromium 把 0.01ms 序列化成 `1e-05s`：按毫秒数值判，原文照抄进实测值。
      const durationMs = style.transitionDuration.endsWith("ms")
        ? parseFloat(style.transitionDuration)
        : parseFloat(style.transitionDuration) * 1000;
      row.actual = `transitionDuration=${style.transitionDuration}（= ${durationMs}ms）、animationName=${style.animationName}（媒体查询命中 ${style.mediaMatches}）`;
      row.verdict =
        (durationMs === 0 || Math.abs(durationMs - 0.01) < 1e-6) &&
        !style.animationName.includes("pulse")
          ? "PASS"
          : "FAIL";
    }
    record(row);
    assertVerdict(row);
  });
});

test.describe("焦点可见", () => {
  test("A24 Tab 到 rail 首行的焦点环", async ({ page }) => {
    const errors = await boot(page, { dock: "closed" });
    const ok = await waitForBoard(page);
    const row: Row = {
      id: "A24",
      expectation:
        "outlineWidth=2px、outlineColor=primary（rgb(99,102,241)）、outlineOffset=2px",
      actual: ok ? "" : `页面未渲染（${errors.join(" / ") || "无 main"}）`,
      verdict: "FAIL",
    };
    if (ok) {
      let reached = false;
      for (let step = 0; step < 12 && !reached; step += 1) {
        await page.keyboard.press("Tab");
        reached = await page.evaluate(() =>
          Boolean(document.activeElement?.closest("[data-space-row]")),
        );
      }
      if (!reached) {
        row.actual = "连按 12 次 Tab 后焦点仍未落在 rail 空间行";
      } else {
        // 行带 `transition-colors`（含 outline-color，120ms）：落定后再读，否则量到的是走色中间帧。
        await page.waitForTimeout(300);
        const style = await page.evaluate(() => {
          const active = document.activeElement as HTMLElement;
          const computed = getComputedStyle(active);
          return {
            label:
              active.getAttribute("aria-label") ?? active.dataset.wsId ?? "",
            outlineWidth: computed.outlineWidth,
            outlineColor: computed.outlineColor,
            outlineOffset: computed.outlineOffset,
            outlineStyle: computed.outlineStyle,
          };
        });
        row.actual = `焦点「${style.label}」：outlineWidth=${style.outlineWidth}、outlineColor=${style.outlineColor}、outlineOffset=${style.outlineOffset}（${style.outlineStyle}）`;
        row.verdict =
          style.outlineWidth === "2px" &&
          style.outlineColor === "rgb(99, 102, 241)" &&
          style.outlineOffset === "2px"
            ? "PASS"
            : "FAIL";
      }
    }
    record(row);
    assertVerdict(row);
  });
});

// 断链后的文案是「第几次重试」：退避首档 1000ms，所以断开后 1.2s 内一定还在第 1 次，
// 1500ms 那个时点已经进第 2 次。点色两处都是 warn。
const RECONNECT_POINT = "rgb(242, 166, 53)";

interface SseSample {
  stamp: number;
  state: string;
  text: string;
  dot: string;
}

/** 一次 evaluate 取齐三项：分三次往返读，会把重渲染前后的两帧拼成一个自相矛盾的快照。 */
async function readSse(
  target: Page,
  selector: string,
): Promise<Omit<SseSample, "stamp"> | null> {
  return target.evaluate((sel) => {
    const root = document.querySelector(sel);
    if (!root) return null;
    const dot = root.querySelector("span[aria-hidden='true']");
    return {
      state: root.getAttribute("data-sse-state") ?? "",
      text: (root.textContent ?? "").trim(),
      dot: dot ? getComputedStyle(dot).backgroundColor : "",
    };
  }, selector);
}

async function sseState(target: Page, selector: string): Promise<string> {
  return target.evaluate(
    (sel) => document.querySelector(sel)?.getAttribute("data-sse-state") ?? "",
    selector,
  );
}

/**
 * SSE 断链。本组会停掉 dev server，断完之后整套页面都加载不出来 —— 所以它排在文件末尾，
 * 且只断一次：dock 头与台账顶部是同一枚状态机的两个显示位（规格要求两处一致），
 * 在同一次断链里同时观测。
 *
 * 断链手段必须真的掐掉已建立的连接。实测：`context.setOffline` 掐不断（仍显示「已连接」），
 * 停 daemon 也掐不断（vite 代理不把上游断开透传给浏览器，20s 后仍「已连接」）；
 * 停掉 dev server 进程组才让浏览器收到断开。
 */
test.describe("SSE 断链（停 dev server，放在最后）", () => {
  test("A22 / A22-补 断链后的重试文案与点色", async ({ page, context }) => {
    const dockErrors = await boot(page, { dock: "open" });
    const dockReady = (await waitForBoard(page)) && (await stillMounted(page));
    // 台账顶部那枚点用第二个页面量：同一 context 下同一次断链，两处都要能看到。
    const ledger = await context.newPage();
    const ledgerErrors = await boot(
      ledger,
      { dock: "closed" },
      `/spaces/${SESSION_WS}?tab=tasks`,
    );
    const ledgerReady = await ledger
      .waitForSelector("[data-sse-state]", { timeout: 15_000 })
      .then(
        () => true,
        () => false,
      );

    const targets = [
      {
        id: "A22",
        page,
        selector: "#peco-dock [data-sse-state]",
        expectation:
          "dock 头：断开后 ≤1.2s「连接中断，第 1 次重试（1/6）」，1500ms 时「第 2 次重试（2/6）」，点 rgb(242,166,53)",
        ready: dockReady,
        errors: dockErrors,
        missing: "dock 未渲染（白屏）",
      },
      {
        id: "A22-补",
        page: ledger,
        selector: "[data-sse-state]",
        expectation:
          "台账顶部（同一状态机）：断开后 ≤1.2s「第 1 次重试（1/6）」，1500ms 时「第 2 次（2/6）」，点 rgb(242,166,53)",
        ready: ledgerReady,
        errors: ledgerErrors,
        missing: "台账顶部没有状态点",
      },
    ];

    if (!targets.every((target) => target.ready)) {
      for (const target of targets) {
        const row: Row = {
          id: target.id,
          expectation: target.expectation,
          actual: `无法测量：${target.errors.join(" / ") || target.missing}`,
          verdict: "FAIL",
          note: target.missing,
        };
        record(row);
        assertVerdict(row);
      }
      await ledger.close();
      return;
    }

    // dev 代理把 SSE 响应头压到首个心跳（15s）才吐出来：不先等 open，断的是一个还没
    // 建立的连接，什么都不会发生。
    const awaited: Record<string, number> = {};
    for (const target of targets) {
      const began = Date.now();
      let state = "";
      while (Date.now() - began < 25_000 && state !== "open") {
        state = await sseState(target.page, target.selector);
        if (state !== "open") await target.page.waitForTimeout(250);
      }
      awaited[target.id] = Date.now() - began;
      if (state !== "open") {
        const row: Row = {
          id: target.id,
          expectation: target.expectation,
          actual: `等 ${awaited[target.id]}ms 事件流仍未 open（当前 ${state}）：拿不到可断的流`,
          verdict: "FAIL",
        };
        record(row);
        assertVerdict(row);
        await ledger.close();
        return;
      }
    }

    const vitePid = Number(process.env.E2E_VITE_PID ?? "");
    if (!Number.isInteger(vitePid) || vitePid <= 0) {
      for (const target of targets) {
        const row: Row = {
          id: target.id,
          expectation: target.expectation,
          actual: "未提供 E2E_VITE_PID，无法造断链",
          verdict: "FAIL",
        };
        record(row);
        assertVerdict(row);
      }
      await ledger.close();
      return;
    }
    // dev server 是 setsid 起的进程组：只杀组长会留下还握着 socket 的 node。
    try {
      process.kill(-vitePid, "SIGTERM");
    } catch {
      process.kill(vitePid, "SIGTERM");
    }

    const samples: Record<string, SseSample[]> = {
      A22: [],
      "A22-补": [],
    };
    const began = Date.now();
    while (Date.now() - began <= 3400) {
      const stamp = Date.now() - began;
      for (const target of targets) {
        const read = await readSse(target.page, target.selector);
        if (read) samples[target.id].push({ stamp, ...read });
      }
      await page.waitForTimeout(100);
    }
    await ledger.close();

    for (const target of targets) {
      const taken = samples[target.id];
      const trail: string[] = [];
      let seen = "";
      for (const sample of taken) {
        const signature = `${sample.state}|${sample.text}|${sample.dot}`;
        if (signature === seen) continue;
        seen = signature;
        trail.push(`+${sample.stamp}ms「${sample.text}」${sample.dot}`);
      }
      const first = taken.find((sample) =>
        sample.text.includes("第 1 次重试（1/6）"),
      );
      const at1500 = taken.find((sample) => sample.stamp >= 1450);
      const row: Row = {
        id: target.id,
        expectation: target.expectation,
        actual: `等「已连接」${awaited[target.id]}ms；断开后轨迹 ${trail.join(" → ") || "无采样"}；1500ms 时点「${at1500?.text ?? "无采样"}」${at1500?.dot ?? ""}（${readColor(at1500?.dot ?? "")}）`,
        verdict: "FAIL",
      };
      row.verdict =
        first !== undefined &&
        first.stamp <= 1200 &&
        first.dot === RECONNECT_POINT &&
        (at1500?.text.includes("第 2 次重试（2/6）") ?? false) &&
        at1500?.dot === RECONNECT_POINT
          ? "PASS"
          : "FAIL";
      if (first === undefined) {
        row.note =
          "断开后没看到「第 1 次重试」：状态机没有进入重连，或首档落地晚于采样窗口";
      }
      record(row);
      assertVerdict(row);
    }
  });
});

/**
 * 汇总放成最后一条 test：文件级 afterAll 会在每个 describe 边界重复触发，
 * 而每个 worker 组又各有一份模块状态，所以读落盘的行而不是内存里的数组。
 */
test("汇总表", () => {
  const collected: Row[] = fs.existsSync(ROWS_FILE)
    ? fs
        .readFileSync(ROWS_FILE, "utf8")
        .split("\n")
        .filter(Boolean)
        .map((line) => JSON.parse(line) as Row)
    : [];
  console.log("\n===== 实测汇总 =====");
  console.log("| # | 期望 | 实测 | 判定 |");
  console.log("|---|---|---|---|");
  for (const row of collected) {
    const note = row.note ? `（${row.note}）` : "";
    console.log(
      `| ${row.id} | ${row.expectation} | ${row.actual} | ${row.verdict}${note} |`,
    );
  }
  const count = (verdict: Verdict) =>
    collected.filter((row) => row.verdict === verdict).length;
  console.log(
    `\n合计 ${collected.length} 条：PASS ${count("PASS")} / FAIL ${count("FAIL")} / SKIPPED ${count("SKIPPED")}`,
  );
  // 17 条规格断言行（15 条 + A3/A4 各多一个 dock 开的档位变体）+ A22 的补充观测。
  // 少一条就说明有断言没跑到。
  expect(collected.length, "有断言没产出实测值").toBe(18);
});
