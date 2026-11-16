import { describe, expect, it } from "vitest";
import {
  BRAND_FRAME_HEIGHT,
  BRAND_FRAME_WIDTH,
  computeDrawRect,
  type BrandDrawRect,
  type BrandDrawRectParams,
} from "@/lib/brandImage";

const targetW = BRAND_FRAME_WIDTH;
const targetH = BRAND_FRAME_HEIGHT;

function rect(overrides: Partial<BrandDrawRectParams>): BrandDrawRect {
  return computeDrawRect({
    imageW: 700,
    imageH: 200,
    targetW,
    targetH,
    zoom: 1,
    offsetX: 0,
    offsetY: 0,
    mode: "cover",
    ...overrides,
  });
}

const IMAGES = [
  { imageW: 700, imageH: 200 },
  { imageW: 200, imageH: 700 },
  { imageW: 352, imageH: 64 },
  { imageW: 32, imageH: 32 },
  { imageW: 1, imageH: 1000 },
];

const OFFSETS = [
  { offsetX: 0, offsetY: 0 },
  { offsetX: 80, offsetY: -40 },
  { offsetX: 10_000, offsetY: 10_000 },
  { offsetX: -10_000, offsetY: -10_000 },
];

describe("computeDrawRect（cover）", () => {
  it("放大后也不露白：横向纵向都盖住目标框", () => {
    for (const image of IMAGES) {
      for (const zoom of [1, 1.5, 3]) {
        for (const offset of OFFSETS) {
          const r = rect({ ...image, zoom, ...offset });
          const where = JSON.stringify({ ...image, zoom, ...offset });
          expect(r.dx, where).toBeLessThanOrEqual(0);
          expect(r.dx + r.dw, where).toBeGreaterThanOrEqual(targetW - 1e-9);
          expect(r.dy, where).toBeLessThanOrEqual(0);
          expect(r.dy + r.dh, where).toBeGreaterThanOrEqual(targetH - 1e-9);
        }
      }
    }
  });

  it("按比例填充：宽高比与原图一致", () => {
    const r = rect({ imageW: 700, imageH: 200, zoom: 1.4 });

    expect(r.dw / r.dh).toBeCloseTo(700 / 200, 10);
  });

  it("无平移时居中；平移量把图顶到边上就停住", () => {
    const centred = rect({ offsetX: 0, offsetY: 0 });
    expect(centred.dx).toBeCloseTo((targetW - centred.dw) / 2, 10);

    const pushedRight = rect({ offsetX: 10_000 });
    expect(pushedRight.dx).toBe(0);
    const pushedLeft = rect({ offsetX: -10_000 });
    expect(pushedLeft.dx + pushedLeft.dw).toBeCloseTo(targetW, 10);
  });

  it("缩放线性放大：2 倍缩放就是 2 倍尺寸", () => {
    const once = rect({ zoom: 1 });
    const twice = rect({ zoom: 2 });

    expect(twice.dw).toBeCloseTo(once.dw * 2, 10);
    expect(twice.dh).toBeCloseTo(once.dh * 2, 10);
  });

  it("连目标框都盖不住时只能居中：这种尺寸怎么摆都会露白", () => {
    const r = rect({ zoom: 0.5 });

    expect(r.dw).toBeLessThan(targetW);
    expect(r.dx).toBeCloseTo((targetW - r.dw) / 2, 10);
  });
});

describe("computeDrawRect（fill）", () => {
  it("拉伸铺满：正好等于目标框，位置归零", () => {
    for (const image of IMAGES) {
      const r = rect({ ...image, mode: "fill" });
      expect(r.dw, JSON.stringify(image)).toBe(targetW);
      expect(r.dh, JSON.stringify(image)).toBe(targetH);
      expect(r.dx).toBe(0);
      expect(r.dy).toBe(0);
    }
  });

  it("缩放后仍不露白：图比目标大时夹在边界内", () => {
    for (const zoom of [1.2, 3]) {
      for (const offset of OFFSETS) {
        const r = rect({ mode: "fill", zoom, ...offset });
        expect(r.dx).toBeLessThanOrEqual(0);
        expect(r.dx + r.dw).toBeGreaterThanOrEqual(targetW - 1e-9);
        expect(r.dy).toBeLessThanOrEqual(0);
        expect(r.dy + r.dh).toBeGreaterThanOrEqual(targetH - 1e-9);
      }
    }
  });
});
