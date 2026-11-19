// 新建/编辑共用一张表单。cron 只由服务端判：非法时把 `invalid_cron` 的 `details`
// 原样摆在字段下面，前端不写第二套规则 —— 两套规则迟早会给出不同答案。

import { useEffect, useRef, useState } from "react";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
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
import { Switch } from "@/components/ui/switch";
import { Textarea } from "@/components/ui/textarea";
import { CronPresets } from "@/components/schedules/CronPresets";
import { spaceLabel } from "@/components/layout/spaceLabel";
import { previewCron } from "@/api/schedules";
import { useCreateSchedule, useUpdateSchedule } from "@/queries/schedules";
import { useSpaces } from "@/queries/spaces";
import { useSpaceStore } from "@/stores/spaceStore";
import { ApiError, errorText } from "@/lib/api";
import { formatCreatedAt } from "@/lib/format";
import type { ScheduleView, Space } from "@/types/daemon";

/** 打字停 300ms 才问一次服务端：cron 的每一段都可能是半句，逐键请求浪费且会乱序。 */
const PREVIEW_DEBOUNCE_MS = 300;

const PREVIEW_MAX = 3;

interface PreviewState {
  expr: string;
  next: string[];
}

interface ScheduleFormProps {
  schedule: ScheduleView | null;
  spaces: Space[];
  defaultWsId: string | null;
  onCancel: () => void;
  onSaved: () => void;
}

