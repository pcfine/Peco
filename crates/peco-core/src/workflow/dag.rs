// ============================================================================
// DagGraph — DAG 构建、拓扑排序与验证
// ============================================================================

use std::collections::{HashMap, HashSet};

use super::definition::WorkflowStep;
use super::error::WorkflowError;

/// 步骤依赖的有向无环图。
///
/// 构建时验证：
/// - 无环（拓扑排序成功）
/// - depends_on 引用的步骤 ID 存在
/// - 无自身依赖
#[derive(Debug)]
pub struct DagGraph {
    #[allow(dead_code)]
    steps: Vec<WorkflowStep>,
    /// 邻接表：step_id → [前置步骤 IDs]
    #[allow(dead_code)]
    dependencies: HashMap<String, Vec<String>>,
    /// 拓扑排序后的层级分组（每层内的步骤可并行执行）
    levels: Vec<Vec<WorkflowStep>>,
}

impl DagGraph {
    /// 从步骤列表构建 DAG，验证合法性。
    pub fn build(steps: &[WorkflowStep]) -> Result<Self, WorkflowError> {
        // 1. 收集所有 step ID
        let ids: HashSet<&str> = steps.iter().map(|s| s.id.as_str()).collect();

        // 2. 验证 depends_on 引用
        for step in steps {
            for dep in &step.depends_on {
                if dep == &step.id {
                    return Err(WorkflowError::InvalidDag(format!(
                        "step '{}' depends on itself",
                        step.id
                    )));
                }
                if !ids.contains(dep.as_str()) {
                    return Err(WorkflowError::InvalidDag(format!(
                        "step '{}' depends on unknown step '{}'",
                        step.id, dep
                    )));
                }
            }
        }

        // 3. Kahn 算法拓扑排序 + 分层
        let levels = kahn_level_sort(steps)?;

        // 4. 构建邻接表
        let mut dependencies: HashMap<String, Vec<String>> = HashMap::new();
        for step in steps {
            dependencies.insert(step.id.clone(), step.depends_on.clone());
        }

        Ok(Self {
            steps: steps.to_vec(),
            dependencies,
            levels,
        })
    }

    /// 返回拓扑层级（每层内的步骤可并行执行）。
    pub fn topological_levels(&self) -> &[Vec<WorkflowStep>] {
        &self.levels
    }
}

