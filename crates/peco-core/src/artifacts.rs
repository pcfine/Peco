//! 产物声明协议（`@@PECO_ARTIFACT`）。
//!
//! # 为什么是 stdout 协议而不是专用工具
//!
//! 工具层拿不到 conversation 上下文（`shell_exec(command, cwd)` 没有会话参数），
//! 而「这个文件属于哪次对话」只能由 SSE 流那一侧判定。因此让**脚本自己声明**
//! 产物路径，工具执行点只负责剥离与收集，归属判定留给宿主层。
//!
//! 对模型零心智负担：脚本固定输出声明行，模型既不需要记得调用工具，也看不到
//! 这些行（剥离发生在写入会话历史之前）。
//!
//! # 协议
//!
//! ```text
//! @@PECO_ARTIFACT {"path":"/abs/path/a.jpg","title":"红狐狸","mime":"image/jpeg"}
//! ```
//!
//! - 每行一条，行首允许前导空白，`path` 必填，`title` / `mime` 可选。
//! - 声明行**一律剥离**，不进入 `result`，因此不会进模型上下文。
//! - JSON 非法时丢弃该声明并告警；不保留原文（否则模型每轮都读到噪声）。
//!
//! # 子 agent 路径的不对称
//!
//! delegate 路径上，模型自写于子 agent 正文里的声明会被父层剥离并升级为父级候选；
//! parallel 路径上同样位置的声明被 JSON 转义，剥不掉，因此不会上浮。该不对称在
//! 声明上浮之前即已存在，不是缺陷 —— 这里记录它，免得后来者把它当 bug「修」掉。

use model_provider::{Content, ContentPart};
use serde::{Deserialize, Serialize};
use tracing::warn;

/// 声明行前缀。
pub const ARTIFACT_MARKER: &str = "@@PECO_ARTIFACT";

/// 一条产物声明。
///
/// `path` 交由宿主层校验（必须落在该用户的 `output/` 下），本模块不做路径判断
/// —— 这里只是解析，不承担安全边界。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactDecl {
    /// 产物绝对路径（或宿主机可解析的路径）。
    pub path: String,
    /// 展示标题；缺省时由文件名推导。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// 媒体类型提示；缺省时由扩展名推导。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime: Option<String>,
}

/// 剥离声明行并收集声明。
///
/// 返回 `(clean, decls)`：`clean` 是去掉所有声明行后的文本（喂给模型的历史），
/// `decls` 是解析成功的声明。非法 JSON 的声明行同样被剥离，只告警不保留。
pub fn strip_artifact_lines(text: &str) -> (String, Vec<ArtifactDecl>) {
    // 绝大多数工具输出不含声明，先做一次廉价判定避免逐行扫描的分配。
    if !text.contains(ARTIFACT_MARKER) {
        return (text.to_string(), Vec::new());
    }

    let mut kept = String::with_capacity(text.len());
    let mut decls = Vec::new();

    for line in text.split_inclusive('\n') {
        let trimmed = line.trim_start();
        let Some(rest) = trimmed.strip_prefix(ARTIFACT_MARKER) else {
            kept.push_str(line);
            continue;
        };

        // 前缀必须与正文分开：`@@PECO_ARTIFACTX` 不是声明，按普通行保留。
        let rest = rest.trim_start();
        if !rest.is_empty() && !rest.starts_with('{') {
            kept.push_str(line);
            continue;
        }

        match serde_json::from_str::<ArtifactDecl>(rest) {
            Ok(decl) => decls.push(decl),
            Err(e) => warn!(
                error = %e,
                line = %rest,
                "Malformed artifact declaration dropped"
            ),
        }
    }

    (kept, decls)
}

/// 对工具的 `Content` 结果剥离声明，返回 `(clean, decls)`。
///
/// `Parts` 中的文本部件一并处理（MCP 工具可能同时回图和文本），全部文本被剥离
/// 干净时退回 `Content::Text("")`，避免出现空部件数组。
pub fn strip_content(content: Content) -> (Content, Vec<ArtifactDecl>) {
    match content {
        Content::Text(text) => {
            let (clean, decls) = strip_artifact_lines(&text);
            (Content::Text(clean), decls)
        }
        Content::Parts(parts) => {
            let mut decls = Vec::new();
            let mut kept = Vec::with_capacity(parts.len());
            for part in parts {
                match part {
                    ContentPart::Text { text } => {
                        let (clean, mut found) = strip_artifact_lines(&text);
                        decls.append(&mut found);
                        if !clean.is_empty() {
                            kept.push(ContentPart::Text { text: clean });
                        }
                    }
                    other => kept.push(other),
                }
            }
            if kept.is_empty() {
                (Content::Text(String::new()), decls)
            } else {
                (Content::Parts(kept), decls)
            }
        }
    }
}

