// [浏览器] 档验收：v2 信息架构 —— 左栏打开集、Peco 团队配置、空间页右栏、团队卡片。
// 全部在真实 Chromium（headless:false，X 下）里量测，每条断言打印「期望 vs 实测」并在末尾汇总。
//
// 测量口径固定：暗色主题、rail 展开、右栏展开；打开集与当前空间用 addInitScript 预置到
// localStorage（形状与 `stores/openSpacesStore.ts` 的 persist 一致：`{state, version}`）。
// 需要「已打开空间」才有真实数据的面板（文件 / 任务 / 记忆）由 beforeAll 通过 daemon 的
// 幂等接口打开 —— 种子只写库与目录，打开是运行期动作。
//
// 这里只测量，不改期望值：实测不符就记 FAIL，交给规格裁决。

import fs from "node:fs";
import path from "node:path";
import {
  test,
  expect,
  type APIRequestContext,
  type Browser,
  type Page,
} from "@playwright/test";

const BASE_URL = process.env.E2E_BASE_URL ?? "http://127.0.0.1:5174";

/** 三个空间足够验证行数、顺序与切换。 */
const OPEN_IDS = ["ws_demo_legal", "ws_demo_finance", "ws_demo_ops"];
const ACTIVE_WS = "ws_demo_legal";
const SPACE_WS = "ws_demo_legal";

const WIDE = { width: 1920, height: 1080 };
const NARROW = { width: 900, height: 800 };
/** 打开集塞满时的视口：左栏列表必须高到要滚，才有「设置跟不跟着滚」可测。 */
const SCROLLABLE = { width: 1280, height: 800 };

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
  "B1",
  "B2",
  "B3",
  "B4",
  "B5",
  "B6",
  "B7",
  "B8",
  "B9",
  "B10",
  "B11",
  "B12",
  "C1",
  "C2",
  "C3",
  "C4",
  "C5",
  "C6",
  "D1",
  "D2",
  "D3",
  "D4",
  "D5",
  "D6",
  "E1",
  "E2",
  "E3",
  "E4",
  "E5",
  "E6",
  "E7",
  "E8",
  "E9",
  "E10",
  "E11",
  "E12",
  "E13",
  "F1",
  "F2",
  "F3",
  "F4",
  "F5",
  "F6",
  "F7",
  "F8",
  "F9",
  "F10",
  "F11",
  "F12",
  "G1",
  "G2",
  "G3",
  "G4",
  "G5",
  "G6",
  "G7",
  "G8",
];

/**
 * 实测行落盘而不是留在内存：断言失败会让 Playwright 换一个 worker 重新导入本文件，
 * 模块里的数组活不过那一次重启，而汇总表要看到全部行。`run.sh` 在开跑前会清掉 `.report`。
 */
const ROWS_FILE = path.join(process.cwd(), "e2e", ".report", "rows.jsonl");

function format(value: unknown): string {
  if (Array.isArray(value))
    return value.length === 0 ? "[]" : value.join(" / ");
  if (value === null) return "null";
  return String(value);
}

/**
 * 记一行实测并软断言：失败只让本条变红，后面的断言继续跑完 —— 测量台要的是全量实测值。
 *
 * 断言消息里带实测值：回归时能直接看出「从 3 行变成 2 行」这类漂移，不必翻日志。
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

interface SessionOptions {
  viewport: { width: number; height: number };
  path: string;
  openIds?: string[];
  activeWsId?: string | null;
  /** 首屏就绪的选择器：等到它出现，后续量测才有意义。 */
  ready: string;
}

interface Session {
  page: Page;
  consoleErrors: string[];
  pageErrors: string[];
  closeRequests: string[];
  dispose: () => Promise<void>;
}

/** 一个独立浏览器上下文：localStorage 预置 + 报错与关闭请求的采集。 */
async function openSession(
  browser: Browser,
  options: SessionOptions,
): Promise<Session> {
  const context = await browser.newContext({
    viewport: options.viewport,
    baseURL: BASE_URL,
  });
  const page = await context.newPage();
  const consoleErrors: string[] = [];
  const pageErrors: string[] = [];
  const closeRequests: string[] = [];

  page.on("console", (message) => {
    if (message.type() === "error") consoleErrors.push(message.text());
  });
  page.on("pageerror", (error) =>
    pageErrors.push(error.message.split("\n")[0]),
  );
  page.on("request", (request) => {
    const path = new URL(request.url()).pathname;
    if (path.endsWith("/close")) {
      closeRequests.push(`${request.method()} ${path}`);
    }
  });

  await page.addInitScript(
    (preset) => {
      localStorage.setItem(
        "peco-webui:theme",
        JSON.stringify({ state: { theme: "dark" }, version: 0 }),
      );
      localStorage.setItem(
        "peco-webui:rail",
        JSON.stringify({ state: { railOpen: true }, version: 0 }),
      );
      localStorage.setItem(
        "peco-webui:ws",
        JSON.stringify({
          state: { currentWsId: preset.activeWsId },
          version: 0,
        }),
      );
      localStorage.setItem(
        "peco-webui:open-spaces",
        JSON.stringify({
          state: {
            openIds: preset.openIds,
            activeWsId: preset.activeWsId,
            seeded: true,
          },
          version: 0,
        }),
      );
    },
    {
      openIds: options.openIds ?? [],
      activeWsId: options.activeWsId ?? null,
    },
  );

  await page.goto(options.path, { waitUntil: "load" });
  await page.waitForSelector(options.ready, { timeout: 20_000 });
  return {
    page,
    consoleErrors,
    pageErrors,
    closeRequests,
    dispose: () => context.close(),
  };
}

