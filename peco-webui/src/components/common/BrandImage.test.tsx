import { describe, expect, it } from "vitest";
import { fireEvent, render } from "@testing-library/react";
import { BrandImage } from "@/components/common/BrandImage";

const IMAGE = "/uploads/brand/x.png";

function firstImage(container: HTMLElement): HTMLImageElement {
  const img = container.querySelector("img");
  if (!img) throw new Error("没有渲染出 img");
  return img;
}

describe("BrandImage", () => {
  it("折叠态：容器裁溢出，内层按 2 倍宽度渲染再压缩", () => {
    const { container } = render(<BrandImage image={IMAGE} collapsed />);
    const img = firstImage(container);
    const box = img.parentElement as HTMLElement;

    expect(box.className).toContain("overflow-hidden");
    expect(box.style.width).toBe("32px");
    expect(box.style.height).toBe("32px");

    expect(img.getAttribute("src")).toBe(IMAGE);
    // 2 倍宽度 + 靠左对齐 = 可见区域正好是原图左半。
    expect(img.className).toContain("w-[200%]");
    expect(img.className).toContain("object-fill");
    expect(img.className).toContain("absolute");
    expect(img.className).toContain("top-0");
    expect(img.className).toContain("left-0");
    // preflight 的 max-width:100% 会把 200% 压回去，必须显式放开。
    expect(img.className).toContain("max-w-none");
  });

  it("展开态：整图装进 176×32，等比缩放到框内", () => {
    const { container } = render(
      <BrandImage image={IMAGE} collapsed={false} />,
    );
    const img = firstImage(container);

    expect(img.style.width).toBe("176px");
    expect(img.style.height).toBe("32px");
    expect(img.className).toContain("object-contain");
    expect(img.className).not.toContain("w-[200%]");
  });

  it("未设置时退回默认标记，外观与改动前一致", () => {
    const { container } = render(<BrandImage image="" collapsed={false} />);
    const img = firstImage(container);

    expect(img.getAttribute("src")).toBe("/favicon.svg");
    expect(img.style.width).toBe("24px");
    expect(img.style.height).toBe("24px");
  });

  it("图片加载失败也退回默认标记，不留破图图标", () => {
    const { container } = render(<BrandImage image={IMAGE} collapsed />);

    fireEvent.error(firstImage(container));

    const img = firstImage(container);
    expect(img.getAttribute("src")).toBe("/favicon.svg");
    expect(img.style.width).toBe("24px");
  });
});
