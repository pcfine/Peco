import axios from "axios";
import api from "./client";
import type {
  SkillListItem,
  SkillDetail,
  SkillFile,
  SkillFileContent,
  SkillResourceFile,
} from "@/types/skill";

export async function listSkills(): Promise<SkillListItem[]> {
  const res = await api.get<SkillListItem[]>("/skills");
  return res.data;
}

export async function getSkill(name: string): Promise<SkillDetail> {
  const res = await api.get<SkillDetail>(`/skills/${encodeURIComponent(name)}`);
  return res.data;
}

/**
 * 写入 SKILL.md；`files` 为要一并落盘的资源文件（仅 scripts/ references/ assets/）。
 * 未列入 `files` 的既有资源文件不会被删除。
 */
export async function upsertSkill(
  name: string,
  content: string,
  files: SkillResourceFile[] = [],
): Promise<{ success: boolean; message?: string }> {
  const res = await api.put<{ success: boolean; message?: string }>(
    `/skills/${encodeURIComponent(name)}`,
    { content, files },
  );
  return res.data;
}

/** 列出 skill 目录内全部文件（不含根级 SKILL.md）。 */
export async function listSkillFiles(
  name: string,
): Promise<{ files: SkillFile[] }> {
  const res = await api.get<{ files: SkillFile[] }>(
    `/skills/${encodeURIComponent(name)}/files`,
  );
  return res.data;
}

/** 读取单个文件内容（服务端 256 KB 截断，二进制返回 415）。 */
export async function readSkillFile(
  name: string,
  path: string,
): Promise<SkillFileContent> {
  const res = await api.get<SkillFileContent>(
    `/skills/${encodeURIComponent(name)}/files`,
    { params: { path } },
  );
  return res.data;
}

export async function deleteSkill(
  name: string,
): Promise<{ success: boolean; message?: string }> {
  const res = await api.delete<{ success: boolean; message?: string }>(
    `/skills/${encodeURIComponent(name)}`,
  );
  return res.data;
}

export async function importSkill(
  name: string,
  content: string,
): Promise<{ success: boolean; message?: string }> {
  const res = await api.post<{ success: boolean; message?: string }>(
    "/skills/import",
    { name, content },
  );
  return res.data;
}

export async function exportSkill(name: string): Promise<void> {
  try {
    const res = await api.get(`/skills/${encodeURIComponent(name)}/export`, {
      responseType: "blob",
    });
    const url = window.URL.createObjectURL(res.data as Blob);
    const a = document.createElement("a");
    a.href = url;
    a.download = `${name}.SKILL.md`;
    document.body.appendChild(a);
    a.click();
    document.body.removeChild(a);
    window.URL.revokeObjectURL(url);
  } catch (err: unknown) {
    // Try to extract server error message from blob response body
    let message: string | undefined;
    if (axios.isAxiosError(err) && err.response?.data instanceof Blob) {
      try {
        const text = await (err.response.data as Blob).text();
        const parsed = JSON.parse(text);
        message = parsed.message;
      } catch {
        // blob is not JSON (e.g. HTML error page), ignore
      }
    }
    if (message) throw new Error(message);
    throw err instanceof Error ? err : new Error("导出失败");
  }
}