interface Box {
  x: number;
  y: number;
  width: number;
  height: number;
  right: number;
  bottom: number;
}

interface PageFacts {
  url: string;
  rootChildren: number;
  mainChildren: number;
  html: { scrollWidth: number; clientWidth: number };
  body: { scrollWidth: number; clientWidth: number };
  main: { scrollWidth: number; clientWidth: number; width: number } | null;
}

async function pageFacts(page: Page): Promise<PageFacts> {
  return page.evaluate(() => {
    const size = (element: Element) => ({
      scrollWidth: element.scrollWidth,
      clientWidth: element.clientWidth,
    });
    const main = document.querySelector("main");
    return {
      url: `${location.pathname}${location.search}`,
      rootChildren: document.querySelector("#root")?.children.length ?? 0,
      mainChildren: main?.firstElementChild ? 1 : 0,
      html: size(document.documentElement),
      body: size(document.body),
      main: main
        ? {
            ...size(main),
            width: Math.round(main.getBoundingClientRect().width),
          }
        : null,
    };
  });
}

interface RailFacts {
  rail: Box | null;
  rows: { wsId: string | null; label: string | null }[];
  active: (string | null)[];
  settings: Box | null;
  scroller: {
    scrollTop: number;
    scrollHeight: number;
    clientHeight: number;
  } | null;
}

async function railFacts(page: Page): Promise<RailFacts> {
  return page.evaluate(() => {
    const box = (element: Element | null) => {
      if (!element) return null;
      const rect = element.getBoundingClientRect();
      return {
        x: Math.round(rect.x),
        y: Math.round(rect.y),
        width: Math.round(rect.width),
        height: Math.round(rect.height),
        right: Math.round(rect.right),
        bottom: Math.round(rect.bottom),
      };
    };
    const aside = document.querySelector("aside");
    const rows = Array.from(
      document.querySelectorAll<HTMLElement>("[data-space-row][data-ws-id]"),
    );
    // 列表是 aside 里唯一纵向可滚的 div：设置那一格在它外面，正是「固定底部」的判据。
    const scroller = aside
      ? Array.from(aside.querySelectorAll<HTMLElement>("div")).find(
          (node) => getComputedStyle(node).overflowY === "auto",
        )
      : null;
    return {
      rail: box(aside),
      rows: rows.map((row) => ({
        wsId: row.dataset.wsId ?? null,
        label: row.getAttribute("aria-label"),
      })),
      active: rows
        .filter((row) => row.getAttribute("aria-current") === "true")
        .map((row) => row.dataset.wsId ?? null),
      settings: box(document.querySelector('a[aria-label="设置"]')),
      scroller: scroller
        ? {
            scrollTop: scroller.scrollTop,
            scrollHeight: scroller.scrollHeight,
            clientHeight: scroller.clientHeight,
          }
        : null,
    };
  });
}

/** 把左栏列表滚到底（与 `railFacts` 找的是同一个 div）。 */
async function scrollRailToBottom(page: Page): Promise<void> {
  await page.evaluate(() => {
    const aside = document.querySelector("aside");
    const scroller = aside
      ? Array.from(aside.querySelectorAll<HTMLElement>("div")).find(
          (node) => getComputedStyle(node).overflowY === "auto",
        )
      : null;
    if (scroller) scroller.scrollTop = scroller.scrollHeight;
  });
  await page.waitForTimeout(300);
}

interface PanelFacts {
  mode: string | null;
  width: number;
  height: number;
  entries: string[];
  /** 任务面板顶部的来源标注（`data-fidelity`：live / digest）与整行文案；没有标注时 null。 */
  fidelity: { kind: string | null; text: string } | null;
  text: string;
}

