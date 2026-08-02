import api from "./client";
import type {
  CreateKbRequest,
  Document,
  KnowledgeBase,
  SyncResult,
} from "@/types/knowledge";
import type { SuccessResponse } from "@/types/common";

export async function listKnowledgeBases(): Promise<KnowledgeBase[]> {
  const res = await api.get<KnowledgeBase[]>("/knowledge");
  return res.data;
}

export async function createKnowledgeBase(
  data: CreateKbRequest,
): Promise<KnowledgeBase> {
  const res = await api.post<KnowledgeBase>("/knowledge", data);
  return res.data;
}

export async function getKnowledgeBase(id: string): Promise<KnowledgeBase> {
  const res = await api.get<KnowledgeBase>(`/knowledge/${id}`);
  return res.data;
}

export async function deleteKnowledgeBase(
  id: string,
): Promise<SuccessResponse> {
  const res = await api.delete<SuccessResponse>(`/knowledge/${id}`);
  return res.data;
}

export async function listDocuments(
  kbId: string,
  offset = 0,
  limit = 50,
  status?: string,
): Promise<Document[]> {
  const params: Record<string, string | number> = { offset, limit };
  if (status) params.status = status;
  const res = await api.get<Document[]>(`/knowledge/${kbId}/documents`, {
    params,
  });
  return res.data;
}

export async function uploadDocument(
  kbId: string,
  file: File,
  onProgress?: (pct: number) => void,
  signal?: AbortSignal,
): Promise<Document> {
  const formData = new FormData();
  formData.append("file", file);
  const res = await api.post<Document>(`/knowledge/${kbId}/upload`, formData, {
    headers: { "Content-Type": "multipart/form-data" },
    onUploadProgress: (e) => {
      if (e.total && onProgress)
        onProgress(Math.round((e.loaded * 100) / e.total));
    },
    signal,
  });
  return res.data;
}

export async function syncKnowledgeBase(kbId: string): Promise<SyncResult> {
  const res = await api.post<SyncResult>(`/knowledge/${kbId}/sync`);
  return res.data;
}

export async function deleteDocument(
  kbId: string,
  docId: string,
): Promise<SuccessResponse> {
  const res = await api.delete<SuccessResponse>(
    `/knowledge/${kbId}/documents/${docId}`,
  );
  return res.data;
}
