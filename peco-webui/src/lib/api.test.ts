// 团队级资源寻址：能力根是团队级的，请求头只有两种落点 —— 借道已打开空间，
// 或直接按团队寻址。

import { beforeEach, describe, expect, it } from "vitest";
import { api, resourceScope } from "@/lib/api";
import { useSpaceStore } from "@/stores/spaceStore";

/** 把一份请求真正发给 axios，只截住最终请求头（不动网络）。 */
async function headersOf(config: Parameters<typeof api.get>[1]) {
  let seen: Record<string, unknown> = {};
  await api.get("/agents", {
    ...config,
    adapter: async (request) => {
      seen = { ...request.headers };
      return {
        data: [],
        status: 200,
        statusText: "ok",
        headers: {},
        config: request,
      };
    },
  });
  return seen;
}

beforeEach(() => {
  useSpaceStore.getState().setCurrent("ws_current");
});

describe("resourceScope", () => {
  it("有已打开空间时发空间头", () => {
    expect(resourceScope({ teamId: "t1", wsId: "ws_a" })).toEqual({
      headers: { "X-Peco-Space": "ws_a" },
    });
  });

  it("没有空间时发团队头", () => {
    expect(resourceScope({ teamId: "t1", wsId: null })).toEqual({
      headers: { "X-Peco-Team": "t1" },
    });
  });

  it("团队头的请求不会被拦截器补上「当前空间」", async () => {
    const headers = await headersOf(
      resourceScope({ teamId: "t1", wsId: null }),
    );

    expect(headers["X-Peco-Team"]).toBe("t1");
    expect(headers["X-Peco-Space"]).toBeUndefined();
  });

  it("空间头的请求照旧带上该空间", async () => {
    const headers = await headersOf(
      resourceScope({ teamId: "t1", wsId: "ws_a" }),
    );

    expect(headers["X-Peco-Space"]).toBe("ws_a");
    expect(headers["X-Peco-Team"]).toBeUndefined();
  });
});
