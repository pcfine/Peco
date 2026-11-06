import { api } from "@/lib/api";
import type { Task } from "@/types/daemon";

export interface TaskFilter {
  target_ws_id?: string;
  origin_ws_id?: string;
  state?: string;
  limit?: number;
}

/** 台账查询；`limit` 由服务端截断，调用方不要自己再切一遍。 */
export async function listTasks(filter: TaskFilter = {}): Promise<Task[]> {
  const res = await api.get<Task[]>("/tasks", { params: filter });
  return res.data;
}
