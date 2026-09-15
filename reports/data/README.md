# 标定数据集说明

| 文件 | 用途 | 结论 |
|---|---|---|
| `synthetic_calibration_pairs.csv` | **主标定集**（v2 自然语域）：53 正例（自然中文改写）+ 60 负例（同项目不同事实/不同侧面），人工编写真值 | min_cluster_cos 建议 0.77（F1 0.843）；dedup_cos 建议 0.92（P≥0.98） |
| `synthetic_calibration_pairs_hard.csv` | **边界探测集**（v1 adversarial，留档）：含同模板单值差异负例（cos 高达 0.93）与标识符压缩正例（cos 低至 0.47）——证明余弦单向量判不了这类对 | 真实系统中这类对由聚类后 Flash LLM 判官兜底，不走余弦硬删 |
| `annotation_pairs_sanitized.json` | 真实库候选对 + AI 预标注（正例 1/907）——证明真实库暂无近重复 | 真实库标定待积累后重跑 |

管线：`calibrate_thresholds --mode synthetic --pairs <csv> --out <json>` → `--mode calibrate --annotated <json>`。
