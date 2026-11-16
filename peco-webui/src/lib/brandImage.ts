// 品牌图的编辑与导出几何。
//
// 预览与导出必须共用同一份算法：各算一遍就会出现「预览不露白、存下去露白」，
// 而那种偏差只在左栏那 32px 里才看得出来。

/** 展开槽位：左栏 w-52 里的那一条横图。 */
export const BRAND_SLOT_WIDTH = 176;
export const BRAND_SLOT_HEIGHT = 32;

/** 编辑目标框 = 展开槽位 × 2，同时就是导出口径。 */
export const BRAND_FRAME_WIDTH = BRAND_SLOT_WIDTH * 2;
export const BRAND_FRAME_HEIGHT = BRAND_SLOT_HEIGHT * 2;

/** 缩放区间：1 是刚好覆盖，再大就是裁更多。 */
export const BRAND_MIN_ZOOM = 1;
export const BRAND_MAX_ZOOM = 3;

/**
 * `cover` 按比例填满目标框（多出来的裁掉）；`fill` 横纵各自拉伸铺满。
 * 两轴各自缩放的 `fill` 与等比缩放的 `cover` 是两种不同意图，不能合成一种。
 */
export type BrandFitMode = "cover" | "fill";

export interface BrandDrawRect {
  dx: number;
  dy: number;
  dw: number;
  dh: number;
}

export interface BrandDrawRectParams {
  imageW: number;
  imageH: number;
  targetW: number;
  targetH: number;
  zoom: number;
  offsetX: number;
  offsetY: number;
  mode: BrandFitMode;
}

/**
 * 单轴位置：图比目标大时允许平移，但夹在「图边不出框」的区间里 —— 露白就是
 * 裁切失败。图比目标小时只能居中，怎么摆都会露白。
 */
function axisPosition(target: number, size: number, offset: number): number {
  if (size <= target) return (target - size) / 2;
  const centred = (target - size) / 2 + offset;
  return Math.min(0, Math.max(target - size, centred));
}

export function computeDrawRect({
  imageW,
  imageH,
  targetW,
  targetH,
  zoom,
  offsetX,
  offsetY,
  mode,
}: BrandDrawRectParams): BrandDrawRect {
  const coverScale = Math.max(targetW / imageW, targetH / imageH) * zoom;
  const dw = mode === "cover" ? imageW * coverScale : targetW * zoom;
  const dh = mode === "cover" ? imageH * coverScale : targetH * zoom;
  return {
    dx: axisPosition(targetW, dw, offsetX),
    dy: axisPosition(targetH, dh, offsetY),
    dw,
    dh,
  };
}

export interface BrandEditParams {
  mode: BrandFitMode;
  zoom: number;
  /** 平移量，单位是目标框的 CSS px。 */
  offsetX: number;
  offsetY: number;
}

export interface LoadedImage {
  /** `drawImage` 的图源。 */
  source: CanvasImageSource;
  width: number;
  height: number;
}

/**
 * 把图片画进画布。画布宽度是目标框的整数倍（预览 2 倍、导出 1 倍），
 * 倍数只影响像素密度，几何仍由 [`computeDrawRect`] 一处决定。
 */
export function drawBrand(
  ctx: CanvasRenderingContext2D,
  image: LoadedImage,
  params: BrandEditParams,
): void {
  const { width, height } = ctx.canvas;
  const scale = width / BRAND_FRAME_WIDTH;
  const rect = computeDrawRect({
    imageW: image.width,
    imageH: image.height,
    targetW: BRAND_FRAME_WIDTH * scale,
    targetH: BRAND_FRAME_HEIGHT * scale,
    zoom: params.zoom,
    offsetX: params.offsetX * scale,
    offsetY: params.offsetY * scale,
    mode: params.mode,
  });
  ctx.clearRect(0, 0, width, height);
  ctx.drawImage(image.source, rect.dx, rect.dy, rect.dw, rect.dh);
}

/** 读本地文件，拿到原始像素尺寸 —— 几何要先知道原图多宽多高。 */
export async function loadImageFile(file: Blob): Promise<LoadedImage> {
  return new Promise((resolve, reject) => {
    const url = URL.createObjectURL(file);
    const image = new Image();
    image.onload = () => {
      URL.revokeObjectURL(url);
      resolve({
        source: image,
        width: image.naturalWidth,
        height: image.naturalHeight,
      });
    };
    image.onerror = () => {
      URL.revokeObjectURL(url);
      reject(new Error("这张图片解不开，换一张试试"));
    };
    image.src = url;
  });
}

/** 导出成品 PNG：尺寸就是导出口径，画法与预览同一份。 */
export async function exportBrandImage(
  image: LoadedImage,
  params: BrandEditParams,
): Promise<Blob> {
  const canvas = document.createElement("canvas");
  canvas.width = BRAND_FRAME_WIDTH;
  canvas.height = BRAND_FRAME_HEIGHT;
  const ctx = canvas.getContext("2d");
  if (!ctx) throw new Error("Canvas 2D 上下文不可用");
  drawBrand(ctx, image, params);
  return new Promise((resolve, reject) => {
    canvas.toBlob((blob) => {
      if (blob) resolve(blob);
      else reject(new Error("导出 PNG 失败"));
    }, "image/png");
  });
}
