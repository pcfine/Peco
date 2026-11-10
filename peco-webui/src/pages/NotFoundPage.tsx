import { PageFrame } from "@/components/layout/PageFrame";

export function NotFoundPage() {
  return (
    <PageFrame title="页面不存在" contentWidth="narrow">
      <div className="space-y-1">
        <p className="text-sm text-ink-muted">这个路径没有对应的页面。</p>
        <a
          href="/"
          className="tap-target -mx-2 px-2 text-xs text-primary-ink hover:underline"
        >
          回到 Peco 面板
        </a>
      </div>
    </PageFrame>
  );
}
