// [浏览器] UI 审计：只测量、不改期望值。输出每个路由的字号/对比度/点击区/边框/圆角/溢出。
import { test, type APIRequestContext } from "@playwright/test";

const OPEN_IDS = ["ws_demo_legal", "ws_demo_finance", "ws_demo_ops"];

const AUDIT = () => {
  const visible = (el: Element) => {
    const r = el.getBoundingClientRect();
    const cs = getComputedStyle(el);
    return r.width > 0 && r.height > 0 && cs.visibility !== "hidden" && cs.display !== "none" && +cs.opacity > 0.05;
  };
  const parse = (c: string) => {
    const m = c.match(/rgba?\(([^)]+)\)/);
    if (!m) return null;
    const p = m[1].split(/[,\s/]+/).filter(Boolean).map(Number);
    return { r: p[0], g: p[1], b: p[2], a: p.length > 3 ? p[3] : 1 };
  };
  const blend = (top: any, bot: any) => ({
    r: top.r * top.a + bot.r * (1 - top.a),
    g: top.g * top.a + bot.g * (1 - top.a),
    b: top.b * top.a + bot.b * (1 - top.a),
    a: 1,
  });
  const bgOf = (el: Element) => {
    let acc: any = null;
    let cur: Element | null = el;
    while (cur) {
      const c = parse(getComputedStyle(cur).backgroundColor);
      if (c && c.a > 0) acc = acc ? blend(acc, c) : c;
      if (acc && acc.a >= 0.999) break;
      cur = cur.parentElement;
    }
    return acc ?? { r: 255, g: 255, b: 255, a: 1 };
  };
  const lum = (c: any) => {
    const f = (v: number) => {
      v /= 255;
      return v <= 0.03928 ? v / 12.92 : Math.pow((v + 0.055) / 1.055, 2.4);
    };
    return 0.2126 * f(c.r) + 0.7152 * f(c.g) + 0.0722 * f(c.b);
  };
  const ratio = (a: any, b: any) => {
    const l1 = lum(a), l2 = lum(b);
    return (Math.max(l1, l2) + 0.05) / (Math.min(l1, l2) + 0.05);
  };

  const all = Array.from(document.querySelectorAll("body *")).filter(visible);
  const fonts: Record<string, number> = {};
  const contrasts: any[] = [];
  const borderEls: string[] = [];
  const radii: Record<string, number> = {};
  let textNodes = 0;

  for (const el of all) {
    const cs = getComputedStyle(el);
    const fs = Math.round(parseFloat(cs.fontSize) * 10) / 10;
    const direct = Array.from(el.childNodes).some((n) => n.nodeType === 3 && n.textContent!.trim().length > 1);
    if (direct) {
      textNodes++;
      fonts[fs] = (fonts[fs] ?? 0) + 1;
      const fg = parse(cs.color);
      if (fg && fg.a > 0.3) {
        const bg = bgOf(el);
        const r = ratio(fg, bg);
        const big = fs >= 18.66 || (fs >= 14 && +cs.fontWeight >= 700);
        if (r < (big ? 3 : 4.5)) {
          contrasts.push({ text: el.textContent!.trim().slice(0, 28), fs, fg: cs.color, bg: `rgb(${Math.round(bg.r)},${Math.round(bg.g)},${Math.round(bg.b)})`, ratio: +r.toFixed(2), cls: (el.className || "").toString().slice(0, 60) });
        }
      }
    }
    const bw = parseFloat(cs.borderTopWidth) + parseFloat(cs.borderRightWidth) + parseFloat(cs.borderBottomWidth) + parseFloat(cs.borderLeftWidth);
    if (bw > 0 && cs.borderTopStyle !== "none") borderEls.push(el.tagName + "." + (el.className || "").toString().split(" ").slice(0, 2).join("."));
    if (cs.borderRadius !== "0px") radii[cs.borderRadius] = (radii[cs.borderRadius] ?? 0) + 1;
  }

  const targets: any[] = [];
  for (const el of Array.from(document.querySelectorAll("a,button,[role=button],input,select,textarea,[tabindex]"))) {
    if (!visible(el)) continue;
    const r = el.getBoundingClientRect();
    if (r.width < 24 || r.height < 24) targets.push({ tag: el.tagName, label: (el.getAttribute("aria-label") || el.textContent || "").trim().slice(0, 20), w: Math.round(r.width), h: Math.round(r.height) });
  }

  const overflow: any[] = [];
  for (const el of all) {
    const cs = getComputedStyle(el);
    if (/auto|scroll|hidden|clip/.test(cs.overflowX)) continue;
    if (el.scrollWidth > el.clientWidth + 2 && el.clientWidth > 0) overflow.push({ tag: el.tagName, cls: (el.className || "").toString().slice(0, 70), scroll: el.scrollWidth, client: el.clientWidth });
  }

  const clickables = Array.from(document.querySelectorAll("a,button,[role=button]")).filter(visible).length;
  const smallText = Object.entries(fonts).filter(([k]) => +k <= 11).reduce((a, [, v]) => a + v, 0);

  return {
    docOverflowX: document.documentElement.scrollWidth - document.documentElement.clientWidth,
    textNodes,
    clickables,
    fontSizes: Object.fromEntries(Object.entries(fonts).sort((a, b) => +a[0] - +b[0])),
    distinctFontSizes: Object.keys(fonts).length,
    shareLe11: +(smallText / Math.max(textNodes, 1)).toFixed(2),
    contrastFails: contrasts.length,
    contrastWorst: contrasts.sort((a, b) => a.ratio - b.ratio).slice(0, 6),
    smallTargets: targets.length,
    smallTargetSamples: targets.slice(0, 6),
    borderedEls: borderEls.length,
    topBordered: Object.entries(borderEls.reduce((a: any, k) => ((a[k] = (a[k] ?? 0) + 1), a), {})).sort((a: any, b: any) => b[1] - a[1]).slice(0, 6),
    radii,
    overflowEls: overflow.length,
    overflowSamples: overflow.slice(0, 5),
  };
};

