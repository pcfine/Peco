/** 字节数显示：<1 KB 用整数 B，其余保留 1 位小数。 */
export function formatBytes(n: number): string {
  if (!Number.isFinite(n) || n < 0) return "0 B";
  if (n < 1024) return `${Math.round(n)} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  if (n < 1024 * 1024 * 1024) return `${(n / (1024 * 1024)).toFixed(1)} MB`;
  return `${(n / (1024 * 1024 * 1024)).toFixed(1)} GB`;
}

/**
 * 解析服务端时间串（DB `datetime('now')` 产出的 `"2026-10-20 12:00:00"`）。
 *
 * 该串是 UTC 但无时区标记，直接交给 `new Date()` 会按本地时区解析 —— 东八区
 * 下整体偏 8 小时，故补 `T` 分隔符与 `Z` 再解析。
 */
export function parseServerTimestamp(s: string): Date {
  return new Date(s.replace(" ", "T") + "Z");
}

function pad2(n: number): string {
  return String(n).padStart(2, "0");
}

/**
 * 展示用创建时间：精确到分钟并按浏览器本地时区呈现。
 *
 * 时间串无效时返回空串，避免把 `Invalid Date` 渲染给用户，由调用方整段省略。
 */
export function formatCreatedAt(raw: string): string {
  const date = parseServerTimestamp(raw);
  if (Number.isNaN(date.getTime())) return "";
  return `${date.getFullYear()}-${pad2(date.getMonth() + 1)}-${pad2(date.getDate())} ${pad2(date.getHours())}:${pad2(date.getMinutes())}`;
}
