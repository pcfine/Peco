import { afterEach, describe, expect, it, vi } from "vitest";
import { toast } from "sonner";
import { ApiError } from "@/lib/api";
import { ignoreNoRunningTask } from "@/lib/turnControls";

vi.mock("sonner", () => ({ toast: { error: vi.fn(), success: vi.fn() } }));

afterEach(() => {
  vi.mocked(toast.error).mockClear();
});

describe("暂停 / 恢复的失败处理", () => {
  it("轮已结束（409 no_running_task）不当成故障", () => {
    ignoreNoRunningTask(
      new ApiError("no running task", {
        status: 409,
        code: "no_running_task",
      }),
      "暂停",
    );

    expect(toast.error).not.toHaveBeenCalled();
  });

  it("其余错误说一句，否则「按了没反应」无迹可循", () => {
    ignoreNoRunningTask(
      new ApiError("连不上 daemon", { status: undefined, code: undefined }),
      "暂停",
    );

    expect(toast.error).toHaveBeenCalledWith("暂停失败，请重试");
  });

  it("文案区分动作：两个按钮共用一个处理函数，报错要指出是哪一下", () => {
    ignoreNoRunningTask(new Error("boom"), "恢复");

    expect(toast.error).toHaveBeenCalledWith("恢复失败，请重试");
  });
});
