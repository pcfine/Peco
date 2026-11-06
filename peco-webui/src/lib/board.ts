// 看板组合：顺序 + 隐藏，存 localStorage。读的时候要能吃下未知 id 与缺失 id ——
// 面板增删是常态，坏的存档不该让看板整块消失。

export const BOARD_PANEL_IDS = ["today", "spaces", "teams", "results"] as const;

export type BoardPanelId = (typeof BOARD_PANEL_IDS)[number];

/** 面板的中文名：只有这里有，不散落到各面板组件里。 */
export const BOARD_PANEL_LABELS: Record<BoardPanelId, string> = {
  today: "今日任务",
  spaces: "空间状态",
  teams: "团队概览",
  results: "成果流",
};

export interface BoardConfig {
  order: BoardPanelId[];
  hidden: BoardPanelId[];
}

const STORAGE_KEY = "peco-webui:board:v1";

export const DEFAULT_BOARD: BoardConfig = {
  order: [...BOARD_PANEL_IDS],
  hidden: [],
};

export function isPanelId(value: unknown): value is BoardPanelId {
  return BOARD_PANEL_IDS.includes(value as BoardPanelId);
}

/**
 * 归一化一份存档。
 *
 * - 未知 id 丢弃（旧版本留下的面板 id）；
 * - 缺失的 id 追加到末尾并保持可见（新版本加的面板默认出现）；
 * - 重复 id 只留第一次出现的位置；
 * - `hidden` 里全命中时放开第一块：至少留一块可见是面板自身的约束。
 */
export function normalizeBoard(raw: unknown): BoardConfig {
  const source = (raw ?? {}) as { order?: unknown; hidden?: unknown };
  const rawOrder = Array.isArray(source.order) ? source.order : [];
  const seen = new Set<BoardPanelId>();
  const order: BoardPanelId[] = [];

  for (const item of rawOrder) {
    if (isPanelId(item) && !seen.has(item)) {
      seen.add(item);
      order.push(item);
    }
  }
  for (const id of BOARD_PANEL_IDS) {
    if (!seen.has(id)) order.push(id);
  }

  const rawHidden = Array.isArray(source.hidden) ? source.hidden : [];
  const hidden = order.filter((id) => rawHidden.includes(id));
  // 全是隐藏的存档（手工改过 / 旧约束不同）会让看板空白：放开第一块。
  if (hidden.length === order.length) return { order, hidden: hidden.slice(1) };

  return { order, hidden };
}

export function loadBoard(): BoardConfig {
  try {
    const stored = localStorage.getItem(STORAGE_KEY);
    if (!stored) return normalizeBoard(DEFAULT_BOARD);
    return normalizeBoard(JSON.parse(stored));
  } catch {
    // 存档坏了就用默认布局，不把看板卡死在空态。
    return normalizeBoard(DEFAULT_BOARD);
  }
}

export function saveBoard(config: BoardConfig): void {
  localStorage.setItem(STORAGE_KEY, JSON.stringify(config));
}

/** 可见面板，按用户顺序。 */
export function visiblePanels(config: BoardConfig): BoardPanelId[] {
  return config.order.filter((id) => !config.hidden.includes(id));
}

/** 隐藏/显示一块；关掉最后一块可见面板时不动（面板开关据此置 disabled）。 */
export function togglePanel(
  config: BoardConfig,
  id: BoardPanelId,
): BoardConfig {
  const isHidden = config.hidden.includes(id);
  if (!isHidden && visiblePanels(config).length <= 1) return config;
  return {
    order: config.order,
    hidden: isHidden
      ? config.hidden.filter((item) => item !== id)
      : [...config.hidden, id],
  };
}

/** 与相邻面板换位；到头则原样返回（首行 ↑ / 末行 ↓ 据此置 disabled）。 */
export function movePanel(
  config: BoardConfig,
  id: BoardPanelId,
  direction: -1 | 1,
): BoardConfig {
  const index = config.order.indexOf(id);
  const target = index + direction;
  if (index < 0 || target < 0 || target >= config.order.length) return config;
  const order = [...config.order];
  order[index] = order[target];
  order[target] = id;
  return { order, hidden: config.hidden };
}