async function panelFacts(page: Page): Promise<PanelFacts | null> {
  return page.evaluate(() => {
    const panel = document.querySelector<HTMLElement>(
      '[data-testid="space-panel"], [data-testid="peco-panel"]',
    );
    if (!panel) return null;
    const rect = panel.getBoundingClientRect();
    const fidelity = panel.querySelector<HTMLElement>(
      '[data-testid="task-fidelity"]',
    );
    return {
      mode: panel.dataset.mode ?? null,
      width: Math.round(rect.width),
      height: Math.round(rect.height),
      entries: Array.from(panel.querySelectorAll("ul li button")).map((node) =>
        (node.textContent ?? "").trim(),
      ),
      fidelity: fidelity
        ? {
            kind: fidelity.dataset.fidelity ?? null,
            text: (fidelity.textContent ?? "").trim(),
          }
        : null,
      text: (panel.textContent ?? "").trim(),
    };
  });
}

/**
 * 任务面板的数据源落定没有：模式是点击即切的，来源标注要等查询回来才挂上；
 * 台账为空时连标注都不挂，只剩空态。两者之外量到的都是骨架。
 */
function taskPanelSettled(facts: PanelFacts | null): boolean {
  if (!facts) return false;
  return facts.fidelity !== null || facts.text.includes("这个空间还没有任务");
}

interface PecoFacts {
  url: string;
  tabs: string[];
  overviewPanels: number;
  newSpaceButtons: number;
}

async function pecoFacts(page: Page): Promise<PecoFacts> {
  return page.evaluate(() => ({
    url: `${location.pathname}${location.search}`,
    tabs: Array.from(document.querySelectorAll('[role="tab"]')).map((node) =>
      (node.textContent ?? "").trim(),
    ),
    overviewPanels: document.querySelectorAll(
      '[data-testid="peco-panel"] [data-testid^="panel-"]',
    ).length,
    newSpaceButtons: Array.from(document.querySelectorAll("button")).filter(
      (node) => (node.textContent ?? "").includes("新建空间"),
    ).length,
  }));
}

interface TeamFacts {
  columns: string[];
  cards: { id: string | null; stats: Record<string, string> }[];
}

async function teamFacts(page: Page): Promise<TeamFacts> {
  return page.evaluate(() => {
    const grid = document.querySelector("ul.grid");
    const columns = grid
      ? getComputedStyle(grid)
          .gridTemplateColumns.split(" ")
          .filter((track) => track !== "")
      : [];
    return {
      columns,
      cards: Array.from(
        document.querySelectorAll<HTMLElement>("[data-team-id]"),
      ).map((card) => ({
        id: card.dataset.teamId ?? null,
        stats: Object.fromEntries(
          Array.from(card.querySelectorAll<HTMLElement>("[data-stat]")).map(
            (stat) => [
              stat.dataset.stat ?? "",
              (stat.textContent ?? "").trim(),
            ],
          ),
        ),
      })),
    };
  });
}

async function pathOf(page: Page): Promise<string> {
  return page.evaluate(() => `${location.pathname}${location.search}`);
}

/** 空间行容器（主按钮 / 状态链接 / × 三个动作的父节点）。 */
function spaceRow(page: Page, wsId: string) {
  return page
    .locator(`[data-space-row][data-ws-id="${wsId}"]`)
    .locator("xpath=..");
}

/** 打开集里的全部空间 id：清单接口是唯一来源，测试不抄一份种子。 */
async function listOpenableSpaceIds(
  request: APIRequestContext,
): Promise<string[]> {
  const response = await request.get("/api/spaces");
  if (!response.ok()) {
    throw new Error(
      `GET /api/spaces 失败：${response.status()} ${await response.text()}`,
    );
  }
  const spaces = (await response.json()) as {
    ws_id: string;
    reserved: boolean;
  }[];
  const ids = spaces.filter((space) => !space.reserved).map((s) => s.ws_id);
  if (ids.length === 0) {
    throw new Error("GET /api/spaces 没有返回可打开的空间：seed 是否跑过？");
  }
  return ids;
}

/** 打开空间：`POST /api/spaces` 对同一 work_dir 幂等，已打开时原样返回。 */
async function openSpace(
  request: APIRequestContext,
  wsId: string,
): Promise<void> {
  const response = await request.get("/api/spaces");
  if (!response.ok()) {
    throw new Error(`GET /api/spaces 失败：${response.status()}`);
  }
  const spaces = (await response.json()) as {
    ws_id: string;
    work_dir: string;
    team_id: string;
    status: string;
  }[];
  const space = spaces.find((item) => item.ws_id === wsId);
  if (!space) throw new Error(`空间 ${wsId} 不在 /api/spaces 里`);
  if (space.status === "open") return;
  const opened = await request.post("/api/spaces", {
    data: { work_dir: space.work_dir, team_id: space.team_id },
  });
  if (!opened.ok()) {
    throw new Error(
      `打开 ${wsId} 失败：${opened.status()} ${await opened.text()}`,
    );
  }
}

interface LedgerTask {
  task_id: string;
  result_digest: string | null;
}

