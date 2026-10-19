import { useNavigate } from "react-router-dom";
import { AgentForm } from "./components/AgentForm";
import { createAgent } from "@/api/agents";
import type { CreateAgentRequest } from "@/types/agent";
import { toast } from "sonner";
import { PageContainer } from "@/components/common/PageContainer";

export function AgentCreatePage() {
  const navigate = useNavigate();

  const handleSubmit = async (data: CreateAgentRequest) => {
    try {
      const agent = await createAgent(data);
      toast.success("Agent 创建成功");
      navigate(`/workspace/agents/${agent.id}/edit`);
    } catch {
      toast.error("创建失败");
    }
  };

  return (
    <div className="@container flex h-full min-h-0 flex-col">
      <PageContainer className="flex min-h-0 flex-1 flex-col space-y-0 max-lg:h-auto max-lg:pb-6">
        <AgentForm
          title="创建 Agent"
          onSubmit={handleSubmit}
          onCancel={() => navigate("/workspace/agents")}
        />
      </PageContainer>
    </div>
  );
}
