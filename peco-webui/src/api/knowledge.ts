import { api, spaceScope } from "@/lib/api";
import type { CreateKnowledgeBaseRequest, KnowledgeBase } from "@/types/daemon";

export async function listKnowledgeBases(
  wsId: string,
): Promise<KnowledgeBase[]> {
  const res = await api.get<KnowledgeBase[]>("/knowledge", spaceScope(wsId));
  return res.data;
}

/** 知识库按团队隔离：同名冲突只在同一团队内成立。 */
export async function createKnowledgeBase(
  wsId: string,
  data: CreateKnowledgeBaseRequest,
): Promise<KnowledgeBase> {
  const res = await api.post<KnowledgeBase>(
    "/knowledge",
    data,
    spaceScope(wsId),
  );
  return res.data;
}
