// 品牌区：左栏那张图的上传、裁剪与清除。
//
// 图先按目标框裁成成品再上传，服务端只存一个 URL —— 左栏各处拿到就能画，
// 不必在显示时再算几何。

import { useRef, useState, type ChangeEvent } from "react";
import { Trash2 } from "lucide-react";
import { toast } from "sonner";
import { BrandImage } from "@/components/common/BrandImage";
import { ErrorBanner } from "@/components/common/ErrorBanner";
import { BrandEditorDialog } from "@/components/settings/BrandEditorDialog";
import { Button } from "@/components/ui/button";
import { Skeleton } from "@/components/ui/skeleton";
import { errorText } from "@/lib/api";
import { useBrand, useSetBrand } from "@/queries/brand";

export function BrandSection() {
  const brand = useBrand();
  const setBrand = useSetBrand();
  const inputRef = useRef<HTMLInputElement>(null);
  const [picked, setPicked] = useState<File | null>(null);
  const image = brand.data?.image ?? "";

  function onPick(event: ChangeEvent<HTMLInputElement>) {
    const file = event.target.files?.[0];
    // 先清空：同一张图连着选两次也要能再进编辑器。
    event.target.value = "";
    if (file) setPicked(file);
  }

  async function remove() {
    try {
      await setBrand.mutateAsync("");
      toast.success("已恢复默认品牌图");
    } catch (error) {
      toast.error(errorText(error));
    }
  }

  return (
    <section className="space-y-2">
      <div className="flex h-8 items-center gap-2 border-b border-line">
        <h2 className="text-xs font-semibold text-ink-muted">品牌</h2>
        <span className="min-w-0 truncate text-caption font-medium text-ink-faint">
          对所有空间与团队生效
        </span>
      </div>

      {brand.error !== null && (
        <ErrorBanner
          message={errorText(brand.error)}
          onRetry={() => void brand.refetch()}
        />
      )}

      {brand.isPending && <Skeleton className="h-14 w-72" />}

      {brand.data && (
        <div className="flex flex-wrap items-end gap-4">
          <figure className="space-y-1">
            <div className="flex h-8 w-44 items-center justify-center rounded-lg border border-line bg-panel">
              <BrandImage image={image} collapsed={false} />
            </div>
            <figcaption className="tabular-nums text-caption text-ink-faint">
              展开 176×32
            </figcaption>
          </figure>

          <figure className="space-y-1">
            <div className="flex size-8 items-center justify-center rounded-lg border border-line bg-panel">
              <BrandImage image={image} collapsed />
            </div>
            <figcaption className="tabular-nums text-caption text-ink-faint">
              折叠 32×32
            </figcaption>
          </figure>

          <div className="flex items-center gap-2">
            <input
              ref={inputRef}
              type="file"
              accept="image/png,image/jpeg,image/gif,image/webp"
              className="hidden"
              onChange={onPick}
            />
            <Button
              type="button"
              size="sm"
              variant="secondary"
              onClick={() => inputRef.current?.click()}
            >
              {image ? "更换" : "上传图片"}
            </Button>
            {image && (
              <Button
                type="button"
                size="sm"
                variant="ghost"
                disabled={setBrand.isPending}
                onClick={() => void remove()}
              >
                <Trash2 /> 删除
              </Button>
            )}
          </div>
        </div>
      )}

      {picked && (
        <BrandEditorDialog file={picked} onClose={() => setPicked(null)} />
      )}
    </section>
  );
}
