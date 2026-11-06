import { api } from "@/lib/api";
import type { PecoIdentity } from "@/types/daemon";

/** Peco 总管空间的身份。`ws_id` 由记录文件派生，不能在前端写死。 */
export async function getPeco(): Promise<PecoIdentity> {
  const res = await api.get<PecoIdentity>("/peco");
  return res.data;
}
