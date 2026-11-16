import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { BrandSection } from "@/components/settings/BrandSection";
import { getBrand, setBrand } from "@/api/brand";
import { uploadImage } from "@/api/upload";

// 像素处理留在纯函数测试里：这里只关心「先传后写」这条链路与按钮语义。
vi.mock("@/api/brand", () => ({ getBrand: vi.fn(), setBrand: vi.fn() }));
vi.mock("@/api/upload", () => ({ uploadImage: vi.fn() }));
vi.mock("@/lib/brandImage", async (importOriginal) => {
  const actual = await importOriginal<typeof import("@/lib/brandImage")>();
  return {
    ...actual,
    loadImageFile: vi.fn(async () => ({
      source: document.createElement("img"),
      width: 700,
      height: 200,
    })),
    exportBrandImage: vi.fn(
      async () => new Blob(["png"], { type: "image/png" }),
    ),
  };
});

function renderSection() {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  });
  return render(
    <QueryClientProvider client={client}>
      <BrandSection />
    </QueryClientProvider>,
  );
}

function fileInput(container: HTMLElement): HTMLInputElement {
  const input = container.querySelector<HTMLInputElement>('input[type="file"]');
  if (!input) throw new Error("没有文件选择框");
  return input;
}

async function openEditor(container: HTMLElement) {
  // 文件框跟着查询结果渲染：先等它落地，再模拟选图。
  await screen.findByRole("button", { name: /上传图片|更换/ });
  fireEvent.change(fileInput(container), {
    target: { files: [new File(["x"], "brand.png", { type: "image/png" })] },
  });
  return screen.findByRole("button", { name: "保存" });
}

beforeEach(() => {
  vi.clearAllMocks();
  // jsdom 没有 canvas：预览绘制直接跳过，用例只断言链路。
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue(null);
  vi.mocked(getBrand).mockResolvedValue({ image: "" });
  vi.mocked(setBrand).mockImplementation(async (image: string) => ({ image }));
  vi.mocked(uploadImage).mockResolvedValue("/uploads/brand/new.png");
});

describe("品牌区", () => {
  it("保存时先上传再写配置，写进去的是上传返回的 URL", async () => {
    const { container } = renderSection();

    fireEvent.click(await openEditor(container));
    fireEvent.click(screen.getByRole("button", { name: "保存" }));

    await waitFor(() =>
      expect(setBrand).toHaveBeenCalledWith("/uploads/brand/new.png"),
    );
    expect(uploadImage).toHaveBeenCalledWith(expect.any(Blob), "brand");
    expect(vi.mocked(uploadImage).mock.invocationCallOrder[0]).toBeLessThan(
      vi.mocked(setBrand).mock.invocationCallOrder[0],
    );
  });

  it("取消编辑器不写任何东西", async () => {
    const { container } = renderSection();

    await openEditor(container);
    fireEvent.click(screen.getByRole("button", { name: "取消" }));

    await waitFor(() =>
      expect(screen.queryByRole("button", { name: "保存" })).toBeNull(),
    );
    expect(uploadImage).not.toHaveBeenCalled();
    expect(setBrand).not.toHaveBeenCalled();
  });

  it("没设过图时只有「上传图片」，预览退回默认标记", async () => {
    const { container } = renderSection();

    expect(
      await screen.findByRole("button", { name: "上传图片" }),
    ).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "删除" })).toBeNull();
    expect(container.querySelectorAll('img[src="/favicon.svg"]').length).toBe(
      2,
    );
  });

  it("已有图时换成「更换」，删除写空串恢复默认", async () => {
    vi.mocked(getBrand).mockResolvedValue({ image: "/uploads/brand/old.png" });
    const { container } = renderSection();

    expect(
      await screen.findByRole("button", { name: "更换" }),
    ).toBeInTheDocument();
    expect(
      container.querySelectorAll('img[src="/uploads/brand/old.png"]').length,
    ).toBe(2);

    fireEvent.click(screen.getByRole("button", { name: "删除" }));

    await waitFor(() => expect(setBrand).toHaveBeenCalledWith(""));
  });
});
