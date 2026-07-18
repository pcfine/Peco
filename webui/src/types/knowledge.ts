// Knowledge base types — aligned with peco-server /api/knowledge/*

export interface KnowledgeBase {
  id: string
  name: string
  description: string
  backend: string
  embedding_model: string
  document_count: number
  chunk_count: number
  created_at: string
  updated_at: string
}

export interface Document {
  id: string
  kb_id: string
  filename: string
  file_size: number
  mime_type: string
  status: 'pending' | 'processing' | 'ready' | 'error'
  error_msg?: string
  created_at: string
}

export interface SyncResult {
  kb_name: string
  added: number
  updated: number
  removed: number
  skipped: number
  errors: [string, string][]
  duration_ms: number
}

export interface CreateKbRequest {
  name: string
  description?: string
  embedding_model?: string
  chunk_strategy?: ChunkStrategyRequest
}

export type ChunkStrategyRequest =
  | { type: 'overlapping-window'; size: number; overlap: number }
  | { type: 'fixed-size'; size: number }
  | { type: 'sentence-based'; max_chars: number }
