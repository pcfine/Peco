// 品牌图编辑器：先在目标框里摆好，再把成品 PNG 交给服务端。
//
// 服务端不做裁剪，所以这里看到的必须就是存下去的：预览画布与导出画布都走
// `drawBrand`，几何只有 `computeDrawRect` 一份。

import {
  useEffect,
  useRef,
  useState,
  type KeyboardEvent as ReactKeyboardEvent,
  type PointerEvent as ReactPointerEvent,
} from "react";
import { Loader2, RotateCcw } from "lucide-react";
import { toast } from "sonner";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Label } from "@/components/ui/label";
import { Slider } from "@/components/ui/slider";
import { uploadImage } from "@/api/upload";
import { errorText } from "@/lib/api";
import {
  BRAND_FRAME_HEIGHT,
  BRAND_FRAME_WIDTH,
  BRAND_MAX_ZOOM,
  BRAND_MIN_ZOOM,
  drawBrand,
  exportBrandImage,
  loadImageFile,
  type BrandEditParams,
  type BrandFitMode,
  type LoadedImage,
} from "@/lib/brandImage";
import { useSetBrand } from "@/queries/brand";

/** 方向键每按一次挪多少：小到能对位，大到按几下就有变化。 */
const KEY_STEP = 4;

const MODES: { value: BrandFitMode; label: string }[] = [
  { value: "cover", label: "按比例填充" },
  { value: "fill", label: "拉伸铺满" },
];

