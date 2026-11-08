// 配置面的表单原语：段标题 + 「右对齐标签 / 控件」两列。
// 标签列定宽，纵向扫下来才落在同一条竖线上。

import type { ReactNode } from "react";
import { Label } from "@/components/ui/label";

interface ConfigSectionProps {
  title: string;
  children: ReactNode;
}

export function ConfigSection({ title, children }: ConfigSectionProps) {
  return (
    <section className="space-y-2 border-b border-line px-4 py-3 last:border-b-0">
      <h3 className="text-xs font-medium text-ink">{title}</h3>
      <div className="space-y-2">{children}</div>
    </section>
  );
}

interface FieldRowProps {
  label: string;
  /** 与控件 `id` 成对：点标签、读屏的「按标签找控件」都落到同一个输入框。 */
  htmlFor?: string;
  hint?: string;
  children: ReactNode;
}

export function FieldRow({ label, htmlFor, hint, children }: FieldRowProps) {
  return (
    <div className="flex items-start gap-3">
      <Label
        htmlFor={htmlFor}
        className="w-[104px] shrink-0 pt-1.5 text-right text-caption font-medium text-ink-faint"
      >
        {label}
      </Label>
      {/* 控件列要能被标签挤窄，否则长内容把整段撑出横向滚动。 */}
      <div className="min-w-0 max-w-[360px] flex-1">
        {children}
        {hint && <p className="mt-1 text-caption text-ink-faint">{hint}</p>}
      </div>
    </div>
  );
}