test("UI audit metrics", async ({ page, request }) => {
  test.setTimeout(180_000);
  await page.addInitScript((ids) => {
    localStorage.setItem("peco-webui:theme", JSON.stringify({ state: { theme: "dark" }, version: 0 }));
    localStorage.setItem("peco-webui:rail", JSON.stringify({ state: { railOpen: true }, version: 0 }));
    localStorage.setItem("peco-webui:open-spaces", JSON.stringify({ state: { openIds: ids, activeWsId: ids[0], seeded: true }, version: 0 }));
  }, OPEN_IDS);

  const spaces = await (await request.get("/api/spaces")).json();
  const teams = await (await request.get("/api/teams")).json();
  const wsId = spaces?.[0]?.ws_id ?? OPEN_IDS[0];
  const teamId = teams?.[0]?.team_id ?? "default";

  const routes = [
    ["peco", "/peco"],
    ["team-peco", "/teams/peco"],
    ["spaces", "/spaces"],
    ["space", `/spaces/${wsId}`],
    ["space-status", `/spaces/${wsId}/status`],
    ["teams", "/teams"],
    ["team", `/teams/${teamId}`],
    ["results", "/results"],
    ["settings", "/settings"],
  ];

  const out: any = {};
  for (const [name, path] of routes) {
    await page.setViewportSize({ width: 1440, height: 900 });
    await page.goto(path, { waitUntil: "networkidle" });
    await page.waitForTimeout(700);
    out[name] = await page.evaluate(AUDIT);
    await page.screenshot({ path: `e2e/.report/audit-${name}.png`, fullPage: false });
  }
  console.log("AUDIT_JSON_START");
  console.log(JSON.stringify({ wsId, teamId, routes: out }, null, 1));
  console.log("AUDIT_JSON_END");
});
