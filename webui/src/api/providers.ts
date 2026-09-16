import api from "./client";

// ── Types ─────────────────────────────────────────────────────────────────

/**
 * provider 在列表/详情中的呈现（`GET /providers`、`GET /providers/{name}`）。
 */
export interface ProviderInfo {
  /** providers.toml 中的 key，也是 agent.md `llm.provider` 引用它的名字。 */
  name: string;
  provider_type: string;
  base_url?: string;
  /** 该 provider 的默认模型；Agent 未在 agent.md 指定 model 时生效。 */
  default_model?: string;
  /** API 风格：`"responses"` | `"chat"`；缺省 = 用该类型的默认风格。 */
  api?: string;
  /** 是否已配置 API Key —— 密钥不回读，只有这个布尔值。 */
  has_api_key: boolean;
  /** 是否为当前默认 provider（agent.md 未指定 provider 时走它）。 */
  is_default: boolean;
}

/**
 * provider 类型的静态元信息（`GET /providers/types`）。
 *
 * 这是表单默认值的唯一来源：默认地址与默认模型都直接取自适配器常量，
 * 前端不维护第二份映射表，避免与真实请求地址漂移。
 */
export interface ProviderTypeInfo {
  /** `providers.toml` 中 `type` 字段的取值。 */
  provider_type: string;
  display_name: string;
  /** 不填 base_url 时适配器实际使用的地址。 */
  default_base_url: string;
  /** API 风格的合法取值，首项为该类型的默认风格。 */
  api_modes: string[];
  /** 常用模型名（自由填写，仅作候选提示）。 */
  suggested_models: string[];
  default_model: string;
}

/**
 * `PUT /providers` 请求体。
 *
 * 语义是**字段级部分更新**：省略的字段保留已存值，空串才清空。
 * 因此编辑时不要把无法回读的 api_key 置空提交 —— 省略它才是"不改动"。
 */
export interface UpsertProviderRequest {
  /** provider 逻辑名；省略时后端回退为 `type`。 */
  name?: string;
  type: string;
  api_key?: string;
  base_url?: string;
  api?: string;
  default_model?: string;
  /** 同时把该 provider 设为默认（`default_provider`）。 */
  set_default?: boolean;
}

/**
 * `POST /providers/test` 请求体 —— 与 upsert 同形的表单草稿。
 *
 * 不落盘、不影响正在运行的对话，因此字段没有已存值可回退：后端要求
 * `api_key` 与模型名非空才能真的发请求。
 */
export type TestProviderRequest = Omit<UpsertProviderRequest, "set_default">;

/** 连接测试结论；`provider_type` / `model` 是实际探针用的值，用于故障定位。 */
export interface TestConnectionResponse {
  success: boolean;
  message: string;
  provider_type: string;
  model: string;
}

export interface ProviderMutationResponse {
  success: boolean;
  message?: string;
}

// ── API ───────────────────────────────────────────────────────────────────

export async function listProviders(): Promise<ProviderInfo[]> {
  const res = await api.get<ProviderInfo[]>("/providers");
  return res.data;
}

/** 各 provider 类型的默认值目录 —— 配置表单用它预填地址与模型。 */
export async function listProviderTypes(): Promise<ProviderTypeInfo[]> {
  const res = await api.get<ProviderTypeInfo[]>("/providers/types");
  return res.data;
}

export async function getProvider(name: string): Promise<ProviderInfo> {
  const res = await api.get<ProviderInfo>(
    `/providers/${encodeURIComponent(name)}`,
  );
  return res.data;
}

export async function upsertProvider(
  data: UpsertProviderRequest,
): Promise<ProviderMutationResponse> {
  const res = await api.put<ProviderMutationResponse>("/providers", data);
  return res.data;
}

export async function deleteProvider(
  name: string,
): Promise<ProviderMutationResponse> {
  const res = await api.delete<ProviderMutationResponse>(
    `/providers/${encodeURIComponent(name)}`,
  );
  return res.data;
}

/** 用**表单当前值**测一次真实请求（保存前可测）。 */
export async function testProviderDraft(
  data: TestProviderRequest,
): Promise<TestConnectionResponse> {
  const res = await api.post<TestConnectionResponse>("/providers/test", data);
  return res.data;
}

/** 用**已保存**的配置测一次真实请求。 */
export async function testProviderConnection(
  name: string,
): Promise<TestConnectionResponse> {
  const res = await api.post<TestConnectionResponse>(
    `/providers/${encodeURIComponent(name)}/test`,
  );
  return res.data;
}
