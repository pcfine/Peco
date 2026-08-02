import { useEffect, useState } from "react";
import { Link } from "react-router-dom";
import { Button } from "@/components/ui/button";
import { Card, CardContent } from "@/components/ui/card";
import { LoadingSpinner } from "@/components/common/LoadingSpinner";
import { EmptyState } from "@/components/common/EmptyState";
import { listAgents, deleteAgent } from "@/api/agents";
import type { AgentListItem } from "@/types/agent";
import { Plus, Trash2, Bot, Wrench, Database, Cpu } from "lucide-react";
import { toast } from "sonner";

export function AgentListPage() {
  const [agents, setAgents] = useState<AgentListItem[]>([]);
  const [loading, setLoading] = useState(true);

  const load = () => {
    listAgents()
      .then(setAgents)
      .catch(() => toast.error("加载 Agent 列表失败"))
      .finally(() => setLoading(false));
  };

  useEffect(() => {
    load();
  }, []);

  const handleDelete = async (id: string) => {
    try {
      await deleteAgent(id);
      setAgents((prev) => prev.filter((a) => a.id !== id));
      toast.success("Agent 已删除");
    } catch {
      toast.error("删除失败");
    }
  };

  if (loading) return <LoadingSpinner />;

  return (
    <div className="max-w-4xl mx-auto space-y-4">
      <div className="flex items-center justify-between">
        <h2 className="text-2xl font-bold">Agent 管理</h2>
        <Link to="/manage/agents/new">
          <Button>
            <Plus className="mr-2 h-4 w-4" />
            创建 Agent
          </Button>
        </Link>
      </div>

      {agents.length === 0 ? (
        <EmptyState
          icon={Bot}
          title="暂无 Agent"
          description="创建一个 Agent 开始使用"
        />
      ) : (
        <div className="grid gap-3 md:grid-cols-2">
          {agents.map((a) => {
            const leftBg = a.background_color || a.color + "18";
            const isImg = a.icon.startsWith("/uploads/");
            return (
              <Link
                key={a.id}
                to={`/manage/agents/${a.id}/edit`}
                className="block"
              >
                <Card className="group h-[170px] hover:border-primary/50 transition-colors cursor-pointer overflow-hidden">
                  <CardContent className="p-3 h-full flex gap-3">
                    {/* 左侧：正方形色块/图片 */}
                    <div
                      className="aspect-square h-full shrink-0 rounded-xl overflow-hidden flex items-center justify-center"
                      style={{ background: isImg ? "transparent" : leftBg }}
                    >
                      {isImg ? (
                        <img
                          src={a.icon}
                          alt=""
                          className="h-full w-full object-cover"
                        />
                      ) : (
                        <span className="text-4xl select-none">
                          {a.icon || "🤖"}
                        </span>
                      )}
                    </div>

                    {/* 右侧：信息区 */}
                    <div className="flex-1 flex flex-col min-w-0 py-1">
                      {/* 顶部：名称 + 操作按钮 */}
                      <div className="flex items-start gap-2">
                        <p className="font-semibold truncate flex-1">
                          {a.name}
                        </p>
                        <div className="flex gap-1 opacity-0 group-hover:opacity-100 shrink-0">
                          <Button
                            variant="ghost"
                            size="icon"
                            className="h-7 w-7"
                            onClick={(e) => {
                              e.stopPropagation();
                              e.preventDefault();
                              handleDelete(a.id);
                            }}
                          >
                            <Trash2 className="h-3.5 w-3.5 text-destructive" />
                          </Button>
                        </div>
                      </div>

                      {/* 描述 */}
                      {a.description && (
                        <p className="text-xs text-muted-foreground truncate mt-0.5">
                          {a.description}
                        </p>
                      )}

                      {/* 元数据区域 */}
                      <div className="flex-1 min-h-0 mt-2 space-y-1">
                        {/* 模型 */}
                        {a.model && (
                          <div className="flex items-center gap-1 text-xs text-muted-foreground">
                            <Cpu className="h-3 w-3 shrink-0" />
                            <span className="truncate">{a.model}</span>
                          </div>
                        )}

                        {/* 工具 */}
                        {(a.tools ?? []).length > 0 && (
                          <div className="flex items-center gap-1 overflow-hidden">
                            <Wrench className="h-3 w-3 shrink-0 text-muted-foreground" />
                            {(a.tools ?? []).slice(0, 3).map((t) => (
                              <span
                                key={t}
                                className="rounded bg-accent px-1.5 py-0.5 text-xs truncate max-w-[100px]"
                              >
                                {t}
                              </span>
                            ))}
                            {(a.tools ?? []).length > 3 && (
                              <span className="text-xs text-muted-foreground shrink-0">
                                +{(a.tools ?? []).length - 3}
                              </span>
                            )}
                          </div>
                        )}

                        {/* 知识库 */}
                        {(a.knowledge_bases ?? []).length > 0 && (
                          <div className="flex items-center gap-1 overflow-hidden">
                            <Database className="h-3 w-3 shrink-0 text-muted-foreground" />
                            {(a.knowledge_bases ?? []).slice(0, 3).map((kb) => (
                              <span
                                key={kb}
                                className="rounded bg-accent px-1.5 py-0.5 text-xs truncate max-w-[100px]"
                              >
                                {kb}
                              </span>
                            ))}
                            {(a.knowledge_bases ?? []).length > 3 && (
                              <span className="text-xs text-muted-foreground shrink-0">
                                +{(a.knowledge_bases ?? []).length - 3}
                              </span>
                            )}
                          </div>
                        )}
                      </div>
                    </div>
                  </CardContent>
                </Card>
              </Link>
            );
          })}
        </div>
      )}
    </div>
  );
}
