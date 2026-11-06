import { api, spaceScope } from "@/lib/api";
import type { SkillInfo } from "@/types/daemon";

export async function listSkills(wsId: string): Promise<SkillInfo[]> {
  const res = await api.get<SkillInfo[]>("/skills", spaceScope(wsId));
  return res.data;
}

/** SKILL.md 原文；服务端只回 `{name, content}`。 */
export async function getSkillMd(
  wsId: string,
  name: string,
): Promise<{ name: string; content: string }> {
  const res = await api.get<{ name: string; content: string }>(
    `/skills/${encodeURIComponent(name)}`,
    spaceScope(wsId),
  );
  return res.data;
}
