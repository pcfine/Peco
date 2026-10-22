/**
 * 产物取流与下载共用的唯一 URL 构造。
 *
 * 后端 DTO 的 `url` 已带 `/api` 前缀，而 axios 实例的 `baseURL` 也是 `/api`；
 * 直接把它交给实例会拼出 `/api/api/...`（axios 只在 `//` 开头时忽略 baseURL）。
 * 转成绝对地址后命中"绝对 URL"判定，baseURL 不再参与拼接，同时仍复用实例的
 * 认证与 401 拦截器。
 */
export function absoluteArtifactUrl(path: string): string {
  return new URL(path, window.location.origin).toString();
}
