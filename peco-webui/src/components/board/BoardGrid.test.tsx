import { beforeEach, describe, expect, it } from "vitest";
import { fireEvent, render, screen } from "@testing-library/react";
import { BoardGrid } from "@/components/board/BoardGrid";
import { BoardEditorPopover } from "@/components/board/BoardEditorPopover";
import { DEFAULT_BOARD, normalizeBoard } from "@/lib/board";
import { useBoardStore } from "@/stores/boardStore";

const PANELS = {
  today: <div>今日</div>,
  spaces: <div>空间</div>,
  teams: <div>团队</div>,
  results: <div>成果</div>,
};

function storedBoard() {
  return JSON.parse(localStorage.getItem("peco-webui:board:v1") ?? "null");
}

beforeEach(() => {
  localStorage.clear();
  useBoardStore.setState({ ...DEFAULT_BOARD });
});

describe("看板组合", () => {
  it("隐藏成果流后它从栅格消失，并立即落盘", () => {
    useBoardStore.getState().toggle("results");
    const { queryByTestId } = render(<BoardGrid panels={PANELS} />);

    expect(queryByTestId("panel-results")).toBeNull();
    expect(queryByTestId("panel-today")).not.toBeNull();
    expect(storedBoard()).toEqual({
      order: ["today", "spaces", "teams", "results"],
      hidden: ["results"],
    });
  });

  it("只剩一块可见时，它的开关被禁用并给出理由", () => {
    useBoardStore.setState({
      order: DEFAULT_BOARD.order,
      hidden: ["spaces", "teams", "results"],
    });
    render(<BoardEditorPopover />);

    fireEvent.click(screen.getByRole("button", { name: /调整看板/ }));

    const lastSwitch = screen.getByRole("switch", { name: "显示今日任务" });
    expect(lastSwitch).toBeDisabled();
    expect(lastSwitch).toHaveAttribute("title", "至少保留一块面板");
  });

  it("未知 id 不渲染，缺失的面板补到末尾", () => {
    const normalized = normalizeBoard({
      order: ["nope", "today"],
      hidden: [],
    });
    useBoardStore.setState({ ...normalized });
    const { queryByTestId, getAllByTestId } = render(
      <BoardGrid panels={PANELS} />,
    );

    expect(queryByTestId("panel-nope")).toBeNull();
    expect(queryByTestId("panel-today")).not.toBeNull();
    // 缺失的三块补到末尾：总数仍是 4（hidden 为空）。
    expect(getAllByTestId(/^panel-/)).toHaveLength(4);
  });
});
