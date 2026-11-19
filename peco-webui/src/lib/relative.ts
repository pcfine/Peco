// 列表里的「最近活动」只到分钟级：精确时间在详情页给，列表一栏只回答「多久以前」。

import { parseServerTimestamp } from "@/lib/format";

/** 服务端 UTC 无时区串 → 「刚刚 / n 分钟前 / n 小时前 / 昨天 / n 天前 / 10月28日」。 */
export function relativeTime(
  raw: string | null,
  now: number = Date.now(),
): string {
  if (!raw) return "—";
  const then = parseServerTimestamp(raw).getTime();
  if (Number.isNaN(then)) return "—";

  const diffMs = now - then;
  if (diffMs < 60_000) return "刚刚";
  if (diffMs < 3_600_000) return `${Math.floor(diffMs / 60_000)} 分钟前`;
  if (diffMs < 86_400_000) return `${Math.floor(diffMs / 3_600_000)} 小时前`;

  const date = new Date(then);
  const nowDate = new Date(now);
  const midnight = (d: Date) =>
    new Date(d.getFullYear(), d.getMonth(), d.getDate()).getTime();
  const dayOffset = Math.round(
    (midnight(nowDate) - midnight(date)) / 86_400_000,
  );
  if (dayOffset === 1) return "昨天";
  if (dayOffset < 7) return `${dayOffset} 天前`;
  return `${date.getMonth() + 1}月${date.getDate()}日`;
}

/**
 * 未来的时刻：「即将运行 / 不到 1 分钟后 / n 分钟后 / n 小时后 / 明天 / n 天后 / 10月28日」。
 *
 * 与 `relativeTime` 分开放：那条读的是「多久以前」，未来的时刻套进去会一律显示
 * 「刚刚」。解析不了返回「—」。
 */
export function relativeUntil(
  raw: string | null,
  now: number = Date.now(),
): string {
  if (!raw) return "—";
  const then = parseServerTimestamp(raw).getTime();
  if (Number.isNaN(then)) return "—";

  const diffMs = then - now;
  if (diffMs <= 0) return "即将运行";
  if (diffMs < 60_000) return "不到 1 分钟后";
  if (diffMs < 3_600_000) return `${Math.floor(diffMs / 60_000)} 分钟后`;
  if (diffMs < 86_400_000) return `${Math.floor(diffMs / 3_600_000)} 小时后`;

  const date = new Date(then);
  const nowDate = new Date(now);
  const midnight = (d: Date) =>
    new Date(d.getFullYear(), d.getMonth(), d.getDate()).getTime();
  const dayOffset = Math.round(
    (midnight(date) - midnight(nowDate)) / 86_400_000,
  );
  if (dayOffset === 1) return "明天";
  if (dayOffset < 7) return `${dayOffset} 天后`;
  return `${date.getMonth() + 1}月${date.getDate()}日`;
}

/** 该时间串是否落在 `now` 的本地日历日；解析不了按「不是今天」处理。 */
export function isToday(raw: string, now: number = Date.now()): boolean {
  const date = parseServerTimestamp(raw);
  if (Number.isNaN(date.getTime())) return false;
  const ref = new Date(now);
  return (
    date.getFullYear() === ref.getFullYear() &&
    date.getMonth() === ref.getMonth() &&
    date.getDate() === ref.getDate()
  );
}
