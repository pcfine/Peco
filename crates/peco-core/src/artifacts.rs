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
}
