//! 合成标定集 CSV 解析。
//!
//! 服务于 `examples/calibrate_thresholds.rs` 的 `synthetic` 模式：真实记忆库中
//! 不含近重复样本（全对余弦均低于阈值地板）时，无法从真实分布标定阈值，改用
//! 人工编写的真值对（正例 / 负例各若干）作为标定集。本模块只做一件事 —
//! 把 CSV 严格解析为 [`LabeledPair`]；嵌入与余弦计算由调用方完成。

use std::collections::HashMap;

/// 合成标定集 CSV 的列名（顺序固定）。
pub const SYNTHETIC_PAIR_HEADER: [&str; 5] = ["pair_id", "category", "label", "text_a", "text_b"];

/// CSV 的一行 — 一对人工标注好 0/1 的合成样本。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LabeledPair {
    /// 对标识（如 `P1-01`），导出标注 JSON 时作为 `doc_id` 前缀。
    pub pair_id: String,
    /// 类别标签（如 `synthetic`），导出标注 JSON 时写入 `title`。
    pub category: String,
    /// 人工标注：1 = 语义重复，0 = 非重复。
    pub label: u8,
    /// 第一段文本。
    pub text_a: String,
    /// 第二段文本。
    pub text_b: String,
}

/// 解析合成标定集 CSV。
///
/// 严格校验：表头必须与 [`SYNTHETIC_PAIR_HEADER`] 逐列匹配（大小写不敏感）、
/// 每行恰好 5 个字段、`label ∈ {0,1}`、`pair_id` 不重复且文本非空。
/// 空行跳过；字段可用双引号包裹（字段内允许逗号），不支持 `""` 转义引号。
///
/// 所有错误均带行号（1 起，含表头行），便于直接定位到 CSV。
pub fn parse_labeled_pairs(csv: &str) -> Result<Vec<LabeledPair>, String> {
    let mut pairs: Vec<LabeledPair> = Vec::new();
    // pair_id → 首次出现的行号，用于重复检测
    let mut seen: HashMap<String, usize> = HashMap::new();
    let mut header_seen = false;

    for (idx, raw) in csv.lines().enumerate() {
        let line_no = idx + 1;
        // 兼容 CRLF；空行（含仅空白）跳过
        let line = raw.trim_end_matches('\r');
        if line.trim().is_empty() {
            continue;
        }
        let fields = split_csv_line(line, line_no)?;

        if !header_seen {
            header_seen = true;
            if fields.len() != SYNTHETIC_PAIR_HEADER.len() {
                return Err(format!(
                    "第 {line_no} 行：表头应为 {} 列，实际 {} 列",
                    SYNTHETIC_PAIR_HEADER.len(),
                    fields.len()
                ));
            }
            for (got, want) in fields.iter().zip(SYNTHETIC_PAIR_HEADER.iter()) {
                if !got.trim().eq_ignore_ascii_case(want) {
                    return Err(format!(
                        "第 {line_no} 行：表头不匹配 — 期望 {:?}，实际 {:?}",
                        SYNTHETIC_PAIR_HEADER.join(","),
                        fields.join(",")
                    ));
                }
            }
            continue;
        }

        if fields.len() != SYNTHETIC_PAIR_HEADER.len() {
            return Err(format!(
                "第 {line_no} 行：字段数为 {}，与表头的 {} 列不符",
                fields.len(),
                SYNTHETIC_PAIR_HEADER.len()
            ));
        }

        let pair_id = fields[0].trim().to_string();
        if pair_id.is_empty() {
            return Err(format!("第 {line_no} 行：pair_id 为空"));
        }
        if let Some(first) = seen.insert(pair_id.clone(), line_no) {
            return Err(format!(
                "第 {line_no} 行：pair_id `{pair_id}` 重复（首次出现于第 {first} 行）"
            ));
        }

        let category = fields[1].trim().to_string();
        if category.is_empty() {
            return Err(format!(
                "第 {line_no} 行（pair_id {pair_id}）：category 为空"
            ));
        }

        let label_raw = fields[2].trim();
        let label = match label_raw {
            "0" => 0u8,
            "1" => 1u8,
            other => {
                return Err(format!(
                    "第 {line_no} 行（pair_id {pair_id}）：label 必须为 0 或 1，实际 {other:?}"
                ));
            }
        };

        // 文本保留原样（不 trim），仅拒绝空文本 — 空白文本无法嵌入出有意义的向量
        let text_a = fields[3].clone();
        let text_b = fields[4].clone();
        if text_a.trim().is_empty() || text_b.trim().is_empty() {
            return Err(format!(
                "第 {line_no} 行（pair_id {pair_id}）：text_a / text_b 不得为空"
            ));
        }

        pairs.push(LabeledPair {
            pair_id,
            category,
            label,
            text_a,
            text_b,
        });
    }

    if !header_seen {
        return Err("CSV 为空 — 连表头行都没有".into());
    }
    if pairs.is_empty() {
        return Err("CSV 只有表头，没有任何数据行".into());
    }
    Ok(pairs)
}

