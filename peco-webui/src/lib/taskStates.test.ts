import { describe, expect, it } from "vitest";
import {
  TASK_COMPLETED_STATE,
  TASK_ORPHAN_STATE,
  TASK_RUNNING_STATES,
  taskStateGroup,
} from "@/lib/taskStates";

describe("任务状态分组", () => {
  it.each([
    ["submitted", "active"],
    ["working", "active"],
    ["input_required", "waiting"],
    ["auth_required", "waiting"],
    ["completed", "done"],
    ["failed", "failed"],
    ["canceled", "failed"],
    ["timed_out", "failed"],
    ["rejected", "failed"],
  ])("%s → %s", (taskState, group) => {
    expect(taskStateGroup(taskState)).toBe(group);
  });

  it("orphan 自成一组：它不是失败，也不是在飞", () => {
    expect(taskStateGroup("orphan")).toBe("orphan");
  });

  it("没见过的状态按在飞处理，比按失败处理更保守", () => {
    expect(taskStateGroup("weird_new_state")).toBe("active");
  });

  it("在飞清单与完成/孤儿标记覆盖看板角标与今日列用的取值", () => {
    expect(TASK_RUNNING_STATES).toEqual(["submitted", "working"]);
    expect(TASK_COMPLETED_STATE).toBe("completed");
    expect(TASK_ORPHAN_STATE).toBe("orphan");
  });
});
