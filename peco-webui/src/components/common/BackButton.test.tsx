import { describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen } from "@testing-library/react";
import { BackButton } from "@/components/common/BackButton";

describe("BackButton", () => {
  it("无障碍名与 tooltip 同源：都取 label", () => {
    render(<BackButton label="返回列表" onClick={() => {}} />);
    const button = screen.getByRole("button", { name: "返回列表" });
    expect(button.getAttribute("title")).toBe("返回列表");
  });

  it("图标固定为 < 形 chevron，不接受带尾巴的箭头", () => {
    render(<BackButton label="返回列表" onClick={() => {}} />);
    const svg = screen
      .getByRole("button", { name: "返回列表" })
      .querySelector("svg");
    if (!svg) throw new Error("没有渲染出 svg");

    const classes = svg.getAttribute("class") ?? "";
    expect(classes).toContain("lucide-chevron-left");
    expect(classes).not.toContain("lucide-arrow-left");
    // 装饰性图标，名字由按钮的 aria-label 承担。
    expect(svg.getAttribute("aria-hidden")).toBe("true");
  });

  it("点击触发 onClick", () => {
    const onClick = vi.fn();
    render(<BackButton label="返回列表" onClick={onClick} />);

    fireEvent.click(screen.getByRole("button", { name: "返回列表" }));
    expect(onClick).toHaveBeenCalledTimes(1);
  });

  it("className 追加而不是覆盖基色", () => {
    render(
      <BackButton
        label="返回列表"
        onClick={() => {}}
        className="absolute left-0"
      />,
    );

    const button = screen.getByRole("button", { name: "返回列表" });
    expect(button.className).toContain("text-ink-muted");
    expect(button.className).toContain("absolute");
    expect(button.className).toContain("left-0");
  });
});
