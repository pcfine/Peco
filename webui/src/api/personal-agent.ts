/**
 * Personal Agent API — 个人助理端点
 *
 * GET  /api/personal-agent/session  — 获取会话快照
 * DELETE /api/personal-agent/session — 清除/重置会话
 * GET  /api/personal-agent/stream   — SSE 流式对话（直接用 fetch，不走 axios）
 */

import api from './client'
import type { SessionSnapshotResponse } from '@/types/chat'

/**
 * 获取个人助理会话快照。
 * GET /api/personal-agent/session
 */
export async function getPersonalSession(): Promise<SessionSnapshotResponse> {
  const { data } = await api.get<SessionSnapshotResponse>('/personal-agent/session')
  return data
}

/**
 * 清除个人助理会话（重置对话）。
 * DELETE /api/personal-agent/session
 */
export async function clearPersonalSession(): Promise<{ success: boolean; message?: string }> {
  const { data } = await api.delete('/personal-agent/session')
  return data
}

/**
 * 构建 SSE 流式对话 URL。
 * GET /api/personal-agent/stream?message=xxx
 */
export function personalStreamUrl(message: string): string {
  return `/api/personal-agent/stream?message=${encodeURIComponent(message)}`
}
