//! cron 表达式：归一化、校验与下次触发时刻。
//!
//! 语义与 `peco-server` 的调度器保持同一套（5 字段按「分 时 日 月 周」解释，前置
//! `0 ` 补出秒域）：用户在两处写同一种表达式，结果不该因为落在哪个模块而不同。
//!
//! 纯函数，不碰库也不看时钟 —— 「现在」一律由调用方传进来。

use chrono::{DateTime, Local};
use croner::Cron;

pub use croner::errors::CronError;

/// 把 5 字段 cron 表达式补成 6 字段（秒域补 `0`）。
///
/// 标准 cron 与前端预设写的都是 5 字段，而 `croner` 的秒域要么必需要么可选：
/// 统一补秒之后，落库的表达式与「下次触发时刻」用的是同一份解释。对已归一化的
/// 结果幂等；6 字段、别名（`@daily`）、空串等原样返回 —— 别名由 croner 自己展开，
/// 其余交给解析报错。
pub fn normalize_cron_expr(expr: &str) -> String {
    if expr.split_whitespace().count() == 5 {
        format!("0 {}", expr.trim())
    } else {
        expr.to_string()
    }
}

/// 校验并返回归一化后的表达式；错误里带 croner 的具体原因。
pub fn validate(expr: &str) -> Result<String, CronError> {
    let normalized = normalize_cron_expr(expr);
    parse(&normalized)?;
    Ok(normalized)
}

/// 解析一条表达式（先归一化）。
fn parse(expr: &str) -> Result<Cron, CronError> {
    Cron::new(&normalize_cron_expr(expr))
        .with_seconds_optional()
        .parse()
}

/// `after` 之后的第一个触发时刻（严格晚于 `after`）。
///
/// `after` 用本机时区：用户写「每天九点」指的是本机的九点，库里存的 UTC 文本
/// 只负责不被时区变化影响，不参与这个判断。
pub fn next_occurrence(expr: &str, after: DateTime<Local>) -> Result<DateTime<Local>, CronError> {
    parse(expr)?.find_next_occurrence(&after, false)
}