/** 该空间作为 target 的台账（`GET /api/spaces/{ws_id}/tasks`），与任务面板回落时同一份数据。 */
async function listSpaceLedgerTasks(
  request: APIRequestContext,
  wsId: string,
): Promise<LedgerTask[]> {
  const response = await request.get(
    `/api/spaces/${encodeURIComponent(wsId)}/tasks`,
  );
  if (!response.ok()) {
    throw new Error(
      `GET /api/spaces/${wsId}/tasks 失败：${response.status()} ${await response.text()}`,
    );
  }
  return (await response.json()) as LedgerTask[];
}

/** 实时快照的状态码与行数：409 要如实报出来，不能当成「开着但 0 行」。 */
async function liveTaskRowCount(
  request: APIRequestContext,
  wsId: string,
): Promise<{ status: number; rows: number | null }> {
  const response = await request.get(
    `/api/spaces/${encodeURIComponent(wsId)}/tasks/live`,
  );
  if (!response.ok()) return { status: response.status(), rows: null };
  return {
    status: response.status(),
    rows: ((await response.json()) as unknown[]).length,
  };
}

/**
 * 找一个台账里一条任务都没有的空间：空态判据要有真实的空数据才量得出来，
 * 而种子里哪个空间空着是种子的事实 —— 照台账接口逐个问，不在这里抄一份。
 */
async function findSpaceWithoutLedgerTasks(
  request: APIRequestContext,
): Promise<string> {
  for (const wsId of await listOpenableSpaceIds(request)) {
    if ((await listSpaceLedgerTasks(request, wsId)).length === 0) return wsId;
  }
  throw new Error("种子里的空间都有台账行：空态判据没有空数据可量");
}

test.beforeAll(async ({ request }) => {
  // 文件 / 任务 / 记忆面板只在空间打开后才有真实数据；未打开时它们走 409 分支，
  // 量到的是错误态而不是默认布局。
  for (const wsId of OPEN_IDS) await openSpace(request, wsId);
});

test("首屏：/ 落到 /peco，无白屏与报错", async ({ browser }) => {
  const session = await openSession(browser, {
    viewport: WIDE,
    path: "/",
    openIds: OPEN_IDS,
    activeWsId: ACTIVE_WS,
    ready: '[data-testid="peco-panel"]',
  });
  try {
    // 白屏与报错都是异步发生的（先渲染骨架、再挂聊天壳），落定后再收一遍。
    await session.page.waitForTimeout(1500);
    const facts = await pageFacts(session.page);
    check("A1", "/ 重定向到 /peco", facts.url, "/peco");
    check(
      "A2",
      "#root 已挂载内容（子节点 ≥ 1）",
      facts.rootChildren >= 1,
      true,
      `实测 ${facts.rootChildren} 个子节点`,
    );
    check("A3", "main 内有渲染内容（白屏判据）", facts.mainChildren >= 1, true);
    check(
      "A4",
      "pageerror 数 = 0",
      session.pageErrors.length,
      0,
      session.pageErrors.join(" | "),
    );
    check(
      "A5",
      "console error 数 = 0",
      session.consoleErrors.length,
      0,
      session.consoleErrors.join(" | "),
    );
  } finally {
    await session.dispose();
  }
});

