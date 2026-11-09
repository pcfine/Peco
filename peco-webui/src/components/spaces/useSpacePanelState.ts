// 右栏的模式与两块的折叠态按空间各记一份：回到某个空间时看到的是上次离开时的样子。
//
// 只存 UI 偏好，不进 query 缓存 —— 它与 daemon 的状态无关，刷新页面也不该丢。
//
// 模式同时能由 URL 的 `?panel=` 给出：它是「正在看哪一栏」，值得被分享和深链，
// 于是压过存档；折叠是偏好而不是可分享状态，只走存档。

import { useCallback, useState } from "react";

export type SpacePanelMode = "overview" | "files" | "tasks" | "memory";

/** Peco 页与空间页共用一个右栏，可选模式与默认落点不同。 */
export type SpacePanelVariant = "space" | "peco";

/** 按钮顺序：任务与记忆是「去看发生了什么」；总览只属于 Peco，所以排在最后。 */
export const SPACE_PANEL_MODES: Record<
  SpacePanelVariant,
  readonly SpacePanelMode[]
> = {
  space: ["tasks", "memory", "files"],
  peco: ["tasks", "memory", "files", "overview"],
};

export const SPACE_PANEL_LABELS: Record<SpacePanelMode, string> = {
  overview: "总览",
  files: "文件",
  tasks: "任务",
  memory: "记忆",
};

export interface SpacePanelState {
  mode: SpacePanelMode;
  /** 右栏是否收起。 */
  collapsed: boolean;
  /** 聊天列是否收起。两块同藏等于白屏，所以它压过右栏的折叠。 */
  chatCollapsed: boolean;
}

function defaultPanel(variant: SpacePanelVariant): SpacePanelState {
  return {
    mode: variant === "peco" ? "overview" : "files",
    collapsed: false,
    chatCollapsed: false,
  };
}

function storageKey(wsId: string): string {
  return `peco-webui:space-panel:${wsId}`;
}

function isModeOf(
  variant: SpacePanelVariant,
  value: unknown,
): value is SpacePanelMode {
  return SPACE_PANEL_MODES[variant].includes(value as SpacePanelMode);
}

/** 存档读不出来就用默认态：一份坏掉的偏好不该让页面变成不可用。 */
function readPanel(wsId: string, variant: SpacePanelVariant): SpacePanelState {
  try {
    const raw = localStorage.getItem(storageKey(wsId));
    if (!raw) return defaultPanel(variant);
    const parsed: unknown = JSON.parse(raw);
    if (typeof parsed !== "object" || parsed === null)
      return defaultPanel(variant);
    const { mode, collapsed, chatCollapsed } = parsed as {
      mode?: unknown;
      collapsed?: unknown;
      chatCollapsed?: unknown;
    };
    const fallback = defaultPanel(variant);
    return {
      // 存档可能来自另一个变体或旧版本：这个变体不认的模式回默认落点。
      mode: isModeOf(variant, mode) ? mode : fallback.mode,
      collapsed:
        typeof collapsed === "boolean" ? collapsed : fallback.collapsed,
      chatCollapsed:
        typeof chatCollapsed === "boolean"
          ? chatCollapsed
          : fallback.chatCollapsed,
    };
  } catch {
    // 隐私模式会禁用 localStorage；旧版本写下的 JSON 也可能已经解析不了。
    return defaultPanel(variant);
  }
}

function savePanel(wsId: string, panel: SpacePanelState): void {
  try {
    localStorage.setItem(storageKey(wsId), JSON.stringify(panel));
  } catch {
    // 存不下只意味着下次回到默认态，本次使用不受影响。
  }
}

export interface SpacePanelOptions {
  /**
   * 视口放不下两列：两块互斥且聊天优先，右栏只在聊天收起后才占位。
   * 开关本身仍是偏好，不随视口变化改写存档。
   */
  exclusive?: boolean;
  /** URL 的 `?panel=` 原值：合法时优先于存档，让「正在看哪一栏」可分享、可深链。 */
  modeFromUrl?: string | null;
  /** 模式变了才写 URL —— 幂等的点击不该多一次 `replaceState`。 */
  onModeChange?: (mode: SpacePanelMode) => void;
}

export interface SpacePanelControls {
  mode: SpacePanelMode;
  /** 右栏此刻是否收起（含 `exclusive` 的派生结果，直接决定渲染）。 */
  collapsed: boolean;
  chatCollapsed: boolean;
  /** 点任一模式都是「去看这个模式」：收起时先展开再切。 */
  selectMode: (mode: SpacePanelMode) => void;
  /** 收起 / 展开右栏。收起时聊天接位，展开时窄屏把聊天让出去。 */
  toggleCollapsed: () => void;
  /** 收起 / 展开聊天列。收起时右栏接位，展开时窄屏把右栏让回去。 */
  toggleChat: () => void;
}

export function useSpacePanelState(
  wsId: string,
  variant: SpacePanelVariant = "space",
  options: SpacePanelOptions = {},
): SpacePanelControls {
  const { exclusive = false, modeFromUrl = null, onModeChange } = options;

  const [stored, setStored] = useState(() => ({
    wsId,
    panel: readPanel(wsId, variant),
  }));

  // 换空间不重挂页面：在渲染期把状态换成新空间的存档，避免先渲染一帧
  // 上一个空间的面板。
  if (stored.wsId !== wsId)
    setStored({ wsId, panel: readPanel(wsId, variant) });
  const panel = stored.wsId === wsId ? stored.panel : readPanel(wsId, variant);

  // URL 里的模式压过存档；参数值不属于这个变体（`overview` 之于空间页、
  // 手改坏的串）就当没给，回落到存档与默认。
  const mode = isModeOf(variant, modeFromUrl) ? modeFromUrl : panel.mode;

  // 两块同藏等于白屏，所以聊天的折叠压过右栏的折叠：聊天收起来时右栏一定在。
  // 窄屏上再叠一层「聊天优先」：默认只摆聊天，右栏要等聊天让位。
  const collapsed = !panel.chatCollapsed && (exclusive || panel.collapsed);

  const write = useCallback(
    (next: SpacePanelState) => {
      setStored({ wsId, panel: next });
      savePanel(wsId, next);
    },
    [wsId],
  );

  const selectMode = useCallback(
    (next: SpacePanelMode) => {
      // 用户点的是「看任务」而不是「切模式」：收起态下先展开。
      write({ ...panel, mode: next, collapsed: false });
      if (next !== mode) onModeChange?.(next);
    },
    [write, panel, mode, onModeChange],
  );

  return {
    mode,
    collapsed,
    chatCollapsed: panel.chatCollapsed,
    selectMode,
    // 展开一块时，窄屏只能留它自己；另一块原本就收着的话也照旧收着。
    toggleCollapsed: useCallback(
      () =>
        write(
          collapsed
            ? {
                ...panel,
                collapsed: false,
                chatCollapsed: panel.chatCollapsed || exclusive,
              }
            : { ...panel, collapsed: true, chatCollapsed: false },
        ),
      [write, panel, collapsed, exclusive],
    ),
    toggleChat: useCallback(
      () =>
        write(
          panel.chatCollapsed
            ? {
                ...panel,
                chatCollapsed: false,
                collapsed: exclusive || panel.collapsed,
              }
            : { ...panel, chatCollapsed: true, collapsed: false },
        ),
      [write, panel, exclusive],
    ),
  };
}
