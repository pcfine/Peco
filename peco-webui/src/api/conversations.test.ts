// 会话命令的落点：空间错一个，服务端按「不属于该空间」直接 404。
// 只换 adapter —— 拦截器与参数序列化照原样跑，请求头与 query 都在这里看得见。

import type { InternalAxiosRequestConfig } from "axios";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { api } from "@/lib/api";
import { pauseConversation, resumeConversation } from "@/api/conversations";
import { useSpaceStore } from "@/stores/spaceStore";

const adapter = vi.fn(async (config: InternalAxiosRequestConfig) => ({
  data: { success: true },
  status: 200,
  statusText: "ok",
  headers: {},
  config,
}));

function lastRequest() {
  const config = adapter.mock.calls.at(-1)?.[0];
  if (!config) throw new Error("adapter 没被调用");
  return { uri: api.getUri(config), headers: config.headers };
}

beforeEach(() => {
  adapter.mockClear();
  api.defaults.adapter = adapter;
  // 「当前空间」故意与命令要落的会话空间不同：dock 恒落在 Peco 空间上。
  useSpaceStore.getState().setCurrent("ws_current");
});

describe("pauseConversation", () => {
  it("带上会话所属的空间，压过「当前空间」", async () => {
    await pauseConversation("agent_1", "conv_1", { wsId: "ws_x" });

    const { uri, headers } = lastRequest();
    expect(uri).toContain("/chat/agent_1/conversations/conv_1/pause");
    expect(uri).toContain("ws=ws_x");
    expect(headers["X-Peco-Space"]).toBe("ws_x");
  });

  it("不传空间时仍由拦截器按当前空间兜底", async () => {
    await pauseConversation("agent_1", "conv_1");

    const { uri, headers } = lastRequest();
    expect(uri).not.toContain("ws=");
    expect(headers["X-Peco-Space"]).toBe("ws_current");
  });
});

describe("resumeConversation", () => {
  it("带上会话所属的空间，压过「当前空间」", async () => {
    await resumeConversation("agent_1", "conv_1", { wsId: "ws_x" });

    const { uri, headers } = lastRequest();
    expect(uri).toContain("/chat/agent_1/conversations/conv_1/resume");
    expect(uri).toContain("ws=ws_x");
    expect(headers["X-Peco-Space"]).toBe("ws_x");
  });
});