test("左栏：打开集渲染、主点击进聊天、状态页链接、× 只移出不关空间", async ({
  browser,
}) => {
  const session = await openSession(browser, {
    viewport: WIDE,
    path: "/peco",
    openIds: OPEN_IDS,
    activeWsId: ACTIVE_WS,
    ready: "[data-space-row][data-ws-id]",
  });
  const { page } = session;
  try {
    const rowCount = await until(
      async () => (await railFacts(page)).rows.length,
      (count) => count === OPEN_IDS.length,
    );
    const initial = await railFacts(page);
    check(
      "B1",
      "预置 3 个打开集 → 左栏渲染 3 行空间（Peco 自己有单独一行）",
      rowCount,
      OPEN_IDS.length,
      `实测行：${initial.rows.map((row) => row.label).join(" / ")}`,
    );
    check(
      "B2",
      "行顺序 = 打开集顺序（最近打开的不跳最前）",
      initial.rows.map((row) => row.wsId),
      OPEN_IDS,
    );
    check(
      "B3",
      "rail 宽度 = 208px",
      initial.rail?.width ?? null,
      208,
      `实测 ${initial.rail?.width}px`,
    );
    check("B4", "激活行 = 预置的 activeWsId", initial.active, [ACTIVE_WS]);

    const statusHref = await spaceRow(page, ACTIVE_WS)
      .getByRole("link", { name: /状态$/ })
      .getAttribute("href");
    check(
      "B5",
      "「状态」链接指向 /spaces/:wsId/status",
      statusHref,
      `/spaces/${ACTIVE_WS}/status`,
    );

    const target = "ws_demo_finance";
    await page.locator(`[data-space-row][data-ws-id="${target}"]`).click();
    const clicked = await until(
      () => pathOf(page),
      (path) => path === `/spaces/${target}`,
    );
    check(
      "B6",
      "行主点击进聊天：切到对应 /spaces/:wsId",
      clicked,
      `/spaces/${target}`,
    );
    const afterClick = await railFacts(page);
    check(
      "B7",
      "激活态跟着点击走（只有被点的那行 aria-current）",
      afterClick.active,
      [target],
    );

    let ring: { label: string; visible: boolean; text: string } | null = null;
    for (let step = 0; step < 20 && !ring; step += 1) {
      await page.keyboard.press("Tab");
      ring = await page.evaluate(() => {
        const element = document.activeElement as HTMLElement | null;
        if (!element?.closest("[data-space-row][data-ws-id]")) return null;
        const style = getComputedStyle(element);
        return {
          label: element.getAttribute("aria-label") ?? "",
          visible:
            style.outlineStyle !== "none" &&
            parseFloat(style.outlineWidth) >= 2,
          text: `${style.outlineStyle} ${style.outlineWidth} ${style.outlineColor} @${style.outlineOffset}`,
        };
      });
    }
    check(
      "B8",
      "键盘 Tab 到空间行时焦点环可见（outline-style ≠ none、宽度 ≥2px）",
      ring?.visible ?? false,
      true,
      ring
        ? `焦点「${ring.label}」outline ${ring.text}`
        : "连按 20 次 Tab 后焦点仍未落在空间行",
    );

    const before = await page.locator("[data-space-row][data-ws-id]").count();
    await spaceRow(page, target)
      .getByRole("button", { name: "从打开列表移除" })
      .click();
    const after = await until(
      () => page.locator("[data-space-row][data-ws-id]").count(),
      (count) => count === before - 1,
    );
    // 请求是异步发出的：点完立刻数会漏掉一拍之后才走的 /close。
    await page.waitForTimeout(300);
    check(
      "B9",
      "× 后左栏行数 -1",
      after,
      before - 1,
      `实测 ${before} → ${after}`,
    );
    check(
      "B10",
      "× 只移出打开集：期间没有发起关闭空间请求（/close 计数 = 0）",
      session.closeRequests.length,
      0,
      session.closeRequests.join(" | "),
    );
    // 被移出的正是刚点开的当前空间：激活态不能悬空，落到打开集里剩下的第一个。
    const remaining = OPEN_IDS.filter((id) => id !== target);
    const removedPath = await until(
      () => pathOf(page),
      (path) => path === `/spaces/${remaining[0]}`,
    );
    check(
      "B11",
      "× 移出的是当前激活空间：导航到第一个剩余打开空间的聊天页",
      removedPath,
      `/spaces/${remaining[0]}`,
      `被移出 ${target}，剩余打开集 ${format(remaining)}`,
    );
    // 激活态换行可能晚一拍：等它落到目标行再量，量到的仍是实测值。
    const afterRemove = await until(
      () => railFacts(page),
      (facts) => facts.active.length === 1 && facts.active[0] === remaining[0],
    );
    check(
      "B12",
      "× 移出的是当前激活空间：激活态落在第一个剩余打开空间那一行（aria-current）",
      afterRemove.active,
      [remaining[0]],
      `实测 aria-current 在 ${format(afterRemove.active)}`,
    );
  } finally {
    await session.dispose();
  }
});

test("设置固定底部：滚动打开集后按钮位置不变", async ({ browser, request }) => {
  const openIds = await listOpenableSpaceIds(request);
  const session = await openSession(browser, {
    viewport: SCROLLABLE,
    path: "/peco",
    openIds,
    activeWsId: openIds[0],
    ready: "[data-space-row][data-ws-id]",
  });
  const { page } = session;
  try {
    const count = await until(
      async () => (await railFacts(page)).rows.length,
      (value) => value === openIds.length,
    );
    const before = await railFacts(page);
    check(
      "C1",
      "预置的每个打开集都渲染成一行",
      count,
      openIds.length,
      `视口 ${SCROLLABLE.width}×${SCROLLABLE.height}`,
    );
    check(
      "C2",
      "打开集超出视口，左栏列表可滚动",
      (before.scroller?.scrollHeight ?? 0) >
        (before.scroller?.clientHeight ?? 0),
      true,
      `scrollHeight ${before.scroller?.scrollHeight} / clientHeight ${before.scroller?.clientHeight}`,
    );

    await scrollRailToBottom(page);
    const after = await railFacts(page);
    check(
      "C3",
      "列表确实滚到底（scrollTop = scrollHeight - clientHeight）",
      after.scroller?.scrollTop ?? -1,
      (before.scroller?.scrollHeight ?? 0) -
        (before.scroller?.clientHeight ?? 0),
    );
    check(
      "C4",
      "滚动后设置按钮位置不变（设置固定左栏底部）",
      (after.settings?.y ?? -1) - (before.settings?.y ?? -2),
      0,
      `y ${before.settings?.y} → ${after.settings?.y}`,
    );
    check(
      "C5",
      "滚动后设置按钮仍在视口内",
      (after.settings?.bottom ?? Number.POSITIVE_INFINITY) <= SCROLLABLE.height,
      true,
      `bottom ${after.settings?.bottom} / 视口高 ${SCROLLABLE.height}`,
    );
    check(
      "C6",
      "设置按钮命中区 = 24×24（图标按钮下限）",
      [after.settings?.width, after.settings?.height],
      [24, 24],
    );
  } finally {
    await session.dispose();
  }
});

