// 模型名归一 —— 库里存的旧别名要映射到 /models 的规范名，否则下拉空白。

import { describe, expect, it } from "vitest";
import {
  DEFAULT_CONTEXT_WINDOW,
  MODELS,
  contextWindowForModel,
  normalizeModel,
} from "../constants";

describe("模型名", () => {
  it("候选只有 provider /models 返回的规范名", () => {
    expect(MODELS.map((m) => m.value)).toEqual([
      "deepseek-flash",
      "deepseek-v4-pro",
    ]);
  });

  it("旧别名 deepseek-v4-flash 归一为 deepseek-flash", () => {
    expect(normalizeModel("deepseek-v4-flash")).toBe("deepseek-flash");
  });

  it("规范名与未知自定义值原样返回，空值取默认", () => {
    expect(normalizeModel("deepseek-v4-pro")).toBe("deepseek-v4-pro");
    expect(normalizeModel("my-local-model")).toBe("my-local-model");
    expect(normalizeModel(undefined)).toBe("deepseek-flash");
  });

  it("上下文窗口按规范名解析，别名不落回默认值", () => {
    expect(contextWindowForModel("deepseek-v4-flash")).toBe(1_048_576);
    expect(contextWindowForModel("deepseek-v4-pro")).toBe(1_048_576);
    expect(contextWindowForModel("unknown")).toBe(DEFAULT_CONTEXT_WINDOW);
    expect(DEFAULT_CONTEXT_WINDOW).toBe(1_048_576);
  });
});
