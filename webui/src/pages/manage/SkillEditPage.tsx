import { useEffect, useState } from "react";
import { useParams, useNavigate } from "react-router-dom";
import { Button } from "@/components/ui/button";
import { Label } from "@/components/ui/label";
import { Textarea } from "@/components/ui/textarea";
import { LoadingSpinner } from "@/components/common/LoadingSpinner";
import { getSkill, upsertSkill } from "@/api/skills";
import type { SkillDetail } from "@/types/skill";
import { ArrowLeft } from "lucide-react";
import { toast } from "sonner";

export function SkillEditPage() {
  const { skillName } = useParams<{ skillName: string }>();
  const navigate = useNavigate();
  const [skill, setSkill] = useState<SkillDetail | null>(null);
  const [content, setContent] = useState("");
  const [saving, setSaving] = useState(false);

  useEffect(() => {
    if (!skillName) return;
    getSkill(skillName)
      .then((detail) => {
        setSkill(detail);
        setContent(detail.content);
      })
      .catch(() => toast.error("加载 Skill 失败"));
  }, [skillName]);

  const handleSave = async () => {
    if (!skill || !content.trim()) return;
    setSaving(true);
    try {
      await upsertSkill(skill.name, content);
      toast.success("保存成功");
      navigate("/workspace/skills");
    } catch {
      toast.error("保存失败");
    } finally {
      setSaving(false);
    }
  };

  if (!skill) return <LoadingSpinner />;

  return (
    <div className="max-w-2xl mx-auto space-y-6">
      {/* Header */}
      <div className="flex items-center gap-4">
        <Button
          variant="ghost"
          size="icon"
          onClick={() => navigate("/workspace/skills")}
        >
          <ArrowLeft className="h-5 w-5" />
        </Button>
        <h2 className="text-2xl font-bold">编辑 Skill — {skill.name}</h2>
      </div>

      {/* Content editor */}
      <div className="space-y-2">
        <Label htmlFor="skill-content">SKILL.md 内容</Label>
        <Textarea
          id="skill-content"
          rows={28}
          className="font-mono text-sm"
          value={content}
          onChange={(e) => setContent(e.target.value)}
        />
      </div>

      {/* Actions */}
      <div className="flex gap-3">
        <Button
          type="button"
          onClick={handleSave}
          disabled={!content.trim() || saving}
        >
          {saving ? "保存中..." : "保存修改"}
        </Button>
        <Button
          type="button"
          variant="outline"
          onClick={() => navigate("/workspace/skills")}
        >
          取消
        </Button>
      </div>
    </div>
  );
}
