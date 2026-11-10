// `/schedules` —— 本期只有空态。
// 不留禁用按钮、不做假开关：一个点不动的 cron 表单比一句实话更误导。

import { PageFrame } from "@/components/layout/PageFrame";

export function SchedulesPage() {
  return (
    <PageFrame title="定时任务" contentWidth="narrow">
      <div className="space-y-1">
        <p className="text-sm font-semibold text-ink">本期尚未实现。</p>
        <p className="text-sm text-ink-muted">
          定时执行器不在本次范围内，工作流与定时执行器在后续版本。此页仅占位。
        </p>
      </div>
    </PageFrame>
  );
}
