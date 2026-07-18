import { useEffect, useState } from 'react'
import { useParams, useNavigate } from 'react-router-dom'
import { AgentForm } from './components/AgentForm'
import { getAgent, updateAgent } from '@/api/agents'
import type { AgentDetail, UpdateAgentRequest } from '@/types/agent'
import { LoadingSpinner } from '@/components/common/LoadingSpinner'
import { toast } from 'sonner'

export function AgentEditPage() {
  const { agentId } = useParams<{ agentId: string }>()
  const navigate = useNavigate()
  const [agent, setAgent] = useState<AgentDetail | null>(null)

  useEffect(() => {
    if (!agentId) return
    getAgent(agentId).then(setAgent).catch(() => toast.error('加载失败'))
  }, [agentId])

  const handleSubmit = async (data: UpdateAgentRequest) => {
    if (!agentId) return
    try {
      await updateAgent(agentId, data)
      toast.success('Agent 已更新')
      navigate('/agents')
    } catch {
      toast.error('更新失败')
    }
  }

  if (!agent) return <LoadingSpinner />

  return (
    <div className="max-w-2xl mx-auto space-y-6">
      <h2 className="text-2xl font-bold">编辑 Agent</h2>
      <AgentForm defaultValues={agent} onSubmit={(d) => handleSubmit(d as UpdateAgentRequest)} />
    </div>
  )
}
