import { useNavigate } from "react-router-dom";
import { AgentForm } from "./components/AgentForm";
import { createAgent } from "@/api/agents";
import type { CreateAgentRequest } from "@/types/agent";
import { Button } from "@/components/ui/button";
import { ArrowLeft } from "lucide-react";
import { toast } from "sonner";

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
    <div className="max-w-2xl mx-auto space-y-6">
      <div className="flex items-center gap-3">
        <Button
          variant="ghost"
          size="icon"
          onClick={() => navigate("/workspace/agents")}
        >
          <ArrowLeft className="h-5 w-5" />
        </Button>
        <h2 className="text-2xl font-bold">创建 Agent</h2>
      </div>
      <AgentForm
        onSubmit={handleSubmit}
        onCancel={() => navigate("/workspace/agents")}
      />
    </div>
  );
}
