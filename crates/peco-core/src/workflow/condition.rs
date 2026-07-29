// ============================================================================
// condition — 条件表达式求值
// ============================================================================

use std::collections::HashMap;

use super::definition::{StepResult, WorkflowStep};
use super::template::TemplateContext;

/// 求值步骤的 condition 表达式。
///
/// `condition` 和 `depends_on` 是正交的：
/// - `depends_on: [A]` → 等待 A 完成（无论成败）
/// - `condition: "{{ steps.A.success }}"` → 仅在 A 成功时执行
/// - 两者都设置 → 等待 A 完成，然后根据渲染结果决定是否执行
/// - 两者都不设置 → 无条件立即执行
///
/// condition 字段使用 minijinja 语法：
/// ```yaml
/// condition: "{{ steps.review.success }}"                      # 布尔求值
/// condition: "{{ steps.review.output.severity == 'critical' }}" # 比较
/// condition: ""                                                 # 总是执行
/// ```
pub fn evaluate_condition(
    step: &WorkflowStep,
    _results: &HashMap<String, StepResult>,
    tpl_ctx: &TemplateContext,
) -> bool {
    match &step.condition {
        None => true,
        Some(expr) if expr.is_empty() => true,
        Some(expr) => tpl_ctx.render_bool(expr),
    }
}
