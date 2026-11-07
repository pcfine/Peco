import { api, spaceScope } from "@/lib/api";
import type { SuccessResponse } from "@/types/common";
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

/**
 * 新建与改写走同一个端点：服务端把 SKILL.md 与 Tier-3 资源一起写。
 * `files` 为本轮不涉及的资源文件，固定空数组 —— 后端要求这个键存在。
 */
export async function saveSkill(
  wsId: string,
  name: string,
  content: string,
): Promise<SuccessResponse> {
  const res = await api.put<SuccessResponse>(
    `/skills/${encodeURIComponent(name)}`,
    { content, files: [] },
    spaceScope(wsId),
  );
  return res.data;
}

/** 删掉整个技能目录（SKILL.md + 资源），不动引用它的 agent 配置。 */
export async function deleteSkill(
  wsId: string,
  name: string,
): Promise<SuccessResponse> {
  const res = await api.delete<SuccessResponse>(
    `/skills/${encodeURIComponent(name)}`,
    spaceScope(wsId),
  );
  return res.data;
}