function ScheduleForm({
  schedule,
  spaces,
  defaultWsId,
  onCancel,
  onSaved,
}: ScheduleFormProps) {
  const [name, setName] = useState(schedule?.name ?? "");
  const [targetWsId, setTargetWsId] = useState(
    schedule?.target_ws_id ??
      (spaces.some((space) => space.ws_id === defaultWsId)
        ? (defaultWsId ?? "")
        : ""),
  );
  const [prompt, setPrompt] = useState(schedule?.prompt ?? "");
  const [cron, setCron] = useState(schedule?.cron_expr ?? "");
  const [enabled, setEnabled] = useState(schedule?.enabled ?? true);

  // 预览的错与提交的错分开存：预览回来得更晚，一次成功不该抹掉刚浮现的提交错误。
  const [cronPreviewError, setCronPreviewError] = useState<string | null>(null);
  const [cronSubmitError, setCronSubmitError] = useState<string | null>(null);
  const [spaceError, setSpaceError] = useState<string | null>(null);
  const [formError, setFormError] = useState<string | null>(null);
  const [preview, setPreview] = useState<PreviewState | null>(null);
  const [previewPending, setPreviewPending] = useState(false);

  // 服务端把错归到哪个字段，就把焦点搬过去：留在提交按钮上，用户看不见错在哪。
  const cronRef = useRef<HTMLInputElement>(null);
  const spaceRef = useRef<HTMLButtonElement>(null);

  const create = useCreateSchedule();
  const update = useUpdateSchedule();
  const saving = create.isPending || update.isPending;

  const expr = cron.trim();
  // 提交错误属于提交时的那条表达式：一改动就不再成立。
  const cronError = cronSubmitError ?? cronPreviewError;

  useEffect(() => {
    setCronSubmitError(null);
    if (expr === "") {
      setPreview(null);
      setCronPreviewError(null);
      setPreviewPending(false);
      return;
    }
    let cancelled = false;
    setPreviewPending(true);
    const timer = setTimeout(() => {
      previewCron(expr)
        .then((result) => {
          if (cancelled) return;
          setPreview({ expr, next: result.next });
          setCronPreviewError(null);
        })
        .catch((error: unknown) => {
          if (cancelled) return;
          setPreview(null);
          setCronPreviewError(errorText(error));
        })
        .finally(() => {
          if (!cancelled) setPreviewPending(false);
        });
    }, PREVIEW_DEBOUNCE_MS);
    return () => {
      cancelled = true;
      clearTimeout(timer);
    };
  }, [expr]);

  const applyServerError = (error: unknown) => {
    const code = error instanceof ApiError ? error.code : undefined;
    const message = errorText(error);
    if (code === "invalid_cron") {
      setCronSubmitError(message);
      cronRef.current?.focus();
    } else if (code === "space_not_found") {
      setSpaceError(message);
      spaceRef.current?.focus();
    } else setFormError(message);
  };

  const trimmedName = name.trim();
  const trimmedPrompt = prompt.trim();
  const incomplete =
    trimmedName === "" ||
    targetWsId === "" ||
    trimmedPrompt === "" ||
    expr === "";

  const submit = () => {
    setFormError(null);
    setSpaceError(null);
    const body = {
      name: trimmedName,
      target_ws_id: targetWsId,
      prompt: trimmedPrompt,
      cron_expr: expr,
      enabled,
    };
    const options = { onSuccess: onSaved, onError: applyServerError };
    if (schedule) {
      update.mutate({ id: schedule.id, body }, options);
    } else {
      create.mutate(body, options);
    }
  };

  // 预览只认当前表达式：上一条的结果在新输入还没回来时不该继续摆在那里。
  const shownPreview = preview && preview.expr === expr ? preview.next : null;
  const previewTimes = (shownPreview ?? [])
    .map((raw) => formatCreatedAt(raw))
    .filter((text) => text !== "")
    .slice(0, PREVIEW_MAX);

  return (
    <form
      className="space-y-3"
      onSubmit={(event) => {
        event.preventDefault();
        submit();
      }}
    >
      <div className="space-y-1">
        <Label
          htmlFor="schedule-name"
          className="text-caption font-medium text-ink-faint"
        >
          名称（必填）
        </Label>
        <Input
          id="schedule-name"
          name="name"
          value={name}
          onChange={(event) => setName(event.target.value)}
          autoComplete="off"
          placeholder="例如 每日早报"
          className="h-8"
        />
      </div>

      <div className="space-y-1">
        <Label
          htmlFor="schedule-space"
          className="text-caption font-medium text-ink-faint"
        >
          目标空间（必填）
        </Label>
        <Select
          value={targetWsId}
          onValueChange={setTargetWsId}
          disabled={spaces.length === 0}
        >
          <SelectTrigger
            ref={spaceRef}
            id="schedule-space"
            aria-label="目标空间"
            aria-invalid={spaceError !== null}
            className="h-8 w-full"
          >
            <SelectValue placeholder="选择空间" />
          </SelectTrigger>
          <SelectContent>
            {spaces.map((space) => (
              <SelectItem key={space.ws_id} value={space.ws_id}>
                {spaceLabel(space)}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
        {spaceError ? (
          <p className="text-caption font-medium text-danger">{spaceError}</p>
        ) : spaces.length === 0 ? (
          <p className="text-caption text-ink-faint">
            还没有可用空间，先在工作台建一个再回来
          </p>
        ) : null}
      </div>

      <div className="space-y-1">
        <Label
          htmlFor="schedule-prompt"
          className="text-caption font-medium text-ink-faint"
        >
          任务内容（必填）
        </Label>
        <Textarea
          id="schedule-prompt"
          name="prompt"
          value={prompt}
          onChange={(event) => setPrompt(event.target.value)}
          aria-describedby="schedule-prompt-hint"
          className="min-h-[72px] text-sm"
        />
        <p id="schedule-prompt-hint" className="text-caption text-ink-faint">
          这段话会作为一条用户消息投进该空间的 @master
          会话，和你在那儿发一条消息一样。
        </p>
      </div>

      <div className="space-y-1">
        <Label
          htmlFor="schedule-cron"
          className="text-caption font-medium text-ink-faint"
        >
          执行计划（必填，cron）
        </Label>
        <Input
          ref={cronRef}
          id="schedule-cron"
          name="cron_expr"
          value={cron}
          onChange={(event) => setCron(event.target.value)}
          autoComplete="off"
          spellCheck={false}
          translate="no"
          placeholder="0 9 * * *"
          aria-invalid={cronError !== null}
          aria-describedby="schedule-cron-status"
          className="h-8 font-mono text-xs"
        />
        <CronPresets value={cron} onPick={setCron} />
        <div id="schedule-cron-status" aria-live="polite" className="space-y-1">
          {cronError ? (
            <p className="break-words text-caption font-medium text-danger">
              {cronError}
            </p>
          ) : expr === "" ? (
            <p className="text-caption text-ink-faint">
              五段式：分 时 日 月 周，按服务端时区（UTC）解释
            </p>
          ) : previewTimes.length > 0 ? (
            <div className="text-caption text-ink-faint">
              <span>下次 {previewTimes.length} 次运行（本机时区）：</span>
              <ul className="flex flex-wrap gap-x-3 tabular-nums text-ink-muted">
                {previewTimes.map((text) => (
                  <li key={text}>{text}</li>
                ))}
              </ul>
            </div>
          ) : previewPending ? (
            <p className="text-caption text-ink-faint">正在预览…</p>
          ) : null}
        </div>
      </div>

      <div className="flex items-center gap-2">
        <Switch
          id="schedule-enabled"
          checked={enabled}
          onCheckedChange={setEnabled}
        />
        <Label
          htmlFor="schedule-enabled"
          className="text-caption font-medium text-ink-faint"
        >
          启用
        </Label>
        <span className="text-caption text-ink-faint">
          停用后配置保留，只是不再按计划投递
        </span>
      </div>

      {formError && (
        <p
          aria-live="polite"
          className="break-words rounded-md border border-destructive/30 bg-destructive/10 px-2 py-1 text-caption font-medium text-danger"
        >
          {formError}
        </p>
      )}

      <DialogFooter>
        <Button
          type="button"
          variant="ghost"
          size="sm"
          disabled={saving}
          onClick={onCancel}
        >
          取消
        </Button>
        <Button type="submit" size="sm" disabled={saving || incomplete}>
          {schedule ? "保存" : "新建"}
        </Button>
      </DialogFooter>
    </form>
  );
}

interface ScheduleFormDialogProps {
  open: boolean;
  /** `null` = 新建。 */
  schedule: ScheduleView | null;
  onOpenChange: (open: boolean) => void;
}

export function ScheduleFormDialog({
  open,
  schedule,
  onOpenChange,
}: ScheduleFormDialogProps) {
  const spaces = useSpaces();
  const currentWsId = useSpaceStore((state) => state.currentWsId);

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>
            {schedule ? "编辑定时任务" : "新建定时任务"}
          </DialogTitle>
          <DialogDescription>
            按 cron 把一段话投进目标空间的 @master
            会话；投递与运行状态在行内展开的 历史里看。
          </DialogDescription>
        </DialogHeader>
        {/* 表单状态放在 DialogContent 里：关掉即卸载，下次打开是干净的一份。 */}
        <ScheduleForm
          schedule={schedule}
          spaces={spaces.data ?? []}
          defaultWsId={currentWsId}
          onCancel={() => onOpenChange(false)}
          onSaved={() => onOpenChange(false)}
        />
      </DialogContent>
    </Dialog>
  );
}
