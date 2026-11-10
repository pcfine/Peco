import { cn } from "@/lib/utils";

/**
 * Peco 项目图标，直接复用站点 favicon 那一份。
 *
 * 用 `<img>` 而不是内联 SVG：favicon 里的 mask/filter 带固定 id，同一文档里
 * 多处内联会撞 id，卸载任一份都可能让另一份的遮罩失去引用。
 */
export function BrandMark({
  className,
  size = 28,
}: {
  className?: string;
  size?: number;
}) {
  return (
    <img
      src="/favicon.svg"
      alt=""
      aria-hidden="true"
      draggable={false}
      className={cn("shrink-0 select-none object-contain", className)}
      style={{ width: size, height: size }}
    />
  );
}
