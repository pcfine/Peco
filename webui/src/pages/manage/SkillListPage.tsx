import { useEffect, useRef, useState } from "react";
import { Button } from "@/components/ui/button";
import { Card, CardContent } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Textarea } from "@/components/ui/textarea";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { LoadingSpinner } from "@/components/common/LoadingSpinner";
import { EmptyState } from "@/components/common/EmptyState";
import {
  listSkills,
  getSkill,
  upsertSkill,
  deleteSkill,
  importSkill,
  exportSkill,
} from "@/api/skills";
import type { SkillListItem, SkillDetail } from "@/types/skill";
import { Upload, Download, Trash2, Puzzle } from "lucide-react";
import { toast } from "sonner";
import axios from "axios";

function getApiErrorMessage(err: unknown): string | undefined {
  if (axios.isAxiosError(err)) {
    if (err.response?.data?.message) return String(err.response.data.message);
    if (err.message) return err.message;
  }
  if (err instanceof Error) return err.message;
  return undefined;
}

export function SkillListPage() {
  const [skills, setSkills] = useState<SkillListItem[]>([]);
  const [loading, setLoading] = useState(true);

  // Import dialog
  const [importDialogOpen, setImportDialogOpen] = useState(false);
  const [importForm, setImportForm] = useState({ name: "", content: "" });
  const [importing, setImporting] = useState(false);

  // Edit dialog
  const [editDialogOpen, setEditDialogOpen] = useState(false);
  const [editingSkill, setEditingSkill] = useState<SkillDetail | null>(null);
  const [editContent, setEditContent] = useState("");
  const [editLoading, setEditLoading] = useState(false);
  const [saving, setSaving] = useState(false);

  // Delete confirmation
  const [deleteTarget, setDeleteTarget] = useState<SkillListItem | null>(null);

  const unmountedRef = useRef(false);

  const load = () => {
    listSkills()
      .then((list) => {
        if (!unmountedRef.current) setSkills(list);
      })
      .catch(() => {
        if (!unmountedRef.current) toast.error("加载 Skill 列表失败");
      })
      .finally(() => {
        if (!unmountedRef.current) setLoading(false);
      });
  };

  useEffect(() => {
    unmountedRef.current = false;
    load();
    return () => {
      unmountedRef.current = true;
    };
  }, []);

  // ── Import ───────────────────────────────────────────────────────────────────

  const openImport = () => {
    setImportForm({ name: "", content: "" });
    setImportDialogOpen(true);
  };

  const closeImport = () => {
    setImportDialogOpen(false);
  };

  const canImport = importForm.name.trim() && importForm.content.trim();

  const handleImport = async () => {
    if (!canImport) return;
    setImporting(true);
    try {
      await importSkill(importForm.name.trim(), importForm.content);
      toast.success("导入成功");
      closeImport();
      load();
    } catch (err) {
      toast.error(getApiErrorMessage(err) || "导入失败");
    } finally {
      setImporting(false);
    }
  };

  // ── Edit ─────────────────────────────────────────────────────────────────────

  const openEdit = async (name: string) => {
    if (editDialogOpen || editLoading) return; // prevent double-click opening multiple dialogs
    setEditLoading(true);
    try {
      const detail = await getSkill(name);
      setEditingSkill(detail);
      setEditContent(detail.content);
      setEditDialogOpen(true);
    } catch (err) {
      toast.error(getApiErrorMessage(err) || "加载 Skill 详情失败");
    } finally {
      setEditLoading(false);
    }
  };

  const closeEdit = () => {
    setEditDialogOpen(false);
    setEditingSkill(null);
    setEditContent("");
  };

  const handleSaveEdit = async () => {
    if (!editingSkill || !editContent.trim()) return;
    setSaving(true);
    try {
      await upsertSkill(editingSkill.name, editContent);
      toast.success("保存成功");
      closeEdit();
      load();
    } catch (err) {
      toast.error(getApiErrorMessage(err) || "保存失败");
    } finally {
      setSaving(false);
    }
  };

  // ── Delete ───────────────────────────────────────────────────────────────────

  const confirmDelete = (skill: SkillListItem) => {
    setDeleteTarget(skill);
  };

  const handleDelete = async () => {
    if (!deleteTarget) return;
    const name = deleteTarget.name;
    try {
      await deleteSkill(name);
      setSkills((prev) => prev.filter((s) => s.name !== name));
      toast.success("已删除");
      // If editing the deleted skill, close the edit dialog
      if (editingSkill?.name === name) {
        closeEdit();
      }
    } catch (err) {
      toast.error(getApiErrorMessage(err) || "删除失败");
    } finally {
      setDeleteTarget(null);
    }
  };

  // ── Export ───────────────────────────────────────────────────────────────────

  const handleExport = async (name: string) => {
    try {
      await exportSkill(name);
      toast.success("导出成功");
    } catch (err) {
      toast.error(getApiErrorMessage(err) || "导出失败");
    }
  };

  // ── Render ───────────────────────────────────────────────────────────────────

  if (loading) return <LoadingSpinner />;

  return (
    <div className="max-w-4xl mx-auto space-y-4">
      {/* Header */}
      <div className="flex items-center justify-between">
        <h2 className="text-2xl font-bold">Skill 管理</h2>
        <Button onClick={openImport}>
          <Upload className="mr-2 h-4 w-4" />
          导入 Skill
        </Button>
      </div>

      {/* Empty state */}
      {skills.length === 0 ? (
        <EmptyState
          icon={Puzzle}
          title="暂无 Skill"
          description="导入一个 Skill 开始使用"
          action={
            <Button onClick={openImport}>
              <Upload className="mr-2 h-4 w-4" />
              导入 Skill
            </Button>
          }
        />
      ) : (
        <div className="grid gap-3 md:grid-cols-2">
          {skills.map((skill) => (
            <Card
              key={skill.name}
              className={`group hover:border-primary/50 transition-colors cursor-pointer ${editLoading ? "pointer-events-none opacity-60" : ""}`}
              onClick={() => openEdit(skill.name)}
            >
              <CardContent className="p-4 flex items-start gap-3">
                <div className="flex h-10 w-10 shrink-0 items-center justify-center rounded-lg bg-accent">
                  <Puzzle className="h-5 w-5" />
                </div>
                <div className="flex-1 min-w-0">
                  <p className="font-medium truncate">{skill.name}</p>
                  <p className="text-xs text-muted-foreground line-clamp-2 mt-0.5">
                    {skill.description || "无描述"}
                  </p>
                </div>

                {/* Actions — visible on hover */}
                <div className="flex gap-0.5 opacity-0 group-hover:opacity-100 transition-opacity shrink-0">
                  <Button
                    variant="ghost"
                    size="icon"
                    onClick={(e) => {
                      e.stopPropagation();
                      handleExport(skill.name);
                    }}
                    title="导出"
                  >
                    <Download className="h-4 w-4" />
                  </Button>
                  <Button
                    variant="ghost"
                    size="icon"
                    onClick={(e) => {
                      e.stopPropagation();
                      confirmDelete(skill);
                    }}
                    title="删除"
                  >
                    <Trash2 className="h-4 w-4 text-destructive" />
                  </Button>
                </div>
              </CardContent>
            </Card>
          ))}
        </div>
      )}

      {/* Delete Confirmation Dialog */}
      <Dialog open={!!deleteTarget} onOpenChange={() => setDeleteTarget(null)}>
        <DialogContent className="sm:max-w-md">
          <DialogHeader>
            <DialogTitle>确认删除</DialogTitle>
            <DialogDescription>
              确定要删除 Skill{" "}
              <span className="font-semibold">{deleteTarget?.name}</span>{" "}
              吗？此操作不可撤销。
            </DialogDescription>
          </DialogHeader>
          <DialogFooter>
            <Button variant="outline" onClick={() => setDeleteTarget(null)}>
              取消
            </Button>
            <Button variant="destructive" onClick={handleDelete}>
              删除
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      {/* Import Dialog */}
      <Dialog
        open={importDialogOpen}
        onOpenChange={(open) => !open && closeImport()}
      >
        <DialogContent className="sm:max-w-lg">
          <DialogHeader>
            <DialogTitle>导入 Skill</DialogTitle>
            <DialogDescription>
              创建一个新的 Skill，或覆盖同名的已有 Skill
            </DialogDescription>
          </DialogHeader>

          <div className="space-y-4">
            <div className="space-y-1.5">
              <Label htmlFor="skill-name">Skill 名称</Label>
              <Input
                id="skill-name"
                placeholder="my-skill"
                value={importForm.name}
                onChange={(e) =>
                  setImportForm((p) => ({ ...p, name: e.target.value }))
                }
              />
            </div>
            <div className="space-y-1.5">
              <Label htmlFor="skill-content">SKILL.md 内容</Label>
              <Textarea
                id="skill-content"
                rows={14}
                className="font-mono text-sm"
                placeholder={
                  "---\nname: my-skill\ndescription: 我的技能\n---\n\n# 系统提示词\n..."
                }
                value={importForm.content}
                onChange={(e) =>
                  setImportForm((p) => ({ ...p, content: e.target.value }))
                }
              />
            </div>
            <p className="text-xs text-muted-foreground">
              如果名称已存在，将覆盖现有 Skill
            </p>
          </div>

          <DialogFooter>
            <Button variant="outline" onClick={closeImport}>
              取消
            </Button>
            <Button onClick={handleImport} disabled={!canImport || importing}>
              {importing ? "导入中..." : "导入"}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      {/* Edit Dialog */}
      <Dialog
        open={editDialogOpen}
        onOpenChange={(open) => !open && closeEdit()}
      >
        <DialogContent className="sm:max-w-2xl">
          <DialogHeader>
            <DialogTitle>{editingSkill?.name} — 编辑</DialogTitle>
            <DialogDescription>编辑 SKILL.md 内容</DialogDescription>
          </DialogHeader>

          <div className="space-y-4">
            <div className="space-y-1.5">
              <Label htmlFor="skill-edit-content">内容</Label>
              <Textarea
                id="skill-edit-content"
                rows={22}
                className="font-mono text-sm"
                value={editContent}
                onChange={(e) => setEditContent(e.target.value)}
              />
            </div>
          </div>

          <DialogFooter>
            <Button variant="outline" onClick={closeEdit}>
              取消
            </Button>
            <Button
              onClick={handleSaveEdit}
              disabled={!editContent.trim() || saving}
            >
              {saving ? "保存中..." : "保存"}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </div>
  );
}
