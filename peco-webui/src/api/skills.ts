import { api, spaceScope } from "@/lib/api";
import type { SuccessResponse } from "@/types/common";
import type { SkillInfo } from "@/types/daemon";
import type {
  SkillFile,
  SkillFileContent,
  SkillResourceFile,
} from "@/types/skill";

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
 * `files` 为本次一并落盘的资源文件；未列出的既有资源文件不会被删除。
 * 后端要求这个键存在，缺省传空数组。
 */
export async function saveSkill(
  wsId: string,
  name: string,
  content: string,
  files: SkillResourceFile[] = [],
): Promise<SuccessResponse> {
  const res = await api.put<SuccessResponse>(
    `/skills/${encodeURIComponent(name)}`,
    { content, files },
    spaceScope(wsId),
  );
  return res.data;
}

/** 列出 skill 目录内全部文件（不含根级 SKILL.md）。 */
export async function listSkillFiles(
  wsId: string,
  name: string,
): Promise<{ files: SkillFile[] }> {
  const res = await api.get<{ files: SkillFile[] }>(
    `/skills/${encodeURIComponent(name)}/files`,
    spaceScope(wsId),
  );
  return res.data;
}

/** 读取单个文件内容（服务端 256 KB 截断，二进制返回 415）。 */
export async function readSkillFile(
  wsId: string,
  name: string,
  path: string,
): Promise<SkillFileContent> {
  const res = await api.get<SkillFileContent>(
    `/skills/${encodeURIComponent(name)}/files`,
    { ...spaceScope(wsId), params: { path } },
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
