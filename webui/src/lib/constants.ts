export const MODELS = [
  { value: 'deepseek-v4-flash', label: 'DeepSeek V4 Flash' },
  { value: 'deepseek-v4-pro', label: 'DeepSeek V4 Pro' },
  { value: 'deepseek-v4', label: 'DeepSeek V4' },
] as const

export const PROVIDERS = [
  { value: 'deepseek', label: 'DeepSeek' },
  { value: 'openai', label: 'OpenAI' },
] as const

export const TOOLS = [
  { value: 'shell_exec', label: 'Shell 执行' },
  { value: 'fetch', label: 'HTTP 请求' },
  { value: 'search_knowledge', label: '知识搜索' },
  { value: 'list_knowledge_bases', label: '知识库列表' },
  { value: 'add_to_knowledge_base', label: '添加到知识库' },
  { value: 'sync_knowledge_base', label: '同步知识库' },
] as const

export const EMBEDDING_MODELS = [
  { value: 'bge-small-zh-v15', label: 'BGE Small ZH (中文推荐)' },
  { value: 'bge-large-zh-v15', label: 'BGE Large ZH' },
  { value: 'all-minilm-l6-v2q', label: 'All-MiniLM-L6 (英文推荐)' },
] as const

export const CHUNK_STRATEGIES = [
  { value: 'overlapping-window', label: '重叠窗口' },
  { value: 'fixed-size', label: '固定大小' },
  { value: 'sentence-based', label: '基于句子' },
] as const
