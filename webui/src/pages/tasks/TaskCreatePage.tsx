import { useState, useEffect } from "react";
import { useNavigate } from "react-router-dom";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Textarea } from "@/components/ui/textarea";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { Switch } from "@/components/ui/switch";
import { LoadingSpinner } from "@/components/common/LoadingSpinner";
import { listAgents } from "@/api/agents";
import { createTask } from "@/api/tasks";
import type { AgentListItem } from "@/types/agent";
import { toast } from "sonner";
import cronstrue from "cronstrue";

export function TaskCreatePage() {
  const navigate = useNavigate();
  const [agents, setAgents] = useState<AgentListItem[]>([]);
  const [form, setForm] = useState({
    agent_id: "",
    name: "",
    cron_expr: "",
    prompt: "",
    enabled: true,
  });
  const [loading, setLoading] = useState(true);

  useEffect(() => {
    listAgents()
      .then(setAgents)
      .finally(() => setLoading(false));
  }, []);

  const handleSubmit = async () => {
    try {
      await createTask(form);
      toast.success("任务创建成功");
      navigate("/tasks");
    } catch {
      toast.error("创建失败，请检查 Cron 表达式");
    }
  };

  let cronDesc = "";
  try {
    cronDesc = cronstrue.toString(form.cron_expr, { locale: "zh_CN" });
  } catch {
    /* invalid */
  }

  if (loading) return <LoadingSpinner />;

  return (
    <div className="max-w-2xl mx-auto space-y-6">
      <h2 className="text-2xl font-bold">创建定时任务</h2>
      <div className="space-y-4">
        <div>
          <Label>名称</Label>
          <Input
            value={form.name}
            onChange={(e) => setForm({ ...form, name: e.target.value })}
          />
        </div>
        <div>
          <Label>Agent</Label>
          <Select
            value={form.agent_id}
            onValueChange={(v) => setForm({ ...form, agent_id: v })}
          >
            <SelectTrigger>
              <SelectValue placeholder="选择 Agent" />
            </SelectTrigger>
            <SelectContent>
              {agents.map((a) => (
                <SelectItem key={a.id} value={a.id}>
                  {a.icon} {a.name}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
        </div>
        <div>
          <Label>Cron 表达式</Label>
          <Input
            value={form.cron_expr}
            onChange={(e) => setForm({ ...form, cron_expr: e.target.value })}
            placeholder="0 9 * * *"
          />
          {cronDesc && (
            <p className="text-sm text-muted-foreground mt-1">{cronDesc}</p>
          )}
        </div>
        <div>
          <Label>提示词</Label>
          <Textarea
            value={form.prompt}
            onChange={(e) => setForm({ ...form, prompt: e.target.value })}
            rows={4}
          />
        </div>
        <div className="flex items-center gap-2">
          <Switch
            checked={form.enabled}
            onCheckedChange={(v) => setForm({ ...form, enabled: v })}
          />
          <Label>启用</Label>
        </div>
        <Button
          onClick={handleSubmit}
          className="w-full"
          disabled={!form.name || !form.agent_id || !form.cron_expr}
        >
          创建
        </Button>
      </div>
    </div>
  );
}
