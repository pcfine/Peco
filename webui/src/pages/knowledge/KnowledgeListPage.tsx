import { useEffect, useState } from "react";
import { Link } from "react-router-dom";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Textarea } from "@/components/ui/textarea";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogTrigger,
} from "@/components/ui/dialog";
import { LoadingSpinner } from "@/components/common/LoadingSpinner";
import { EmptyState } from "@/components/common/EmptyState";
import { PageHeader } from "@/components/common/PageHeader";
import { PageContainer } from "@/components/common/PageContainer";
import { CardGrid } from "@/components/common/CardGrid";
import {
  listKnowledgeBases,
  createKnowledgeBase,
  deleteKnowledgeBase,
} from "@/api/knowledge";
import type { KnowledgeBase, CreateKbRequest } from "@/types/knowledge";
import { Plus, Trash2, BookOpen } from "lucide-react";
import { toast } from "sonner";
import axios from "axios";
import { kbNameSchema } from "@/lib/validators";

// 后端错误体为 { error, details }
function getApiErrorMessage(err: unknown): string | undefined {
  if (axios.isAxiosError(err)) {
    if (err.response?.data?.details) return String(err.response.data.details);
    if (err.response?.data?.message) return String(err.response.data.message);
    if (err.message) return err.message;
  }
  if (err instanceof Error) return err.message;
  return undefined;
}

export function KnowledgeListPage() {
  const [kbs, setKbs] = useState<KnowledgeBase[]>([]);
  const [loading, setLoading] = useState(true);
  const [open, setOpen] = useState(false);
  const [form, setForm] = useState({ name: "", description: "" });

  // 空串只禁用提交，不显示错误
  const trimmedName = form.name.trim();
  const nameResult = kbNameSchema.safeParse(trimmedName);
  const nameValid = nameResult.success;
  const nameError =
    !nameResult.success && trimmedName !== ""
      ? nameResult.error.issues[0]?.message
      : null;

  const load = () => {
    listKnowledgeBases()
      .then(setKbs)
      .catch(() => toast.error("加载失败"))
      .finally(() => setLoading(false));
  };
  useEffect(() => {
    load();
  }, []);

  const handleCreate = async () => {
    try {
      const data: CreateKbRequest = {
        name: form.name,
        description: form.description,
      };
      await createKnowledgeBase(data);
      toast.success("知识库创建成功");
      setOpen(false);
      setForm({ name: "", description: "" });
      load();
    } catch (err) {
      toast.error(getApiErrorMessage(err) ?? "创建失败");
    }
  };

  const handleDelete = async (id: string) => {
    try {
      await deleteKnowledgeBase(id);
      setKbs((p) => p.filter((k) => k.id !== id));
      toast.success("已删除");
    } catch {
      toast.error("删除失败");
    }
  };

  if (loading) return <LoadingSpinner />;

  return (
    <PageContainer>
      <PageHeader
        title="知识库"
        actions={
          <Dialog open={open} onOpenChange={setOpen}>
            <DialogTrigger asChild>
              <Button>
                <Plus className="mr-2 h-4 w-4" />
                创建知识库
              </Button>
            </DialogTrigger>
            <DialogContent>
              <DialogHeader>
                <DialogTitle>新建知识库</DialogTitle>
              </DialogHeader>
              <div className="space-y-4">
                <div className="space-y-2">
                  <Label>名称</Label>
                  <Input
                    value={form.name}
                    placeholder="例如 my-kb-01"
                    aria-invalid={!!nameError}
                    onChange={(e) => setForm({ ...form, name: e.target.value })}
                  />
                  <p className="text-xs text-muted-foreground">
                    以字母或数字开头，仅可包含字母、数字、.、_、-，最多 64
                    个字符（不能以 @ 开头）
                  </p>
                  {nameError && (
                    <p className="text-sm text-destructive">{nameError}</p>
                  )}
                </div>
                <div className="space-y-2">
                  <Label>描述</Label>
                  <Textarea
                    value={form.description}
                    onChange={(e) =>
                      setForm({ ...form, description: e.target.value })
                    }
                  />
                </div>
                <Button
                  onClick={handleCreate}
                  disabled={!nameValid}
                  className="w-full"
                >
                  创建
                </Button>
              </div>
            </DialogContent>
          </Dialog>
        }
      />

      {kbs.length === 0 ? (
        <EmptyState icon={BookOpen} title="暂无知识库" />
      ) : (
        <CardGrid>
          {kbs.map((kb) => (
            <Link key={kb.id} to={`/workspace/knowledge/${kb.id}`} className="block h-full">
              <Card className="group h-full py-0 hover:bg-accent/50 transition-colors">
                <CardHeader className="flex flex-row items-center justify-between p-4">
                  <div className="min-w-0 flex-1">
                    <CardTitle className="text-base truncate">
                      {kb.name}
                    </CardTitle>
                    <p className="text-xs text-muted-foreground mt-1">
                      {kb.description || "无描述"}
                    </p>
                    <p className="text-xs text-muted-foreground mt-1">
                      {kb.document_count} 文档 · {kb.chunk_count} 分块 ·{" "}
                      {kb.embedding_model}
                    </p>
                  </div>
                  <Button
                    variant="ghost"
                    size="icon"
                    className="shrink-0 opacity-0 group-hover:opacity-100"
                    onClick={(e) => {
                      e.preventDefault();
                      handleDelete(kb.id);
                    }}
                  >
                    <Trash2 className="h-4 w-4 text-destructive" />
                  </Button>
                </CardHeader>
              </Card>
            </Link>
          ))}
        </CardGrid>
      )}
    </PageContainer>
  );
}
