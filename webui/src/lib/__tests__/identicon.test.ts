import { describe, expect, it } from "vitest";
import {
  IDENTICON_GRID,
  createIdenticon,
  pixelIconValue,
  resolveAgentIcon,
} from "../identicon";

const SEED_A = "3fa9c1d2";
const SEED_B = "8b7e0a55";

describe("像素图标", () => {
  it("同一 seed 生成完全相同的图案与配色", () => {
    expect(createIdenticon(SEED_A)).toEqual(createIdenticon(SEED_A));
  });

  it("不同 seed 得到不同图案", () => {
    const a = createIdenticon(SEED_A);
    const b = createIdenticon(SEED_B);
    expect(a.cells).not.toEqual(b.cells);
  });

  it("左右镜像对称", () => {
    for (const seed of [SEED_A, SEED_B, "agent"]) {
      const { cells } = createIdenticon(seed);
      expect(cells).toHaveLength(IDENTICON_GRID);
      for (const row of cells) {
        expect(row).toHaveLength(IDENTICON_GRID);
        for (let i = 0; i < IDENTICON_GRID; i++) {
          expect(row[i]).toBe(row[IDENTICON_GRID - 1 - i]);
        }
      }
    }
  });

  it("图案不会全空", () => {
    for (let i = 0; i < 50; i++) {
      const { cells } = createIdenticon(`seed-${i}`);
      expect(cells.flat().filter(Boolean).length).toBeGreaterThanOrEqual(7);
    }
  });
});

describe("icon 字段解析", () => {
  it("上传图按图片渲染", () => {
    const r = resolveAgentIcon("/uploads/abc.png", "任意");
    expect(r.kind).toBe("image");
    expect(r.value).toBe("/uploads/abc.png");
  });

  it("pixel: 前缀取种子", () => {
    const r = resolveAgentIcon(pixelIconValue(SEED_A), "任意");
    expect(r.kind).toBe("pixel");
    expect(r.value).toBe(SEED_A);
    expect(r.identicon).toEqual(createIdenticon(SEED_A));
  });

  it("空值与历史默认 🤖 都退化为按名称生成的像素图标", () => {
    for (const icon of ["", "🤖", undefined]) {
      const r = resolveAgentIcon(icon, "设计助手");
      expect(r.kind).toBe("pixel");
      expect(r.value).toBe("设计助手");
      expect(r.identicon).toEqual(createIdenticon("设计助手"));
    }
  });

  it("其它字符仍按 emoji 渲染", () => {
    expect(resolveAgentIcon("🧠", "任意").kind).toBe("emoji");
  });
});
