// ProviderSection — Provider 配置管理区块

import { useEffect, useState } from "react";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Badge } from "@/components/ui/badge";
import { Skeleton } from "@/components/ui/skeleton";
import {
  listProviders,
  upsertProvider,
  deleteProvider,
  testProviderConnection,
} from "@/api/providers";
import type { ProviderInfo } from "@/api/providers";
import { Plus, Pencil, Trash2, Wifi, AlertCircle } from "lucide-react";
import { toast } from "sonner";

// ── Component ─────────────────────────────────────────────────────────────

export function ProviderSection() {
  const [providers, setProviders] = useState<ProviderInfo[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  // Dialog state
  const [dialogOpen, setDialogOpen] = useState(false);
  const [editing, setEditing] = useState<ProviderInfo | null>(null);
  const [formName, setFormName] = useState("");
  const [formType, setFormType] = useState("deepseek");
  const [formApiKey, setFormApiKey] = useState("");
  const [formBaseUrl, setFormBaseUrl] = useState("");
  const [formModel, setFormModel] = useState("");
  const [saving, setSaving] = useState(false);

  // Delete confirm
  const [deleteTarget, setDeleteTarget] = useState<ProviderInfo | null>(null);
  const [deleting, setDeleting] = useState(false);

  // Testing
  const [testing, setTesting] = useState<string | null>(null);

  // ── Load ─────────────────────────────────────────────────────────────

  const load = () => {
    setLoading(true);
    setError(null);
    listProviders()
      .then(setProviders)
      .catch(() => setError("加载 Provider 配置失败"))
      .finally(() => setLoading(false));
  };

  useEffect(() => {
    load();
  }, []);

  // ── Dialog helpers ────────────────────────────────────────────────────

  const openCreate = () => {
    setEditing(null);
    setFormName("");
    setFormType("deepseek");
    setFormApiKey("");
    setFormBaseUrl("");
    setFormModel("");
    setDialogOpen(true);
  };

  const openEdit = (p: ProviderInfo) => {
    setEditing(p);
    setFormName(p.name);
    setFormType(p.provider_type);
    setFormApiKey("");
    setFormBaseUrl(p.base_url ?? "");
    setFormModel(p.models[0] ?? "");
    setDialogOpen(true);
  };

  // ── Save ─────────────────────────────────────────────────────────────

  const handleSave = async () => {
    if (!formName.trim() || !formType.trim()) {
      toast.error("名称和类型不能为空");
      return;
    }
    setSaving(true);
    try {
      await upsertProvider({
        type: formType.trim(),
        api_key: formApiKey || undefined,
        base_url: formBaseUrl || undefined,
        models: formModel ? [formModel] : undefined,
      });
      toast.success(editing ? "Provider 已更新" : "Provider 已添加");
      setDialogOpen(false);
      load();
    } catch {
      toast.error("保存失败");
    } finally {
      setSaving(false);
    }
  };

  // ── Delete ───────────────────────────────────────────────────────────

  const handleDelete = async () => {
    if (!deleteTarget) return;
    setDeleting(true);
    try {
      await deleteProvider(deleteTarget.name);
      toast.success(`已删除 ${deleteTarget.name}`);
      setDeleteTarget(null);
      load();
    } catch {
      toast.error("删除失败");
    } finally {
      setDeleting(false);
    }
  };

  // ── Test connection ──────────────────────────────────────────────────

  const handleTest = async (name: string) => {
    setTesting(name);
    try {
      const result = await testProviderConnection(name);
      if (result.success) {
        toast.success(`连接测试: ${result.message ?? "成功"}`);
      } else {
        toast.error(`连接测试: ${result.message ?? "失败"}`);
      }
    } catch {
      toast.error("连接测试失败");
    } finally {
      setTesting(null);
    }
  };

  // ── Render ───────────────────────────────────────────────────────────

  return (
    <div className="space-y-4">
      <div className="flex items-center justify-between">
        <h3 className="text-sm font-medium">已配置的 Provider</h3>
        <Button variant="outline" size="sm" onClick={openCreate}>
          <Plus className="h-4 w-4 mr-1" />
          添加 Provider
        </Button>
      </div>

      {/* Loading */}
      {loading && (
        <div className="space-y-2">
          <Skeleton className="h-16 w-full" />
          <Skeleton className="h-16 w-full" />
        </div>
      )}

      {/* Error */}
      {!loading && error && (
        <div className="flex items-center gap-2 text-sm text-destructive py-2">
          <AlertCircle className="h-4 w-4" />
          <span>{error}</span>
          <Button variant="outline" size="sm" onClick={load}>
            重试
          </Button>
        </div>
      )}

      {/* Empty */}
      {!loading && !error && providers.length === 0 && (
        <p className="text-sm text-muted-foreground py-4 text-center">
          暂无 Provider 配置，点击上方按钮添加
        </p>
      )}

      {/* Provider list */}
      {!loading &&
        !error &&
        providers.map((p) => (
          <div
            key={p.name}
            className="flex items-center justify-between rounded-lg border p-4"
          >
            <div className="space-y-1 min-w-0">
              <div className="flex items-center gap-2">
                <span className="font-medium">{p.name}</span>
                <Badge variant="secondary" className="text-xs">
                  {p.provider_type}
                </Badge>
              </div>
              {p.base_url && (
                <p className="text-xs text-muted-foreground truncate">
                  {p.base_url}
                </p>
              )}
              {p.models.length > 0 && (
                <p className="text-xs text-muted-foreground">
                  Model: {p.models.join(", ")}
                </p>
              )}
            </div>
            <div className="flex items-center gap-1 shrink-0 ml-4">
              <Button
                variant="ghost"
                size="sm"
                onClick={() => handleTest(p.name)}
                disabled={testing === p.name}
              >
                <Wifi className="h-4 w-4 mr-1" />
                {testing === p.name ? "测试中…" : "测试"}
              </Button>
              <Button
                variant="ghost"
                size="icon"
                className="h-8 w-8"
                onClick={() => openEdit(p)}
              >
                <Pencil className="h-4 w-4" />
              </Button>
              <Button
                variant="ghost"
                size="icon"
                className="h-8 w-8 text-destructive hover:text-destructive"
                onClick={() => setDeleteTarget(p)}
              >
                <Trash2 className="h-4 w-4" />
              </Button>
            </div>
          </div>
        ))}

      {/* Upsert Dialog */}
      <Dialog open={dialogOpen} onOpenChange={setDialogOpen}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>
              {editing ? "编辑 Provider" : "添加 Provider"}
            </DialogTitle>
            <DialogDescription>
              {editing
                ? `修改 ${editing.name} 的配置`
                : "配置新的 LLM Provider 连接"}
            </DialogDescription>
          </DialogHeader>
          <div className="space-y-4">
            <div className="space-y-2">
              <Label>名称</Label>
              <Input
                value={formName}
                onChange={(e) => setFormName(e.target.value)}
                placeholder="deepseek"
                disabled={!!editing}
              />
              {editing && (
                <p className="text-xs text-muted-foreground">名称不可修改</p>
              )}
            </div>
            <div className="space-y-2">
              <Label>类型</Label>
              <Select value={formType} onValueChange={setFormType}>
                <SelectTrigger>
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="deepseek">DeepSeek</SelectItem>
                  <SelectItem value="openai">OpenAI</SelectItem>
                </SelectContent>
              </Select>
            </div>
            <div className="space-y-2">
              <Label>API Key</Label>
              <Input
                type="password"
                value={formApiKey}
                onChange={(e) => setFormApiKey(e.target.value)}
                placeholder={editing ? "留空则保留旧值" : "${DEEPSEEK_API_KEY}"}
              />
            </div>
            <div className="space-y-2">
              <Label>Base URL</Label>
              <Input
                value={formBaseUrl}
                onChange={(e) => setFormBaseUrl(e.target.value)}
                placeholder="https://api.deepseek.com/v1"
              />
            </div>
            <div className="space-y-2">
              <Label>默认模型</Label>
              <Input
                value={formModel}
                onChange={(e) => setFormModel(e.target.value)}
                placeholder="deepseek-v4-flash"
              />
            </div>
          </div>
          <DialogFooter>
            <Button variant="outline" onClick={() => setDialogOpen(false)}>
              取消
            </Button>
            <Button onClick={handleSave} disabled={saving}>
              {saving ? "保存中…" : "保存"}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      {/* Delete confirmation */}
      <Dialog
        open={!!deleteTarget}
        onOpenChange={(open) => {
          if (!open) setDeleteTarget(null);
        }}
      >
        <DialogContent>
          <DialogHeader>
            <DialogTitle>删除 Provider</DialogTitle>
            <DialogDescription>
              确定要删除 Provider「{deleteTarget?.name}」吗？此操作不可撤销。
            </DialogDescription>
          </DialogHeader>
          <DialogFooter>
            <Button
              variant="outline"
              onClick={() => setDeleteTarget(null)}
              disabled={deleting}
            >
              取消
            </Button>
            <Button
              variant="destructive"
              onClick={handleDelete}
              disabled={deleting}
            >
              {deleting ? "删除中…" : "删除"}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </div>
  );
}
