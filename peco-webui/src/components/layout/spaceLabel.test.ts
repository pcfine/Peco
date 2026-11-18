import { describe, expect, it } from "vitest";
import { spaceInitial, spaceLabel } from "@/components/layout/spaceLabel";
import type { Space } from "@/types/daemon";

// 优先级 name → label → 目录末段：顺序钉住，退了就会把「客户合同审阅」显示成
// 目录名。只喂声明要的三项，多给不该影响结果，少给（缺 name、mock）也得能算。

type Names = Pick<Space, "name" | "label" | "work_dir">;

function names(overrides: Partial<Names> = {}): Names {
  return {
    name: null,
    label: null,
    work_dir: "/home/you/work/client-contract",
    ...overrides,
  };
}

describe("空间显示名", () => {
  it("name 压过 label：marker 的人读名是用户填的那个", () => {
    expect(
      spaceLabel(names({ name: "客户合同审阅", label: "client-contract" })),
    ).toBe("客户合同审阅");
  });

  it("没有 name 就退 label：旧空间只有库里的逻辑名", () => {
    expect(spaceLabel(names({ label: "client-contract" }))).toBe(
      "client-contract",
    );
  });

  it("name 与 label 都缺，退目录末段（不是 ws_id）", () => {
    expect(spaceLabel(names())).toBe("client-contract");
  });

  it("空串与纯空白视同缺失：mock 与缺 name 的响应不带后端那层过滤", () => {
    // 空串不能让标题变空，也不能挡住 label。
    expect(spaceLabel(names({ name: "", label: "client-contract" }))).toBe(
      "client-contract",
    );
    // 纯空白同理，label 也空时退到目录末段。
    expect(spaceLabel(names({ name: "   ", label: "\t " }))).toBe(
      "client-contract",
    );
    // 两侧空白不进标题。
    expect(spaceLabel(names({ name: " 客户合同审阅 " }))).toBe("客户合同审阅");
  });

  it("work_dir 末尾带斜杠，末段仍是最后那个目录名", () => {
    expect(
      spaceLabel(names({ work_dir: "/home/you/work/client-contract/" })),
    ).toBe("client-contract");
    // 连续斜杠与根目录都不该算出空串。
    expect(spaceLabel(names({ work_dir: "/srv//client-contract//" }))).toBe(
      "client-contract",
    );
    expect(spaceLabel(names({ work_dir: "/" }))).toBe("/");
  });

  it("收起态单字标记跟着显示名走：name 优先", () => {
    expect(spaceInitial(names({ name: "客户合同审阅", label: "x" }))).toBe(
      "客",
    );
    expect(spaceInitial(names({ label: "Peco" }))).toBe("P");
  });

  it("emoji 是代理对：按码点取首字，不切半个", () => {
    expect(spaceInitial(names({ name: "🎯 目标" }))).toBe("🎯");
  });

  it("连 work_dir 都空时退原样，单字标记给占位点", () => {
    const empty = names({ work_dir: "" });
    expect(spaceLabel(empty)).toBe("");
    expect(spaceInitial(empty)).toBe("·");
  });
});
