import { api, resourceScope, type ResourceScope } from "@/lib/api";
import type { CreateKnowledgeBaseRequest, KnowledgeBase } from "@/types/daemon";

export async function listKnowledgeBases(
  scope: ResourceScope,
): Promise<KnowledgeBase[]> {
  const res = await api.get<KnowledgeBase[]>(
    "/knowledge",
    resourceScope(scope),
  );
  return res.data;
}

/** 知识库按团队隔离：同名冲突只在同一团队内成立。 */
export async function createKnowledgeBase(
  scope: ResourceScope,
  data: CreateKnowledgeBaseRequest,
): Promise<KnowledgeBase> {
  const res = await api.post<KnowledgeBase>(
    "/knowledge",
    data,
    resourceScope(scope),
  );
  return res.data;
}
