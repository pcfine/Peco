function pad2(n: number): string {
  return String(n).padStart(2, "0");
}

/** 本地日历日的整日差（`from` 减 `to`）；夏令时的一天是 23/25 小时，故四舍五入。 */
function localDayOffset(from: Date, to: Date): number {
  const midnight = (d: Date) =>
    new Date(d.getFullYear(), d.getMonth(), d.getDate()).getTime();
  return Math.round((midnight(from) - midnight(to)) / 86_400_000);
}

/**
 * 气泡时间戳：按与 `now` 的距离分档（今天 / 昨天 / 同年更早 / 跨年），
 * 精确到分钟，取浏览器本地时区。
 *
 * 非法值（缺失 / NaN / 非正数 / Infinity）返回 `null`，由调用方整段省略：
 * `new Date(undefined).toISOString()` 会抛 RangeError，气泡会白屏。
 * `now` 显式传入，单测才能确定性地断言「昨天」而不受运行时刻影响。
 */
export function formatMessageTime(
  ms: number | undefined | null,
  now: number = Date.now(),
): { label: string; iso: string } | null {
  if (typeof ms !== "number" || !Number.isFinite(ms) || ms <= 0) return null;

  const date = new Date(ms);
  const nowDate = new Date(now);
  const iso = date.toISOString();
  const hhmm = `${pad2(date.getHours())}:${pad2(date.getMinutes())}`;
  const monthDay = `${pad2(date.getMonth() + 1)}-${pad2(date.getDate())}`;

  const dayOffset = localDayOffset(nowDate, date);
  if (dayOffset === 0) return { label: hhmm, iso };
  if (dayOffset === 1) return { label: `昨天 ${hhmm}`, iso };
  if (date.getFullYear() === nowDate.getFullYear()) {
    return { label: `${monthDay} ${hhmm}`, iso };
  }
  return { label: `${date.getFullYear()}-${monthDay} ${hhmm}`, iso };
}
