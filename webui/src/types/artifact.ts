// 对话产物类型 —— 与后端 `ArtifactItem` / `ArtifactPage` DTO 逐字段对应

/**
 * 产物类别：已知四类 + 开放兜底。
 *
 * 服务端 `kind` 是字符串，新增类别时前端不应崩在类型上，故保留 `string` 兜底；
 * 渲染层必须把未知值降级到"只能下载"（不能假定四类之外不会出现）。
 */
export type ArtifactKind =
  | "image"
  | "video"
  | "audio"
  | "document"
  | (string & {});

export interface ArtifactItem {
  id: string;
  conversation_id: string;
  kind: ArtifactKind;
  mime: string;
  title: string;
  size_bytes: number;
  /** 取流地址，形如 `/api/artifacts/{id}/content`，需 Bearer；交给 axios 前须转绝对地址 */
  url: string;
  /** 下载地址（服务端附 `Content-Disposition: attachment`） */
  download_url: string;
  /** DB `datetime('now')` 产出的 UTC 无时区标记串，展示前须经 `parseServerTimestamp` */
  created_at: string;
  /** 文件当前是否仍存在；被清理后为 false */
  available: boolean;
}

/** 会话产物分页响应；`limit`/`offset` 是服务端 clamp 后的生效值。 */
export interface ArtifactPage {
  artifacts: ArtifactItem[];
  /** 该会话产物总数，不随分页变化 */
  total: number;
  limit: number;
  offset: number;
}
