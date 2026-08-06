import api from "./client";
import type {
  ScheduleResponse,
  CreateScheduleRequest,
  ReplaceScheduleRequest,
  UpdateScheduleRequest,
} from "@/types/workflow";
import type { SuccessResponse } from "@/types/common";

export async function listSchedules(): Promise<ScheduleResponse[]> {
  const res = await api.get<ScheduleResponse[]>("/schedules");
  return res.data;
}

export async function createSchedule(
  data: CreateScheduleRequest,
): Promise<ScheduleResponse> {
  const res = await api.post<ScheduleResponse>("/schedules", data);
  return res.data;
}

export async function replaceSchedule(
  workflowName: string,
  data: ReplaceScheduleRequest,
): Promise<ScheduleResponse> {
  const res = await api.put<ScheduleResponse>(
    `/schedules/${encodeURIComponent(workflowName)}`,
    data,
  );
  return res.data;
}

export async function updateSchedule(
  workflowName: string,
  data: UpdateScheduleRequest,
): Promise<ScheduleResponse> {
  const res = await api.patch<ScheduleResponse>(
    `/schedules/${encodeURIComponent(workflowName)}`,
    data,
  );
  return res.data;
}

export async function deleteSchedule(
  workflowName: string,
): Promise<SuccessResponse> {
  const res = await api.delete<SuccessResponse>(
    `/schedules/${encodeURIComponent(workflowName)}`,
  );
  return res.data;
}
