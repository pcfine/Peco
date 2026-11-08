export function NotFoundPage() {
  return (
    <div className="mx-auto w-full max-w-narrow space-y-1">
      <h1 className="text-xl font-semibold text-ink">页面不存在</h1>
      <p className="text-sm text-ink-muted">这个路径没有对应的页面。</p>
      <a
        href="/"
        className="tap-target -mx-2 px-2 text-xs text-primary-ink hover:underline"
      >
        回到 Peco 面板
      </a>
    </div>
  );
}
