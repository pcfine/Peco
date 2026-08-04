import api from "./client";

// ── Types ─────────────────────────────────────────────────────────────────

export interface ProviderInfo {
  name: string;
  provider_type: string;
  base_url?: string;
  models: string[];
}

export interface UpsertProviderRequest {
  type: string;
  api_key?: string;
  base_url?: string;
  models?: string[];
}

export interface TestConnectionResponse {
  success: boolean;
  message?: string;
}

// ── API ───────────────────────────────────────────────────────────────────

export async function listProviders(): Promise<ProviderInfo[]> {
  const res = await api.get<ProviderInfo[]>("/providers");
  return res.data;
}

export async function getProvider(name: string): Promise<ProviderInfo> {
  const res = await api.get<ProviderInfo>(`/providers/${encodeURIComponent(name)}`);
  return res.data;
}

export async function upsertProvider(
  data: UpsertProviderRequest,
): Promise<{ success: boolean; message?: string }> {
  const res = await api.put<{ success: boolean; message?: string }>(
    "/providers",
    data,
  );
  return res.data;
}

export async function deleteProvider(
  name: string,
): Promise<{ success: boolean; message?: string }> {
  const res = await api.delete<{ success: boolean; message?: string }>(
    `/providers/${encodeURIComponent(name)}`,
  );
  return res.data;
}

export async function testProviderConnection(
  name: string,
): Promise<TestConnectionResponse> {
  const res = await api.post<TestConnectionResponse>(
    `/providers/${encodeURIComponent(name)}/test`,
  );
  return res.data;
}
