// 闸门页的页框：登录前没有应用壳（左栏、顶栏都不该在），但字体、配色、圆角
// 与壳里共用同一套 token —— 认证是应用的一部分，不是另一套站点。
//
// 宽度固定在 360：表单里最长的一条是 setup token，再宽只会让视线横穿。

import type { ReactNode } from "react";
import { BrandMark } from "@/components/common/BrandMark";

interface AuthFrameProps {
  title: string;
  description: ReactNode;
  children: ReactNode;
}

export function AuthFrame({ title, description, children }: AuthFrameProps) {
  return (
    <div className="flex h-full min-h-0 overflow-y-auto bg-canvas px-4 py-10 text-ink">
      {/* `m-auto` 而不是 `items-center`：内容比视口高时，居中会把顶部裁掉。 */}
      <main className="m-auto w-full max-w-[360px] space-y-6">
        <header className="space-y-2">
          <div className="flex items-center gap-2">
            <BrandMark size={22} />
            <span className="text-sm font-semibold">Peco</span>
          </div>
          <h1 className="text-xl font-semibold text-ink">{title}</h1>
          <p className="text-sm text-ink-muted">{description}</p>
        </header>
        {children}
      </main>
    </div>
  );
}