/// 按 CSV 规则切分一行。
///
/// 引号必须包裹整个字段（字段内允许逗号）；引号不闭合、引号出现在字段中间、
/// 或引号闭合后还有多余字符时报错并附带行号。
fn split_csv_line(line: &str, line_no: usize) -> Result<Vec<String>, String> {
    let mut fields: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut in_quotes = false;
    let mut closed = false; // 引号字段已闭合，其后只允许分隔符

    for ch in line.chars() {
        if in_quotes {
            if ch == '"' {
                in_quotes = false;
                closed = true;
            } else {
                cur.push(ch);
            }
            continue;
        }
        if closed {
            if ch == ',' {
                fields.push(std::mem::take(&mut cur));
                closed = false;
            } else {
                return Err(format!(
                    "第 {line_no} 行：引号字段闭合后出现多余字符 {ch:?}"
                ));
            }
            continue;
        }
        match ch {
            '"' if cur.is_empty() => in_quotes = true,
            '"' => {
                return Err(format!("第 {line_no} 行：引号必须包裹整个字段"));
            }
            ',' => fields.push(std::mem::take(&mut cur)),
            _ => cur.push(ch),
        }
    }

    if in_quotes {
        return Err(format!("第 {line_no} 行：引号不闭合"));
    }
    fields.push(cur);
    Ok(fields)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_quoted_and_bare_fields() {
        let csv = "pair_id,category,label,text_a,text_b\n\
                   P1-01,synthetic,1,\"peco 前端用 React 19，构建工具是 Vite。\",peco 前端技术栈：React 19 + Vite\n\
                   \n\
                   N1-01,synthetic,0,\"cron 触发让整理全自动。\",\"cron 表达式必须含秒域。\"\n";
        let pairs = parse_labeled_pairs(csv).expect("应能解析");
        assert_eq!(pairs.len(), 2);
        assert_eq!(pairs[0].pair_id, "P1-01");
        assert_eq!(pairs[0].category, "synthetic");
        assert_eq!(pairs[0].label, 1);
        // 引号内的中文逗号保留，包裹引号被剥掉
        assert_eq!(pairs[0].text_a, "peco 前端用 React 19，构建工具是 Vite。");
        assert_eq!(pairs[0].text_b, "peco 前端技术栈：React 19 + Vite");
        assert_eq!(pairs[1].label, 0);
        assert_eq!(pairs[1].text_b, "cron 表达式必须含秒域。");
    }

    #[test]
    fn rejects_wrong_header() {
        let csv = "id,category,label,text_a,text_b\nP1-01,synthetic,1,a,b\n";
        let err = parse_labeled_pairs(csv).expect_err("表头错应报错");
        assert!(err.contains("表头"), "错误应提及表头，实际: {err}");
        assert!(err.contains("第 1 行"), "错误应带行号，实际: {err}");
    }

    #[test]
    fn rejects_invalid_label() {
        let csv = "pair_id,category,label,text_a,text_b\nP1-01,synthetic,2,a,b\n";
        let err = parse_labeled_pairs(csv).expect_err("label 非法应报错");
        assert!(err.contains("label"), "错误应提及 label，实际: {err}");
        assert!(err.contains("第 2 行"), "错误应带行号，实际: {err}");
        assert!(err.contains("P1-01"), "错误应带 pair_id，实际: {err}");
    }

    #[test]
    fn rejects_field_count_mismatch() {
        let csv = "pair_id,category,label,text_a,text_b\nP1-01,synthetic,1,a\n";
        let err = parse_labeled_pairs(csv).expect_err("字段数不符应报错");
        assert!(err.contains("第 2 行"), "错误应带行号，实际: {err}");
        assert!(err.contains("5"), "错误应说明期望列数，实际: {err}");
    }
}
