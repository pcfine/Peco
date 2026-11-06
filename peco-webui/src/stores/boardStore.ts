// 看板组合的单一来源：顶栏的「调整看板」与看板本体读同一个 store，
// 否则开关点下去页面要等一次 remount 才生效。

import { create } from "zustand";
import {
  loadBoard,
  movePanel,
  saveBoard,
  togglePanel,
  type BoardPanelId,
} from "@/lib/board";

interface BoardState {
  order: BoardPanelId[];
  hidden: BoardPanelId[];
  toggle: (id: BoardPanelId) => void;
  move: (id: BoardPanelId, direction: -1 | 1) => void;
}

const initial = loadBoard();

export const useBoardStore = create<BoardState>()((set, get) => {
  /** 每次改动立即落盘：看板没有保存按钮，刷新后必须还是用户调过的样子。 */
  const commit = (next: { order: BoardPanelId[]; hidden: BoardPanelId[] }) => {
    saveBoard(next);
    set(next);
  };

  return {
    order: initial.order,
    hidden: initial.hidden,
    toggle: (id) => commit(togglePanel(get(), id)),
    move: (id, direction) => commit(movePanel(get(), id, direction)),
  };
});
