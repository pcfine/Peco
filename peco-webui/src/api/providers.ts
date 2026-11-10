// 团队级 provider 配置：providers.toml 住在团队能力根下，路径里已带 team_id，
// 与「当前空间」无关，所以这些请求不带 `X-Peco-Space`。

import { api } from "@/lib/api";

/** `GET .../providers` 的条目。密钥永不回读，只暴露是否已配置。 */
export interface ProviderInfo {
  /** providers.toml 的 key，也是 agent.md `llm.provider` 引用它的名字。 */
  name: string;
  provider_type: string;
  /** `null` = 用该类型的默认地址。 */
  base_url: string | null;
  /** `null` = 没设默认模型，模型由 agent.md 决定。 */
  default_model: string | null;
  /** API 风格；`null` = 用该类型的默认风格。 */
  api: string | null;
  has_api_key: boolean;
  is_default: boolean;
  /**
   * 这条配置来自哪一层：`team` 是本团队文件里写的（可改），`peco` 与 `system`
   * 是继承来的（只读）。
   */
  source?: ProviderSource;
}

/** 生效 provider 的层来源；低 → 高为 `system` → `peco` → `team`。 */
export type ProviderSource = "team" | "peco" | "system";

/** 条目来自哪一层。字段缺失按 `team` 处理：整列表一律只读比误判更糟。 */
export function providerSource(provider: ProviderInfo): ProviderSource {
  return provider.source ?? "team";
}

/** provider 类型目录 —— 表单默认值的唯一来源，前端不另存一份映射表。 */
export interface ProviderTypeInfo {
  provider_type: string;
  display_name: string;
  default_base_url: string;
  /** 合法 API 风格，首项为该类型的默认风格。 */
  api_modes: string[];
  /** 常用模型名，仅作候选提示。 */
  suggested_models: string[];
  default_model: string;
}

/**
 * `PUT .../providers` 请求体：字段级部分更新 —— 省略表示保留已存值，空串表示清空。
 *
 * `api_key` 无法回读，编辑时必须省略而不是传空串，否则会清掉已存密钥。
 */
export interface UpsertProviderRequest {
  /** 省略时服务端回退为 `type`。 */
  name?: string;
  type: string;
  api_key?: string;
  base_url?: string;
  api?: string;
  default_model?: string;
  set_default?: boolean;
}

/** `POST .../providers/test` 请求体：草稿没有已存值可回退，字段直传。 */
export interface TestDraftRequest {
  name?: string;
  type: string;
  api_key: string;
  base_url: string;
  api: string;
  default_model: string;
}

/** 连接探针结论；`provider_type` / `model` 是实际探到的值，用于定位。 */
export interface TestResponse {
  success: boolean;
  message: string;
  provider_type: string;
  model: string;
}

export interface ProviderMutationResponse {
  success: boolean;
  message?: string;
}

function providersPath(teamId: string): string {
  return `/teams/${encodeURIComponent(teamId)}/providers`;
}

export async function listProviders(teamId: string): Promise<ProviderInfo[]> {
  const res = await api.get<ProviderInfo[]>(providersPath(teamId));
  return res.data;
}

export async function listProviderTypes(
  teamId: string,
): Promise<ProviderTypeInfo[]> {
  const res = await api.get<ProviderTypeInfo[]>(
    `${providersPath(teamId)}/types`,
  );
  return res.data;
}

export async function getProvider(
  teamId: string,
  name: string,
): Promise<ProviderInfo> {
  const res = await api.get<ProviderInfo>(
    `${providersPath(teamId)}/${encodeURIComponent(name)}`,
  );
  return res.data;
}

export async function upsertProvider(
  teamId: string,
  body: UpsertProviderRequest,
): Promise<ProviderMutationResponse> {
  const res = await api.put<ProviderMutationResponse>(
    providersPath(teamId),
    body,
  );
  return res.data;
}

export async function deleteProvider(
  teamId: string,
  name: string,
): Promise<ProviderMutationResponse> {
  const res = await api.delete<ProviderMutationResponse>(
    `${providersPath(teamId)}/${encodeURIComponent(name)}`,
  );
  return res.data;
}

/** 用**表单当前值**测一次真实请求：不落盘，保存前可测。 */
export async function testProviderDraft(
  teamId: string,
  body: TestDraftRequest,
): Promise<TestResponse> {
  const res = await api.post<TestResponse>(
    `${providersPath(teamId)}/test`,
    body,
  );
  return res.data;
}

/** 用**已保存**的配置测一次真实请求。 */
export async function testProviderConnection(
  teamId: string,
  name: string,
): Promise<TestResponse> {
  const res = await api.post<TestResponse>(
    `${providersPath(teamId)}/${encodeURIComponent(name)}/test`,
    undefined,
  );
  return res.data;
}
