/** 当前路由是否已经显示 Peco 的会话本体：`/peco` 或 Peco 空间的详情页。 */
export function isPecoSurface(
  pathname: string,
  pecoWsId: string | null | undefined,
): boolean {
  // 地址栏里带不带尾斜杠是同一页，比较前先归一。
  const path = pathname.replace(/\/+$/, "") || "/";
  if (path === "/peco") return true;
  // `/spaces/{pecoWsId}/status` 没有聊天列，不算同一个会话本体。
  return Boolean(pecoWsId) && path === `/spaces/${pecoWsId}`;
}
