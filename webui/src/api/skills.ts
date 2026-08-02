import axios from "axios";
import api from "./client";
import type { SkillListItem, SkillDetail } from "@/types/skill";

export async function listSkills(): Promise<SkillListItem[]> {
  const res = await api.get<SkillListItem[]>("/skills");
  return res.data;
}

export async function getSkill(name: string): Promise<SkillDetail> {
  const res = await api.get<SkillDetail>(`/skills/${encodeURIComponent(name)}`);
  return res.data;
}

export async function upsertSkill(
  name: string,
  content: string,
): Promise<{ success: boolean; message?: string }> {
  const res = await api.put<{ success: boolean; message?: string }>(
    `/skills/${encodeURIComponent(name)}`,
    { content },
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
