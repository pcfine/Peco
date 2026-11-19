import { api } from "@/lib/api";
import type {
  CreateScheduleRequest,
  CronPreview,
  ScheduleRun,
  ScheduleRunAccepted,
  ScheduleView,
  UpdateScheduleRequest,
} from "@/types/daemon";

/** 列表按 `created_at` DESC，排序归服务端，前端不再排一遍。 */
export async function listSchedules(): Promise<ScheduleView[]> {
  const res = await api.get<ScheduleView[]>("/schedules");
  return res.data;
}

export async function createSchedule(
  body: CreateScheduleRequest,
): Promise<ScheduleView> {
  const res = await api.post<ScheduleView>("/schedules", body);
  return res.data;
}

export async function updateSchedule(
  id: string,
  body: UpdateScheduleRequest,
): Promise<ScheduleView> {
  const res = await api.patch<ScheduleView>(
    `/schedules/${encodeURIComponent(id)}`,
    body,
  );
  return res.data;
}

/** 历史运行记录随任务一起删除，服务端负责，前端只确认。 */
export async function deleteSchedule(id: string): Promise<{ removed: true }> {
  const res = await api.delete<{ removed: true }>(
    `/schedules/${encodeURIComponent(id)}`,
  );
  return res.data;
}

/**
 * 预览未来 3 次运行时刻。
 *
 * cron 只由服务端判：非法时 400 `invalid_cron`，`details` 是可原样展示的那句话，
 * 前端不另写一套校验，两套规则迟早会给出不同答案。
 */
export async function previewCron(cronExpr: string): Promise<CronPreview> {
  const res = await api.post<CronPreview>("/schedules/preview", {
    cron_expr: cronExpr,
  });
  return res.data;
}

/**
 * 立刻跑一轮：`pending` = 目标会话忙、已排队重试；`failed` = 同步启动就失败了
 * （空间打不开 / agent 装不上），成因看执行历史。
 */
export async function runSchedule(id: string): Promise<ScheduleRunAccepted> {
  const res = await api.post<ScheduleRunAccepted>(
    `/schedules/${encodeURIComponent(id)}/run`,
  );
  return res.data;
}

/** 运行历史按 `started_at` DESC，`limit` 由服务端截断。 */
export async function listScheduleRuns(
  id: string,
  limit = 20,
): Promise<ScheduleRun[]> {
  const res = await api.get<ScheduleRun[]>(
    `/schedules/${encodeURIComponent(id)}/runs`,
    { params: { limit } },
  );
  return res.data;
}