/// 从 `after` 起的 n 个触发时刻（严格递增，都不早于 `after` 之后）。
///
/// 逐个以「上一个结果」为起点重算：一次算出 n 个要自己处理跨天聚合，而 croner
/// 的迭代器语义是「下一个匹配」，串起来就是同一件事。
pub fn next_occurrences(
    expr: &str,
    after: DateTime<Local>,
    n: usize,
) -> Result<Vec<DateTime<Local>>, CronError> {
    let cron = parse(expr)?;
    let mut cursor = after;
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        let next = cron.find_next_occurrence(&cursor, false)?;
        out.push(next);
        cursor = next;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Datelike, NaiveDate, Timelike};

    /// 本机时区的某个中午。取 12:00 而不是 00:00：夏令时切换都发生在凌晨，
    /// 午夜在某些时区当天根本不存在，"after" 构造不出来会让用例随机失败。
    fn local_noon(y: i32, m: u32, d: u32) -> DateTime<Local> {
        use chrono::TimeZone;
        Local
            .with_ymd_and_hms(y, m, d, 12, 0, 0)
            .single()
            .expect("本地中午必然存在")
    }

    /// 只比较本地日历字段：夏令时偏移因机器而异，比较绝对时刻会让用例随环境漂移。
    fn naive_fields(at: DateTime<Local>) -> (i32, u32, u32, u32, u32) {
        let local = at.naive_local();
        (
            local.year(),
            local.month(),
            local.day(),
            local.hour(),
            local.minute(),
        )
    }

    #[test]
    fn five_fields_get_a_zero_second_domain() {
        assert_eq!(normalize_cron_expr("0 9 * * *"), "0 0 9 * * *");
        assert_eq!(
            normalize_cron_expr("*/5 9-17 * * 1-5"),
            "0 */5 9-17 * * 1-5"
        );
        assert_eq!(normalize_cron_expr("  0 9 * * *  "), "0 0 9 * * *");
    }

    /// 幂等：补秒不得累积，否则「归一化两次」等于换了一条表达式。
    #[test]
    fn normalization_is_idempotent() {
        for expr in ["0 9 * * *", "0 0 9 * * *", "30 */5 * * * *"] {
            let once = normalize_cron_expr(expr);
            assert_eq!(normalize_cron_expr(&once), once, "对 {expr} 不是幂等的");
        }
        assert_eq!(normalize_cron_expr("0 0 9 * * *"), "0 0 9 * * *");
    }

    /// 字段数不对、别名、空串一律原样返回：归一化不认识它们，更不该替它们编一个形状。
    #[test]
    fn unusable_input_is_passed_through_untouched() {
        for expr in ["", "   ", "0 9 * *", "0 0 9 * * * 2026", "@daily"] {
            assert_eq!(normalize_cron_expr(expr), expr);
        }
    }

    #[test]
    fn validation_accepts_five_and_six_fields_and_rejects_garbage() {
        assert!(validate("0 9 * * *").is_ok(), "5 字段是合法的");
        assert!(validate("30 0 9 * * *").is_ok(), "6 字段是合法的");

        let error = validate("0 9 * *").expect_err("4 字段必须被拒");
        assert!(
            !error.to_string().is_empty(),
            "错误必须带具体原因，前端要拿它展示"
        );
        assert!(validate("0 0 99 * * *").is_err(), "越界的小时必须被拒");
        assert!(
            validate("not a cron at all").is_err(),
            "非 cron 文本必须被拒"
        );
        // 别名由 croner 自己展开（`@daily` = 每天零点），归一化不拦它们。
        let alias = validate("@daily").expect("croner 支持 @daily");
        assert_eq!(alias, "@daily", "别名原样落库，计算时再展开");
    }

    /// 每分钟：下一个时刻落在 60 秒之内，且严格晚于起点。
    #[test]
    fn every_minute_advances_within_a_minute() {
        let after = local_noon(2026, 5, 6);
        let next = next_occurrence("* * * * *", after).expect("每分钟必然有下一次");
        assert!(next > after, "下一次必须严格晚于起点");
        assert!(
            next - after <= chrono::Duration::minutes(1),
            "每分钟的表达式不该跳过一分钟以上，实际 {next}"
        );
        assert_eq!(next.second(), 0, "秒域补的是 0，落点必须在整秒");
    }

    /// 跨月：1 月 31 日之后的下一次「每月 1 日零点」在 2 月 1 日。
    #[test]
    fn the_next_month_boundary_is_crossed() {
        let next = next_occurrence("0 0 1 * *", local_noon(2026, 1, 31)).unwrap();
        assert_eq!(naive_fields(next), (2026, 2, 1, 0, 0));
    }

    /// 跨年：12 月 31 日之后的下一次「每年 1 月 1 日零点」在次年。
    #[test]
    fn the_next_year_boundary_is_crossed() {
        let next = next_occurrence("0 0 1 1 *", local_noon(2026, 12, 31)).unwrap();
        assert_eq!(naive_fields(next), (2027, 1, 1, 0, 0));
    }

    /// 闰年：2 月 29 日的表达式在平年之后要一路推到下一个闰年，不能退化成 3 月 1 日。
    #[test]
    fn february_29th_waits_for_the_next_leap_year() {
        let next = next_occurrence("0 0 29 2 *", local_noon(2027, 3, 1)).unwrap();
        assert_eq!(naive_fields(next), (2028, 2, 29, 0, 0));
    }

    /// 每天九点：本机时区的九点整，与时区偏移无关。
    #[test]
    fn daily_nine_lands_on_local_nine() {
        let next = next_occurrence("0 9 * * *", local_noon(2026, 3, 4)).unwrap();
        assert_eq!(naive_fields(next), (2026, 3, 5, 9, 0));
    }

    /// 预览要三个严格递增的时刻：相等或乱序会让前端的「接下来三次」变成谎话。
    #[test]
    fn three_previewed_occurrences_are_strictly_increasing() {
        let after = local_noon(2026, 3, 4);
        let next = next_occurrences("0 9 * * *", after, 3).unwrap();
        assert_eq!(next.len(), 3);
        for (index, at) in next.iter().enumerate() {
            let (year, month, day, hour, _) = naive_fields(*at);
            let expected = NaiveDate::from_ymd_opt(2026, 3, 5 + index as u32)
                .expect("3 月的日期")
                .and_hms_opt(9, 0, 0)
                .expect("合法时刻");
            assert_eq!(
                (year, month, day, hour),
                (
                    expected.year(),
                    expected.month(),
                    expected.day(),
                    expected.hour()
                )
            );
        }
    }
}
