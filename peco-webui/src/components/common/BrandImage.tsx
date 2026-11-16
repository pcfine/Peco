// 左栏品牌图片：展开态整图，折叠态只取左半并横向压缩。
//
// 折叠槽位只有 32px 宽，而品牌图是按 176×32 设计的；等比缩进去会变成一条细横条，
// 所以内层按 2 倍宽度渲染再裁掉右半 —— 看得见的恰好是原图左 1/2。

import { useEffect, useState } from "react";
import { BrandMark } from "@/components/common/BrandMark";
import { BRAND_SLOT_HEIGHT, BRAND_SLOT_WIDTH } from "@/lib/brandImage";

interface BrandImageProps {
  /** `/uploads/brand/...`；空串表示未设置，退回默认标记。 */
  image: string;
  collapsed: boolean;
  /** 槽位高度（默认即左栏规格 32px），宽度按展开槽位的比例推出。 */
  size?: number;
}

export function BrandImage({
  image,
  collapsed,
  size = BRAND_SLOT_HEIGHT,
}: BrandImageProps) {
  const [broken, setBroken] = useState(false);

  // 换了图要重新给一次机会：坏图标记跟着组件活着，新图就永远显示不出来。
  useEffect(() => setBroken(false), [image]);

  if (!image || broken) return <BrandMark size={24} />;

  if (collapsed) {
    return (
      <span
        className="relative shrink-0 overflow-hidden"
        style={{ width: size, height: size }}
      >
        {/* `max-w-none` 抵消 preflight 的 `max-width: 100%`，否则 2 倍宽度会被压回容器。 */}
        <img
          src={image}
          alt=""
          aria-hidden="true"
          draggable={false}
          onError={() => setBroken(true)}
          className="absolute top-0 left-0 h-full w-[200%] max-w-none object-fill"
        />
      </span>
    );
  }

  return (
    <img
      src={image}
      alt=""
      aria-hidden="true"
      draggable={false}
      onError={() => setBroken(true)}
      className="shrink-0 select-none object-contain"
      style={{
        width: (size * BRAND_SLOT_WIDTH) / BRAND_SLOT_HEIGHT,
        height: size,
      }}
    />
  );
}
