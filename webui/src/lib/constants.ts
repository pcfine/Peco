export const MODELS = [
  { value: "deepseek-flash", label: "DeepSeek V4.1 Flash" },
  { value: "deepseek-v4-pro", label: "DeepSeek V4 Pro" },
] as const;

/** 默认模型 —— API 的 flash 档规范名。 */
export const DEFAULT_MODEL = "deepseek-flash";

/** 历史别名 → 规范名。`deepseek-v4-flash` 仍可调用，但 /models 已只列规范名。 */
const LEGACY_MODEL_ALIASES: Record<string, string> = {
  "deepseek-v4-flash": "deepseek-flash",
};

/** 把历史别名归一为规范模型名，未知值原样返回。 */
export function normalizeModel(model?: string): string {
  if (!model) return DEFAULT_MODEL;
  return LEGACY_MODEL_ALIASES[model] ?? model;
}

/** 上下文窗口总量（token）— 取自 provider /models 的 context_window。 */
export const DEFAULT_CONTEXT_WINDOW = 1_048_576;

export const MODEL_CONTEXT_WINDOW: Record<string, number> = {
  "deepseek-flash": 1_048_576,
  "deepseek-v4-pro": 1_048_576,
};

/** 按模型名解析上下文窗口总量，未知模型回退默认值。 */
export function contextWindowForModel(model?: string): number {
  return (model && MODEL_CONTEXT_WINDOW[normalizeModel(model)]) || DEFAULT_CONTEXT_WINDOW;
}

export const PROVIDERS = [
  { value: "deepseek", label: "DeepSeek" },
  { value: "openai", label: "OpenAI" },
] as const;

export const TOOLS = [
  { value: "shell", label: "Shell 执行" },
  { value: "fetch", label: "HTTP 请求" },
  { value: "web_search", label: "网页搜索" },
  { value: "search_knowledge", label: "知识搜索" },
  { value: "list_knowledge_bases", label: "知识库列表" },
  { value: "add_to_knowledge_base", label: "添加到知识库" },
  { value: "sync_knowledge_base", label: "同步知识库" },
] as const;

export const EMBEDDING_MODELS = [
  { value: "bge-small-zh-v15", label: "BGE Small ZH (中文轻量)" },
  { value: "bge-base-zh-v15", label: "BGE Base ZH (中文推荐)" },
  { value: "bge-large-zh-v15", label: "BGE Large ZH" },
  { value: "all-minilm-l6-v2q", label: "All-MiniLM-L6 (英文推荐)" },
] as const;

export const CHUNK_STRATEGIES = [
  { value: "overlapping-window", label: "重叠窗口" },
  { value: "fixed-size", label: "固定大小" },
  { value: "sentence-based", label: "基于句子" },
] as const;
