import { useState } from "react";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Switch } from "@/components/ui/switch";
import { Card, CardContent } from "@/components/ui/card";
import { Clock, HelpCircle } from "lucide-react";
import type { ScheduleResponse } from "@/types/workflow";

interface ScheduleConfigFormProps {
  workflowName: string;
  existing?: ScheduleResponse | null;
  onSave: (data: {
    cron: string;
    enabled: boolean;
    timezone?: string;
  }) => Promise<void>;
  onDelete?: () => Promise<void>;
  saving?: boolean;
}

const CRON_PRESETS = [
  { label: "每分钟", value: "* * * * *" },
  { label: "每5分钟", value: "*/5 * * * *" },
  { label: "每小时", value: "0 * * * *" },
  { label: "每天 09:00", value: "0 9 * * *" },
  { label: "每个工作日 09:00", value: "0 9 * * 1-5" },
  { label: "每周一 09:00", value: "0 9 * * 1" },
];

/**
 * Schedule configuration form.
 * Cron presets are provided for common use cases.
 */
export function ScheduleConfigForm({
  workflowName,
  existing,
  onSave,
  onDelete,
  saving,
}: ScheduleConfigFormProps) {
  const [cron, setCron] = useState(existing?.cron ?? "0 9 * * *");
  const [enabled, setEnabled] = useState(existing?.enabled ?? true);
  const [timezone, setTimezone] = useState(existing?.timezone ?? "");

  const handleSave = async () => {
    await onSave({
      cron,
      enabled,
      timezone: timezone || undefined,
    });
  };

  return (
    <Card>
      <CardContent className="p-4 space-y-4">
        <div className="flex items-center gap-2">
          <Clock className="h-4 w-4 text-muted-foreground" />
          <span className="font-medium text-sm">
            {existing ? "编辑调度" : "创建调度"}
          </span>
        </div>

        {/* Cron expression */}
        <div className="space-y-1.5">
          <Label className="text-xs flex items-center gap-1">
            Cron 表达式
            <span title="标准 5 位 cron 表达式：分 时 日 月 周">
              <HelpCircle className="h-3 w-3 text-muted-foreground" />
            </span>
          </Label>
          <Input
            className="h-8 text-xs font-mono"
            value={cron}
            onChange={(e) => setCron(e.target.value)}
            placeholder="0 9 * * 1-5"
          />
          <div className="flex flex-wrap gap-1 mt-1">
            {CRON_PRESETS.map((preset) => (
              <button
                key={preset.value}
                type="button"
                className="text-xs px-2 py-0.5 rounded border hover:bg-accent transition-colors"
                onClick={() => setCron(preset.value)}
              >
                {preset.label}
              </button>
            ))}
          </div>
        </div>

        {/* Timezone */}
        <div className="space-y-1.5">
          <Label className="text-xs">时区 (可选)</Label>
          <Input
            className="h-8 text-xs"
            value={timezone}
            onChange={(e) => setTimezone(e.target.value)}
            placeholder="Asia/Shanghai (默认: 系统时区)"
          />
        </div>

        {/* Enabled toggle */}
        <div className="flex items-center justify-between">
          <Label className="text-xs">启用调度</Label>
          <Switch checked={enabled} onCheckedChange={setEnabled} />
        </div>

        {/* Actions */}
        <div className="flex gap-2 pt-1">
          <Button
            size="sm"
            onClick={handleSave}
            disabled={saving || !cron.trim()}
          >
            {saving ? "保存中..." : existing ? "更新调度" : "创建调度"}
          </Button>
          {existing && onDelete && (
            <Button
              variant="destructive"
              size="sm"
              onClick={onDelete}
              disabled={saving}
            >
              删除调度
            </Button>
          )}
        </div>
      </CardContent>
    </Card>
  );
}
