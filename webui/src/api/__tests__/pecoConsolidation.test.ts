// 记忆自动整理 API client 契约测试 —— 路径 / 动词 / 载荷与后端逐字对齐

import { beforeEach, describe, expect, it, vi } from "vitest";
import api from "../client";
import {
  getConsolidationOptin,
  getConsolidationState,
  setConsolidationOptin,
} from "../peco";

vi.mock("../client", () => ({
  default: { get: vi.fn(), put: vi.fn() },
}));

const GET = vi.mocked(api.get);
const PUT = vi.mocked(api.put);

beforeEach(() => {
  GET.mockReset();
  PUT.mockReset();
});

describe("consolidation API", () => {
  it("GET /peco/memory/consolidation/optin 读取开关", async () => {
    GET.mockResolvedValue({ data: { enabled: true } });

    await expect(getConsolidationOptin()).resolves.toEqual({ enabled: true });
    expect(GET).toHaveBeenCalledWith("/peco/memory/consolidation/optin");
  });

  it("PUT /peco/memory/consolidation/optin 以 { enabled } 写入", async () => {
    PUT.mockResolvedValue({ data: { enabled: false } });

    await expect(setConsolidationOptin(false)).resolves.toEqual({
      enabled: false,
    });
    expect(PUT).toHaveBeenCalledWith("/peco/memory/consolidation/optin", {
      enabled: false,
    });
  });

  it("GET /peco/memory/consolidation/state 读取运行状态", async () => {
    const payload = {
      last_run_at: "2026-09-15T10:30:00Z",
      last_run_stats: { scanned: 3, merged: 1 },
    };
    GET.mockResolvedValue({ data: payload });

    await expect(getConsolidationState()).resolves.toEqual(payload);
    expect(GET).toHaveBeenCalledWith("/peco/memory/consolidation/state");
  });
});