test("团队页：卡片网格、四个计数、Peco 不在列表", async ({ browser }) => {
  const session = await openSession(browser, {
    viewport: WIDE,
    path: "/teams",
    openIds: OPEN_IDS,
    activeWsId: ACTIVE_WS,
    ready: "[data-team-id]",
  });
  try {
    const facts = await teamFacts(session.page);
    check(
      "D1",
      "团队卡片数 = 3（种子的法务 / 财务 / 运维）",
      facts.cards.length,
      3,
      facts.cards.map((card) => card.id).join(" / "),
    );
    check(
      "D2",
      "团队用卡片网格，列数 ≥ 2",
      facts.columns.length >= 2,
      true,
      `实测 ${facts.columns.length} 列：${facts.columns.join(" ")}`,
    );
    const legal = facts.cards.find((card) => card.id === "legal");
    check(
      "D3",
      "卡片显示 agents / skills / MCP / 空间 四个计数",
      Object.keys(legal?.stats ?? {}),
      ["agent", "skill", "mcp", "space"],
    );
    check(
      "D4",
      "legal 卡片空间计数 = 8（种子里每个团队 8 个空间）",
      legal?.stats.space ?? null,
      "8",
    );
    check(
      "D5",
      "四个计数都是数字，没有「还没告诉我」占位",
      Object.values(legal?.stats ?? {}).filter((value) => value === "–").length,
      0,
      `实测 ${format(Object.entries(legal?.stats ?? {}).map(([key, value]) => `${key}=${value}`))}`,
    );
    check(
      "D6",
      "Peco 不出现在团队列表",
      facts.cards.filter((card) => card.id === "peco").length,
      0,
    );
  } finally {
    await session.dispose();
  }
});

test("Peco 页：默认聊天 + 总览；·peco 链接进团队配置页", async ({
  browser,
}) => {
  const session = await openSession(browser, {
    viewport: WIDE,
    path: "/peco",
    openIds: OPEN_IDS,
    activeWsId: ACTIVE_WS,
    ready: '[data-testid="peco-chat-column"]',
  });
  const { page } = session;
  try {
    const initial = await until(
      () => pecoFacts(page),
      (facts) => facts.overviewPanels === 4,
    );
    const panel = await until(
      () => panelFacts(page),
      (facts) => facts !== null,
    );
    check(
      "E1",
      "默认聊天列在（peco-chat-column）",
      await page.locator('[data-testid="peco-chat-column"]').count(),
      1,
    );
    check("E2", "Peco 右栏默认「总览」", panel?.mode ?? null, "overview");
    check(
      "E3",
      "右栏占一半 856（含 border-l 落 857）",
      panel?.width ?? null,
      857,
      `实测 ${panel?.width}px`,
    );
    check(
      "E4",
      "总览四块面板都在（今日 / 空间 / 团队 / 成果）",
      initial.overviewPanels,
      4,
    );

    await page.getByRole("link", { name: "·peco" }).click();
    const config = await until(
      () => pecoFacts(page),
      (facts) => facts.url.startsWith("/teams/peco"),
    );
    check(
      "E6",
      "·peco 链接进团队配置页",
      config.url.split("?")[0],
      "/teams/peco",
    );
    check(
      "E7",
      "配置页有分类导航",
      config.tabs.length > 0,
      true,
      `实测 tab：${config.tabs.join(" / ")}`,
    );
    check(
      "E8",
      "Peco 团队没有「绑定空间」tab",
      config.tabs.includes("绑定空间"),
      false,
      `实测 tab：${config.tabs.join(" / ")}`,
    );
    check("E9", "Peco 团队没有「新建空间」入口", config.newSpaceButtons, 0);

    await page.goto("/teams/peco", { waitUntil: "load" });
    await page.waitForSelector("main h1");
    const direct = await until(
      () => pecoFacts(page),
      (facts) => facts.url === "/teams/peco",
    );
    check(
      "E10",
      "直达 /teams/peco 上屏即配置页（无「绑定空间」）",
      direct.tabs.includes("绑定空间"),
      false,
    );
    check("E11", "直达时 URL 保持 /teams/peco", direct.url, "/teams/peco");
  } finally {
    await session.dispose();
  }
});

