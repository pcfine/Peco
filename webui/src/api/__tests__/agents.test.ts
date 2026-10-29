// 总管 Agent 图片能力探测（getAssistantSupportsImages）契约测试 —— 结果 memo、
// 并发去重、改配置失效、失败不缓存。回归背景：能力值静态，每次进 /peco 都重探
// 会随路由往返重复打请求（开发态 StrictMode 双跑再翻倍）。

import { beforeEach, describe, expect, it, vi } from "vitest";
import api from "../client";
import {
  getAssistantSupportsImages,
  updateAgent,
  __resetAgentCapabilitiesForTests,
} from "../agents";

vi.mock("../client", () => ({
  default: { get: vi.fn(), post: vi.fn(), patch: vi.fn(), delete: vi.fn() },
}));

const GET = vi.mocked(api.get);
const PATCH = vi.mocked(api.patch);

const agentList = [{ id: "a1", name: "any-agent", is_master: true }];
const detail = (supports_images: boolean) => ({ id: "a1", supports_images });

/** 队列化「listAgents → getAgent」两次响应。 */
function queueProbe(supportsImages: boolean): void {
  GET.mockResolvedValueOnce({ data: agentList }).mockResolvedValueOnce({
    data: detail(supportsImages),
  });
}

beforeEach(() => {
  __resetAgentCapabilitiesForTests();
  GET.mockReset();
  PATCH.mockReset();
});

describe("getAssistantSupportsImages", () => {
  it("重复调用命中缓存，只探一次（2 个请求）", async () => {
    queueProbe(true);

    await expect(getAssistantSupportsImages()).resolves.toBe(true);
    await expect(getAssistantSupportsImages()).resolves.toBe(true);

    expect(GET).toHaveBeenCalledTimes(2);
  });

  it("并发调用共用同一次探测（in-flight 去重）", async () => {
    queueProbe(true);

    const [a, b] = await Promise.all([
      getAssistantSupportsImages(),
      getAssistantSupportsImages(),
    ]);

    expect([a, b]).toEqual([true, true]);
    expect(GET).toHaveBeenCalledTimes(2);
  });

  it("无总管 Agent 时返回 false，且不请求 agent 详情", async () => {
    GET.mockResolvedValueOnce({
      data: [{ id: "x", name: "other", is_master: false }],
    });

    await expect(getAssistantSupportsImages()).resolves.toBe(false);
    expect(GET).toHaveBeenCalledTimes(1);
  });

  it("updateAgent 后缓存失效，下次重探", async () => {
    queueProbe(true);
    await expect(getAssistantSupportsImages()).resolves.toBe(true);

    PATCH.mockResolvedValueOnce({ data: detail(false) });
    await updateAgent("a1", {});

    queueProbe(false);
    await expect(getAssistantSupportsImages()).resolves.toBe(false);
    expect(GET).toHaveBeenCalledTimes(4);
  });

  it("探测失败不缓存，下次重试", async () => {
    GET.mockRejectedValueOnce(new Error("boom"));
    await expect(getAssistantSupportsImages()).rejects.toThrow("boom");

    queueProbe(true);
    await expect(getAssistantSupportsImages()).resolves.toBe(true);
    expect(GET).toHaveBeenCalledTimes(3);
  });
});
