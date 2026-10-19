import { useEffect, useState } from "react";
import { useParams, useNavigate } from "react-router-dom";
import { AgentForm } from "./components/AgentForm";
import { getAgent, updateAgent } from "@/api/agents";
import type { AgentDetail, UpdateAgentRequest } from "@/types/agent";
import { LoadingSpinner } from "@/components/common/LoadingSpinner";
import { PageContainer } from "@/components/common/PageContainer";
import { toast } from "sonner";

export function AgentEditPage() {
  const { agentId } = useParams<{ agentId: string }>();
  const navigate = useNavigate();
  const [agent, setAgent] = useState<AgentDetail | null>(null);

  useEffect(() => {
    if (!agentId) return;
    getAgent(agentId)
      .then(setAgent)
      .catch(() => toast.error("加载失败"));
  }, [agentId]);

  const handleSubmit = async (data: UpdateAgentRequest) => {
    if (!agentId) return;
    try {
      await updateAgent(agentId, data);
      toast.success("Agent 已更新");
      navigate("/workspace/agents");
    } catch {
      toast.error("更新失败");
    }
  };

  if (!agent) return <LoadingSpinner />;

  return (
    <div className="@container flex h-full min-h-0 flex-col">
      <PageContainer className="flex min-h-0 flex-1 flex-col space-y-0 max-lg:h-auto max-lg:pb-6">
        <AgentForm
          title="编辑 Agent"
          defaultValues={agent}
          onSubmit={(d) => handleSubmit(d as UpdateAgentRequest)}
          onCancel={() => navigate("/workspace/agents")}
        />
      </PageContainer>
    </div>
  );
}