/// Kahn 算法 + BFS 分层。
///
/// 返回拓扑排序后的层级分组。同一层级内的步骤无相互依赖。
fn kahn_level_sort(steps: &[WorkflowStep]) -> Result<Vec<Vec<WorkflowStep>>, WorkflowError> {
    if steps.is_empty() {
        return Ok(Vec::new());
    }

    // 入度表：step_id → 未完成的依赖数
    let mut in_degree: HashMap<&str, usize> = HashMap::new();
    for step in steps {
        in_degree.insert(&step.id, step.depends_on.len());
    }

    // 构建反向邻接表：step_id → [依赖它的步骤 IDs]（优化：避免全量扫描）
    let mut reverse_deps: HashMap<&str, Vec<&str>> = HashMap::new();
    for step in steps {
        for dep in &step.depends_on {
            reverse_deps.entry(dep.as_str()).or_default().push(&step.id);
        }
    }

    // Step ID → &WorkflowStep 映射
    let step_map: HashMap<&str, &WorkflowStep> = steps.iter().map(|s| (s.id.as_str(), s)).collect();

    // 第 0 层：入度为 0 的步骤
    let mut current_level: Vec<&WorkflowStep> = steps
        .iter()
        .filter(|s| in_degree[s.id.as_str()] == 0)
        .collect();

    if current_level.is_empty() && !steps.is_empty() {
        return Err(WorkflowError::InvalidDag(
            "DAG contains a cycle — no entry node found".into(),
        ));
    }

    let mut levels: Vec<Vec<WorkflowStep>> = Vec::new();

    while !current_level.is_empty() {
        levels.push(current_level.iter().map(|s| (*s).clone()).collect());

        let mut next_level: Vec<&WorkflowStep> = Vec::new();

        for node in &current_level {
            // 查找所有依赖此节点的步骤
            if let Some(dependents) = reverse_deps.get(node.id.as_str()) {
                for dep_id in dependents {
                    let entry = in_degree.get_mut(dep_id).unwrap();
                    *entry -= 1;
                    if *entry == 0
                        && let Some(step) = step_map.get(dep_id)
                    {
                        next_level.push(step);
                    }
                }
            }
        }

        current_level = next_level;
    }

    // 检查是否有未处理的节点（循环依赖）
    let processed: usize = levels.iter().map(|l| l.len()).sum();
    if processed < steps.len() {
        return Err(WorkflowError::InvalidDag(
            "DAG contains a cycle — not all nodes processed".into(),
        ));
    }

    Ok(levels)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflow::definition::{OnFailure, StepConfig, StepType};

    fn make_step(id: &str, name: &str, deps: Vec<&str>) -> WorkflowStep {
        WorkflowStep {
            id: id.to_string(),
            name: name.to_string(),
            step_type: StepType::Shell,
            config: StepConfig::Shell {
                command: format!("echo '{id}'"),
            },
            depends_on: deps.into_iter().map(|s| s.to_string()).collect(),
            condition: None,
            timeout_seconds: None,
            on_failure: OnFailure::Abort,
            retry_policy: None,
            output_schema: None,
        }
    }

    #[test]
    fn test_kahn_sort_simple_chain() {
        let steps = vec![
            make_step("A", "Step A", vec![]),
            make_step("B", "Step B", vec!["A"]),
            make_step("C", "Step C", vec!["B"]),
        ];
        let dag = DagGraph::build(&steps).unwrap();
        let levels = dag.topological_levels();
        assert_eq!(levels.len(), 3);
        assert_eq!(levels[0].len(), 1);
        assert_eq!(levels[0][0].id, "A");
        assert_eq!(levels[1].len(), 1);
        assert_eq!(levels[1][0].id, "B");
        assert_eq!(levels[2].len(), 1);
        assert_eq!(levels[2][0].id, "C");
    }

    #[test]
    fn test_kahn_sort_parallel_branches() {
        // A → (B, C) → D
        let steps = vec![
            make_step("A", "A", vec![]),
            make_step("B", "B", vec!["A"]),
            make_step("C", "C", vec!["A"]),
            make_step("D", "D", vec!["B", "C"]),
        ];
        let dag = DagGraph::build(&steps).unwrap();
        let levels = dag.topological_levels();
        assert_eq!(levels.len(), 3);
        assert_eq!(levels[0].len(), 1); // [A]
        assert_eq!(levels[0][0].id, "A");
        assert_eq!(levels[1].len(), 2); // [B, C]
        let level1_ids: Vec<&str> = levels[1].iter().map(|s| s.id.as_str()).collect();
        assert!(level1_ids.contains(&"B"));
        assert!(level1_ids.contains(&"C"));
        assert_eq!(levels[2].len(), 1); // [D]
        assert_eq!(levels[2][0].id, "D");
    }

    #[test]
    fn test_kahn_sort_diamond() {
        // A → (B, C) → D
        let steps = vec![
            make_step("A", "A", vec![]),
            make_step("B", "B", vec!["A"]),
            make_step("C", "C", vec!["A"]),
            make_step("D", "D", vec!["B", "C"]),
        ];
        let dag = DagGraph::build(&steps).unwrap();
        let levels = dag.topological_levels();
        assert_eq!(levels.len(), 3);
    }

    #[test]
    fn test_kahn_sort_empty() {
        let dag = DagGraph::build(&[]).unwrap();
        assert!(dag.topological_levels().is_empty());
    }

    #[test]
    fn test_kahn_sort_single_node() {
        let steps = vec![make_step("A", "A", vec![])];
        let dag = DagGraph::build(&steps).unwrap();
        assert_eq!(dag.topological_levels().len(), 1);
        assert_eq!(dag.topological_levels()[0].len(), 1);
    }

    #[test]
    fn test_kahn_sort_cycle_detection() {
        let steps = vec![
            make_step("A", "A", vec!["B"]),
            make_step("B", "B", vec!["A"]),
        ];
        let err = DagGraph::build(&steps).unwrap_err();
        assert!(matches!(err, WorkflowError::InvalidDag(_)));
        assert!(err.to_string().contains("cycle"));
    }

    #[test]
    fn test_depends_on_self_reference() {
        let steps = vec![make_step("A", "A", vec!["A"])];
        let err = DagGraph::build(&steps).unwrap_err();
        assert!(matches!(err, WorkflowError::InvalidDag(_)));
        assert!(err.to_string().contains("itself"));
    }

    #[test]
    fn test_depends_on_unknown_step() {
        let steps = vec![make_step("A", "A", vec!["Z"])];
        let err = DagGraph::build(&steps).unwrap_err();
        assert!(matches!(err, WorkflowError::InvalidDag(_)));
        assert!(err.to_string().contains("unknown"));
    }
}