/// 把声明渲染为声明行块：每条恰好一行，行尾带换行。
///
/// 序列化交给 `serde_json`：手写 `format!` 拼 JSON 在 `path` 含引号、换行或非 ASCII
/// 时会产出非法或跨行的声明行，而跨行声明必然剥不干净。
pub(crate) fn render_artifact_lines(decls: &[ArtifactDecl]) -> String {
    let mut out = String::new();
    for decl in decls {
        out.push_str(ARTIFACT_MARKER);
        out.push(' ');
        out.push_str(&serde_json::to_string(decl).expect("ArtifactDecl is always serializable"));
        out.push('\n');
    }
    out
}

/// 把声明行块注入到 `text` **顶部**，返回可直接下发给模型/宿主的文本。
///
/// 必须前置而非后置：后缀注入在 `text` 不以换行结尾时会把 marker 粘到正文行尾，
/// 于是不再匹配「行首」规则，声明剥不掉、反倒漏进模型上下文。
pub(crate) fn with_artifact_lines(text: &str, decls: &[ArtifactDecl]) -> String {
    if decls.is_empty() {
        return text.to_string();
    }
    let mut out = render_artifact_lines(decls);
    out.push_str(text);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_declaration_and_keeps_output() {
        let text = "生成完成\n@@PECO_ARTIFACT {\"path\":\"/a/b.jpg\"}\n共 1 个文件\n";
        let (clean, decls) = strip_artifact_lines(text);
        assert_eq!(clean, "生成完成\n共 1 个文件\n");
        assert_eq!(decls.len(), 1);
        assert_eq!(decls[0].path, "/a/b.jpg");
        assert_eq!(decls[0].title, None);
    }

    #[test]
    fn accepts_indented_declaration() {
        let text = "  @@PECO_ARTIFACT {\"path\":\"/a/b.mp4\",\"title\":\"视频\"}\n";
        let (clean, decls) = strip_artifact_lines(text);
        assert_eq!(clean, "");
        assert_eq!(decls.len(), 1);
        assert_eq!(decls[0].title.as_deref(), Some("视频"));
    }

    #[test]
    fn collects_multiple_declarations() {
        let text = concat!(
            "@@PECO_ARTIFACT {\"path\":\"/a/1.jpg\"}\n",
            "middle\n",
            "@@PECO_ARTIFACT {\"path\":\"/a/2.mp4\",\"mime\":\"video/mp4\"}\n",
        );
        let (clean, decls) = strip_artifact_lines(text);
        assert_eq!(clean, "middle\n");
        assert_eq!(decls.len(), 2);
        assert_eq!(decls[1].mime.as_deref(), Some("video/mp4"));
    }

    #[test]
    fn drops_malformed_declaration_without_leaking_marker() {
        let text = "@@PECO_ARTIFACT {not json}\nok\n";
        let (clean, decls) = strip_artifact_lines(text);
        assert_eq!(clean, "ok\n");
        assert!(decls.is_empty(), "malformed declaration must not be kept");
    }

    #[test]
    fn leaves_similar_prefix_untouched() {
        // 少见但真实：别的工具可能输出以 marker 开头但非声明的文本
        let text = "@@PECO_ARTIFACTX 不是声明\n";
        let (clean, decls) = strip_artifact_lines(text);
        assert_eq!(clean, text);
        assert!(decls.is_empty());
    }

    #[test]
    fn text_without_marker_is_returned_unchanged() {
        let text = "plain output\n";
        let (clean, decls) = strip_artifact_lines(text);
        assert_eq!(clean, text);
        assert!(decls.is_empty());
    }

    #[test]
    fn strip_content_handles_parts_and_collects_from_all_text_parts() {
        use model_provider::ContentPart;

        let content = Content::Parts(vec![
            ContentPart::Text {
                text: "@@PECO_ARTIFACT {\"path\":\"/a/1.jpg\"}\n看图\n".to_string(),
            },
            ContentPart::Image {
                url: "data:image/png;base64,AAA".to_string(),
                detail: None,
            },
            ContentPart::Text {
                text: "@@PECO_ARTIFACT {\"path\":\"/a/2.jpg\"}".to_string(),
            },
        ]);
        let (clean, decls) = strip_content(content);
        assert_eq!(decls.len(), 2);
        // 全被剥离的文本部件不保留；图片部件原样保留
        match clean {
            Content::Parts(parts) => {
                assert_eq!(parts.len(), 2);
                assert_eq!(
                    parts[0],
                    ContentPart::Text {
                        text: "看图\n".to_string()
                    }
                );
            }
            other => panic!("expected Parts, got {other:?}"),
        }
    }

    #[test]
    fn strip_content_text_leaves_no_marker() {
        let (clean, decls) = strip_content(Content::Text(
            "@@PECO_ARTIFACT {\"path\":\"/a/b.jpg\"}\n".to_string(),
        ));
        assert_eq!(decls.len(), 1);
        assert_eq!(clean, Content::Text(String::new()));
    }

    #[test]
    fn declaration_only_output_yields_empty_text() {
        let text = "@@PECO_ARTIFACT {\"path\":\"/a/b.jpg\"}\n";
        let (clean, decls) = strip_artifact_lines(text);
        assert_eq!(clean, "");
        assert_eq!(decls.len(), 1);
    }

    // ── 声明渲染 / 注入（子 agent 路径上浮）────────────────────────────────

    fn decl(path: &str) -> ArtifactDecl {
        ArtifactDecl {
            path: path.to_string(),
            title: None,
            mime: None,
        }
    }

    /// 变异点：`with_artifact_lines` 改成后缀注入，或漏掉换行 ⇒ 正文还原失败。
    #[test]
    fn with_then_strip_round_trips_single_decl() {
        let d = decl("/a/b.jpg");
        let text = with_artifact_lines("正文", std::slice::from_ref(&d));
        let (clean, decls) = strip_artifact_lines(&text);
        assert_eq!(clean, "正文");
        assert_eq!(decls, vec![d]);
    }

    /// 组合律：注入只可能把「声明」加在前面，正文自身的解析结果必须原样保留。
    ///
    /// 变异点：注入点改成后缀（或忘加换行）会让 `t` 自带的声明、缩进声明、
    /// 相似前缀这几类样本还原错位 ⇒ 断言红。
    #[test]
    fn with_artifact_lines_composes_with_strip() {
        let d = decl("/a/new.jpg");
        let samples = [
            "",
            "x",
            "x\n",
            "第一行\n第二行\n",
            "@@PECO_ARTIFACT {\"path\":\"/a/own.jpg\"}\n正文",
            "  @@PECO_ARTIFACT {\"path\":\"/a/indented.jpg\"}\n",
            "@@PECO_ARTIFACTX 不是声明\n",
            "  \n\t\n",
        ];
        for t in samples {
            let (own_clean, own_decls) = strip_artifact_lines(t);
            let mut expected_decls = vec![d.clone()];
            expected_decls.extend(own_decls);
            let injected = with_artifact_lines(t, std::slice::from_ref(&d));
            assert_eq!(
                strip_artifact_lines(&injected),
                (own_clean, expected_decls),
                "sample {t:?} must round-trip"
            );

            // 空声明表：必须逐字节不动（不得追加空行、不得改空白）
            assert_eq!(with_artifact_lines(t, &[]), t);
        }
    }

    /// 一条声明恒一行：`path` / `title` 里的换行、引号、反斜杠、marker 文本
    /// 都必须被 JSON 转义，否则声明会跨行、剥不干净。
    ///
    /// 变异点：改用手写 `format!` 拼 JSON ⇒ 含换行/引号的样本产出多行或非法 JSON。
    #[test]
    fn render_emits_exactly_one_line_per_decl() {
        let cases = [
            ArtifactDecl {
                path: "/a/b.jpg".into(),
                title: None,
                mime: None,
            },
            ArtifactDecl {
                path: "/a/with\nnewline.jpg".into(),
                title: Some("标题带换行\n第二行".into()),
                mime: Some("image/jpeg".into()),
            },
            ArtifactDecl {
                path: format!("/a/{ARTIFACT_MARKER}.jpg"),
                title: None,
                mime: None,
            },
            ArtifactDecl {
                path: "/a/红狐狸.jpg".into(),
                title: Some("带\"引号\"的标题".into()),
                mime: None,
            },
            ArtifactDecl {
                path: "/a/back\\slash.jpg".into(),
                title: Some("反斜杠 \\ 样本".into()),
                mime: None,
            },
        ];

        for d in &cases {
            let rendered = render_artifact_lines(std::slice::from_ref(d));
            assert_eq!(
                rendered.lines().count(),
                1,
                "one decl must be one line: {d:?}"
            );
            assert!(
                rendered.ends_with('\n'),
                "decl line must be newline-terminated"
            );
            assert_eq!(strip_artifact_lines(&rendered).1, vec![d.clone()]);
        }
    }

    /// 空声明表渲染为空串（调用方据此判断是否需要注入）。
    #[test]
    fn render_empty_decls_is_empty_string() {
        assert_eq!(render_artifact_lines(&[]), "");
    }

    /// 非法 JSON 声明行照样剥离、正文保留、不 panic。
    ///
    /// 变异点：注入函数若对 JSON 失败 `unwrap` 或保留原文 ⇒ 要么 panic，要么 marker 漏回正文。
    #[test]
    fn malformed_declaration_line_is_stripped_without_panic() {
        let text = format!("{ARTIFACT_MARKER} {{broken json\n正文\n");
        let (clean, decls) = strip_artifact_lines(&text);
        assert_eq!(clean, "正文\n");
        assert!(decls.is_empty());
    }

    /// `@@PECO_ARTIFACTX` 这类相似前缀不是声明，注入后仍按普通行保留。
    ///
    /// 变异点：放宽前缀判定到「以 marker 开头即可」⇒ 该行被剥，正文缺行。
    #[test]
    fn similar_prefix_is_not_a_declaration_even_after_injection() {
        let t = "@@PECO_ARTIFACTX 相似前缀\n";
        let d = decl("/a/b.jpg");
        let injected = with_artifact_lines(t, std::slice::from_ref(&d));
        let (clean, decls) = strip_artifact_lines(&injected);
        assert_eq!(clean, t);
        assert_eq!(decls, vec![d]);
    }
}
