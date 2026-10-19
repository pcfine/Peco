// Skill types — aligned with peco-server /api/skills/*

export interface SkillListItem {
  name: string;
  description: string;
}

export interface SkillDetail {
  name: string;
  content: string;
}

/** 目录内单个文件（GET /api/skills/{name}/files）。 */
export type SkillFileKind =
  | "markdown"
  | "code"
  | "data"
  | "env"
  | "text"
  | "binary";

export interface SkillFile {
  /** 相对 skill 根目录，`/` 分隔。 */
  path: string;
  size: number;
  /** RFC3339（UTC）。 */
  mtime: string;
  kind: SkillFileKind;
  /** 能否通过现有写入 API 保存（scripts/ references/ assets/）。 */
  writable: boolean;
}

export interface SkillFileContent {
  path: string;
  size: number;
  mtime: string;
  kind: SkillFileKind;
  content: string;
  truncated: boolean;
}

/** 写入用资源文件；路径必须在 scripts/ references/ assets/ 之下。 */
export interface SkillResourceFile {
  path: string;
  content: string;
}
