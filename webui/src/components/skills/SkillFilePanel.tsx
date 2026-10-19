import type { ReactNode } from "react";
import { PanelLeftClose } from "lucide-react";
import { Button } from "@/components/ui/button";

interface SkillFilePanelProps {
  count: number;
  onCollapse: () => void;
  children: ReactNode;
}

/** 常驻文件栏：栏头 44px 与编辑器工具栏连成一条直线。 */
export function SkillFilePanel({
  count,
  onCollapse,
  children,
}: SkillFilePanelProps) {
  return (
    <div
      data-testid="file-tree"
      className="flex h-full w-[240px] shrink-0 flex-col border-r bg-card"
    >
      <div
        data-testid="file-tree-header"
        className="flex h-11 shrink-0 items-center gap-2 border-b px-3"
      >
        <span className="text-xs font-medium">文件</span>
        <span className="ml-auto text-[11px] tabular-nums text-muted-foreground">
          {count}
        </span>
        <Button
          type="button"
          size="icon-xs"
          variant="ghost"
          className="size-6"
          onClick={onCollapse}
          title="收起文件栏"
          aria-expanded={true}
          aria-controls="skill-files"
          data-testid="file-tree-collapse"
        >
          <PanelLeftClose className="size-3.5" />
          <span className="sr-only">收起文件栏</span>
        </Button>
      </div>
      <div className="min-h-0 flex-1 overflow-y-auto py-2 [scrollbar-gutter:stable] [overscroll-behavior:contain]">
        {children}
      </div>
    </div>
  );
}
