export function NotFoundPage() {
  return (
    <div className="max-w-[480px] space-y-1">
      <h2 className="text-base font-semibold text-ink">页面不存在</h2>
      <p className="text-sm text-ink-muted">这个路径没有对应的页面。</p>
      <a href="/" className="text-xs text-primary-ink hover:underline">
        回到 Peco 面板
      </a>
    </div>
  );
}