test("空间页：默认文件、切任务后刷新仍任务、收起后一次点击进记忆", async ({
  browser,
  request,
}) => {
  const session = await openSession(browser, {
    viewport: WIDE,
    path: `/spaces/${SPACE_WS}`,
    openIds: OPEN_IDS,
    activeWsId: ACTIVE_WS,
    ready: '[data-testid="space-panel"]',
  });
  const { page } = session;
  // 右栏头部是 `role=tablist aria-label="右栏"`：模式按钮现在是 tab，不是 aria-pressed 按钮。
  const modeBar = page.getByRole("tablist", { name: "右栏" });
  try {
    const files = await until(
      () => panelFacts(page),
      (facts) => facts !== null,
    );
    check("F1", "空间页右栏默认「文件」", files?.mode ?? null, "files");
    check(
      "F2",
      "右栏占一半 856（含 border-l 落 857）",
      files?.width ?? null,
      857,
      `实测 ${files?.width}px`,
    );
    const entries = files?.entries ?? [];
    check(
      "F3",
      "文件面板读到播种的根目录条目（README.md 与 output/）",
      entries.some((entry) => entry.includes("README.md")) &&
        entries.some((entry) => entry.includes("output")),
      true,
      `实测 ${entries.length} 行：${entries.join(" / ")}`,
    );

    // tab 名可能带在飞角标（「任务 1」），按前缀匹配；只在这一组里找，不会撞到别处的 tab。
    await modeBar.getByRole("tab", { name: /^任务/ }).click();
    const tasks = await until(
      () => panelFacts(page),
      (facts) => facts?.mode === "tasks" && taskPanelSettled(facts),
    );
    check("F4", "点「任务」切到任务模式", tasks?.mode ?? null, "tasks");
    // 种子里这个空间被派发过任务而 Hub 里没有历史：快照是 200 加空数组，
    // 历史只在台账里 —— 面板必须回落台账，并说清看到的是摘要而不是实时。
    check(
      "F5",
      "实时快照为空时回落台账，并标注「摘要」（不是「实时」）",
      [
        tasks?.fidelity?.kind ?? "无标注",
        (tasks?.fidelity?.text ?? "").includes("摘要"),
      ],
      ["digest", true],
      tasks?.fidelity
        ? `实测标注：${tasks.fidelity.text.replace(/\s+/g, " ")}`
        : `面板顶部没有来源标注（实测片段：${(tasks?.text ?? "").replace(/\s+/g, " ").slice(0, 40)}）`,
    );
    // 期望条数取台账接口本身：断言的是「台账有几条就渲染几条」，不是抄一份种子条数。
    const ledger = await listSpaceLedgerTasks(request, SPACE_WS);
    check(
      "F11",
      "回落台账时把台账每一行都渲染出来（行数 = /tasks 台账条数）",
      (tasks?.entries ?? []).length,
      ledger.length,
      `空间 ${SPACE_WS} 台账 ${ledger.length} 条：${ledger
        .map((task) => task.result_digest ?? task.task_id)
        .join(" / ")}`,
    );

    await page.reload({ waitUntil: "load" });
    const reloaded = await until(
      () => panelFacts(page),
      (facts) => facts?.mode === "tasks",
    );
    check(
      "F6",
      "刷新后右栏仍是任务模式（模式按空间存 localStorage）",
      reloaded?.mode ?? null,
      "tasks",
    );

    await page.getByRole("button", { name: "收起右栏" }).click();
    const collapsed = await until(
      () => panelFacts(page),
      (facts) => facts === null,
    );
    check("F7", "收起后右栏不渲染", collapsed === null, true);

    // 收起态没有 tab 栏可点：回来路径在左头 —— 一枚「展开右栏」，点一次就回来。
    const expandVisible = await page
      .getByRole("button", { name: "展开右栏" })
      .isVisible();
    await page.getByRole("button", { name: "展开右栏" }).click();
    const reopened = await until(
      () => panelFacts(page),
      (facts) => facts !== null,
    );
    check(
      "F8",
      "收起态从左头「展开右栏」一次点击回到右栏（模式仍是任务）",
      [expandVisible, reopened?.mode ?? null],
      [true, "tasks"],
      `回来时 space-panel ${reopened === null ? "null" : `mode=${reopened.mode}`}`,
    );

    await modeBar.getByRole("tab", { name: "记忆", exact: true }).click();
    // 模式先落、清单后到：等空态文案出现再量，否则量到的是列表还在路上的那一帧。
    const memory = await until(
      () => panelFacts(page),
      (facts) =>
        facts?.mode === "memory" && facts.text.includes("本空间还没有记忆。"),
    );
    check(
      "F9",
      "右栏占一半 856（含 border-l 落 857）",
      memory?.width ?? null,
      857,
      `实测 ${memory?.width}px`,
    );
    check(
      "F10",
      "记忆面板空态写清「本空间还没有记忆」",
      (memory?.text ?? "").includes("本空间还没有记忆。"),
      true,
      `实测片段：${(memory?.text ?? "").replace(/\s+/g, " ").slice(0, 60)}`,
    );
  } finally {
    await session.dispose();
  }
});