export function BrandEditorDialog({
  file,
  onClose,
}: {
  file: File;
  onClose: () => void;
}) {
  const [image, setImage] = useState<LoadedImage | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [mode, setMode] = useState<BrandFitMode>("cover");
  const [zoom, setZoom] = useState(BRAND_MIN_ZOOM);
  const [offset, setOffset] = useState({ x: 0, y: 0 });
  const [saving, setSaving] = useState(false);
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const dragRef = useRef<{
    pointerX: number;
    pointerY: number;
    offsetX: number;
    offsetY: number;
  } | null>(null);
  const setBrand = useSetBrand();

  useEffect(() => {
    let live = true;
    setImage(null);
    setLoadError(null);
    loadImageFile(file).then(
      (loaded) => {
        if (live) setImage(loaded);
      },
      (error: unknown) => {
        if (live) setLoadError(errorText(error));
      },
    );
    return () => {
      live = false;
    };
  }, [file]);

  const params: BrandEditParams = {
    mode,
    zoom,
    offsetX: offset.x,
    offsetY: offset.y,
  };

  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas || !image) return;
    const ctx = canvas.getContext("2d");
    if (!ctx) return;
    drawBrand(ctx, image, { mode, zoom, offsetX: offset.x, offsetY: offset.y });
    // 依赖逐项列出：`params` 每帧都是新对象，放进依赖里会每渲染都重画。
  }, [image, mode, zoom, offset.x, offset.y]);

  function reset() {
    setMode("cover");
    setZoom(BRAND_MIN_ZOOM);
    setOffset({ x: 0, y: 0 });
  }

  function onPointerDown(event: ReactPointerEvent<HTMLCanvasElement>) {
    event.currentTarget.setPointerCapture(event.pointerId);
    dragRef.current = {
      pointerX: event.clientX,
      pointerY: event.clientY,
      offsetX: offset.x,
      offsetY: offset.y,
    };
  }

  function onPointerMove(event: ReactPointerEvent<HTMLCanvasElement>) {
    const drag = dragRef.current;
    if (!drag) return;
    setOffset({
      x: drag.offsetX + event.clientX - drag.pointerX,
      y: drag.offsetY + event.clientY - drag.pointerY,
    });
  }

  function endDrag(event: ReactPointerEvent<HTMLCanvasElement>) {
    if (!dragRef.current) return;
    dragRef.current = null;
    event.currentTarget.releasePointerCapture(event.pointerId);
  }

  // 拖动的键盘替代：方向键直接改同一份偏移量。
  function onKeyDown(event: ReactKeyboardEvent<HTMLCanvasElement>) {
    const step: Record<string, [number, number]> = {
      ArrowLeft: [-KEY_STEP, 0],
      ArrowRight: [KEY_STEP, 0],
      ArrowUp: [0, -KEY_STEP],
      ArrowDown: [0, KEY_STEP],
    };
    const delta = step[event.key];
    if (!delta) return;
    event.preventDefault();
    setOffset((current) => ({
      x: current.x + delta[0],
      y: current.y + delta[1],
    }));
  }

  async function save() {
    if (!image) return;
    setSaving(true);
    try {
      const blob = await exportBrandImage(image, params);
      // 顺序固定：先拿到 URL 再写配置，写配置失败的图不会被引用。
      const url = await uploadImage(blob, "brand");
      await setBrand.mutateAsync(url);
      toast.success("品牌图片已更新");
      onClose();
    } catch (error) {
      toast.error(errorText(error));
    } finally {
      setSaving(false);
    }
  }

  return (
    <Dialog
      open
      onOpenChange={(open) => {
        if (!open && !saving) onClose();
      }}
    >
      <DialogContent className="sm:max-w-md">
        <DialogHeader>
          <DialogTitle>编辑品牌图片</DialogTitle>
          <DialogDescription>
            目标框 352×64，预览同尺寸放大两倍显示。拖动图片或用方向键调整位置。
          </DialogDescription>
        </DialogHeader>

        <div className="space-y-3">
          <div className="w-fit overflow-hidden rounded-md border border-line bg-panel">
            <canvas
              ref={canvasRef}
              width={BRAND_FRAME_WIDTH * 2}
              height={BRAND_FRAME_HEIGHT * 2}
              tabIndex={0}
              aria-label="品牌图片预览：拖动或用方向键平移"
              onPointerDown={onPointerDown}
              onPointerMove={onPointerMove}
              onPointerUp={endDrag}
              onPointerCancel={endDrag}
              onKeyDown={onKeyDown}
              className="block cursor-grab touch-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-hidden active:cursor-grabbing"
              style={{ width: BRAND_FRAME_WIDTH, height: BRAND_FRAME_HEIGHT }}
            />
          </div>

          {loadError !== null && (
            <p className="text-sm text-danger">{loadError}</p>
          )}

          <div className="flex items-center gap-2">
            <span className="w-14 shrink-0 text-xs text-ink-muted">
              填充方式
            </span>
            <div role="group" aria-label="填充方式" className="flex gap-1">
              {MODES.map((item) => (
                <Button
                  key={item.value}
                  type="button"
                  size="xs"
                  variant={mode === item.value ? "secondary" : "ghost"}
                  aria-pressed={mode === item.value}
                  onClick={() => setMode(item.value)}
                >
                  {item.label}
                </Button>
              ))}
            </div>
          </div>

          <div className="flex items-center gap-2">
            <Label
              htmlFor="brand-zoom"
              className="w-14 shrink-0 text-xs text-ink-muted"
            >
              缩放
            </Label>
            <Slider
              id="brand-zoom"
              aria-label="缩放"
              className="max-w-56"
              min={BRAND_MIN_ZOOM}
              max={BRAND_MAX_ZOOM}
              step={0.05}
              value={[zoom]}
              onValueChange={([next]) => setZoom(next ?? BRAND_MIN_ZOOM)}
            />
            <span className="w-10 shrink-0 text-right tabular-nums text-mono-sm text-ink-faint">
              {zoom.toFixed(2)}×
            </span>
          </div>

          <Button
            type="button"
            size="xs"
            variant="ghost"
            onClick={reset}
            className="text-ink-muted"
          >
            <RotateCcw /> 重置
          </Button>
        </div>

        <DialogFooter>
          <Button
            type="button"
            variant="ghost"
            size="sm"
            disabled={saving}
            onClick={onClose}
          >
            取消
          </Button>
          <Button
            type="button"
            size="sm"
            disabled={saving || !image}
            onClick={() => void save()}
          >
            {saving && <Loader2 className="animate-spin" />}
            保存
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
