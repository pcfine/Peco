import { describe, expect, it } from "vitest";
import { render, screen } from "@testing-library/react";
import { SchedulesPage } from "@/pages/SchedulesPage";

// A25：占位页不留可点控件 —— 一个点不动的 cron 表单比一句实话更误导。
describe("定时任务占位页", () => {
  it("内容区没有按钮，文案说清本期未实现", () => {
    const { container } = render(<SchedulesPage />);
    expect(screen.getByText(/本期尚未实现/)).toBeInTheDocument();
    expect(screen.getByText(/后续版本/)).toBeInTheDocument();
    expect(container.querySelectorAll("button")).toHaveLength(0);
  });
});