test("空间页：实时快照与台账都为空时只剩空态，且不标来源", async ({
  browser,
  request,
}) => {
  const emptyWsId = await findSpaceWithoutLedgerTasks(request);
  // 必须真的把空间打开：没打开时 live 是 409，走的是另一条回落分支，
  // 量到的空态就证明不了「快照 200 但没有任务」这一条。
  await openSpace(request, emptyWsId);
  const session = await openSession(browser, {
    viewport: WIDE,
    path: `/spaces/${emptyWsId}`,
    openIds: [emptyWsId],
    activeWsId: emptyWsId,
    ready: '[data-testid="space-panel"]',
  });
  const { page } = session;
  try {
    await page
      .getByRole("tablist", { name: "右栏" })
      .getByRole("tab", { name: /^任务/ })
      .click();
    const tasks = await until(
      () => panelFacts(page),
      (facts) => facts?.mode === "tasks" && taskPanelSettled(facts),
    );
    // 三件事一起判：快照是 200 但 0 行、面板没挂来源标注、空态文案在。
    // 少了第一项，409（空间没打开）那条回落分支也能让这条断言变绿。
    const live = await liveTaskRowCount(request, emptyWsId);
    check(
      "F12",
      "实时快照为空且台账也为空：只剩空态，不挂「摘要」标注",
      [
        live.status,
        live.rows,
        tasks?.fidelity?.kind ?? "无标注",
        (tasks?.text ?? "").includes("这个空间还没有任务"),
      ],
      [200, 0, "无标注", true],
      `空间 ${emptyWsId}：live ${live.status} / ${live.rows} 行、台账 0 条、${
        tasks?.fidelity
          ? `面板却挂了标注「${tasks.fidelity.text.replace(/\s+/g, " ")}」`
          : "面板无来源标注"
      }`,
    );
  } finally {
    await session.dispose();
  }
});

test("溢出：1920×1080 与 900×800 的三种页面都没有横向溢出", async ({
  browser,
}) => {
  const cases: {
    id: string;
    viewport: { width: number; height: number };
    path: string;
    ready: string;
    /** 空间页两种独占态各量一次：折叠后单块吃满整宽是新的溢出高发面。 */
    collapse?: "chat" | "panel";
  }[] = [
    {
      id: "G1",
      viewport: WIDE,
      path: "/peco",
      ready: '[data-testid="peco-panel"]',
    },
    {
      id: "G2",
      viewport: WIDE,
      path: `/spaces/${SPACE_WS}`,
      ready: '[data-testid="space-panel"]',
    },
    { id: "G3", viewport: WIDE, path: "/teams", ready: "[data-team-id]" },
    {
      id: "G4",
      viewport: NARROW,
      path: "/peco",
      ready: '[data-testid="peco-panel"]',
    },
    {
      id: "G5",
      viewport: NARROW,
      path: `/spaces/${SPACE_WS}`,
      ready: '[data-testid="space-panel"]',
    },
    { id: "G6", viewport: NARROW, path: "/teams", ready: "[data-team-id]" },
    {
      id: "G7",
      viewport: WIDE,
      path: `/spaces/${SPACE_WS}`,
      ready: '[data-testid="space-panel"]',
      collapse: "panel",
    },
    {
      id: "G8",
      viewport: WIDE,
      path: `/spaces/${SPACE_WS}`,
      ready: '[data-testid="space-panel"]',
      collapse: "chat",
    },
  ];

  for (const entry of cases) {
    const session = await openSession(browser, {
      viewport: entry.viewport,
      path: entry.path,
      openIds: OPEN_IDS,
      activeWsId: ACTIVE_WS,
      ready: entry.ready,
    });
    try {
      if (entry.collapse) {
        await session.page
          .getByRole("button", {
            name: entry.collapse === "chat" ? "收起对话" : "收起右栏",
          })
          .click();
        await session.page.waitForTimeout(300);
      }
      const facts = await pageFacts(session.page);
      check(
        entry.id,
        `${entry.viewport.width}×${entry.viewport.height} 的 ${entry.path}${
          entry.collapse
            ? `（${entry.collapse === "chat" ? "收起对话" : "收起右栏"}后独占）`
            : ""
        } 无横向溢出（scrollWidth = clientWidth）`,
        [
          facts.html.scrollWidth - facts.html.clientWidth,
          facts.body.scrollWidth - facts.body.clientWidth,
        ],
        [0, 0],
        `html ${facts.html.scrollWidth}/${facts.html.clientWidth}、body ${facts.body.scrollWidth}/${facts.body.clientWidth}、main ${facts.main?.scrollWidth}/${facts.main?.clientWidth}`,
      );
    } finally {
      await session.dispose();
    }
  }
});

/**
 * 汇总放最后一条 test：读落盘的行而不是内存数组 —— 断言失败会换 worker 重新导入本文件。
 * 同一 id 以最后一次为准（重跑/重启都可能追加同名行）。
 */
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
