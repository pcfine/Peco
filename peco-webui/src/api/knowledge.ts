import { api, spaceScope } from "@/lib/api";
import type { KnowledgeBase } from "@/types/daemon";

export async function listKnowledgeBases(
  wsId: string,
): Promise<KnowledgeBase[]> {
  const res = await api.get<KnowledgeBase[]>("/knowledge", spaceScope(wsId));
  return res.data;
}
