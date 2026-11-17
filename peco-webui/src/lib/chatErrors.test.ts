import { describe, expect, it } from "vitest";
import {
  chatFailureToast,
  fromError,
  fromResponse,
  userFacingDetail,
} from "@/lib/chatErrors";

function abortError(): Error {
  const err = new Error("The operation was aborted");
  err.name = "AbortError";
  return err;
}

describe("对话失败的两个入口", () => {
  it("拿到响应就按 body 分档，与服务端 code 一起带出来", () => {
    const failure = fromResponse(
      500,
      JSON.stringify({
        code: "provider_unconfigured",
        error: "no provider",
        details: "去设置 → 系统（本机）配一条",
      }),
    );

    expect(failure).toEqual({
      kind: "server",
      status: 500,
      code: "provider_unconfigured",
      detail: "去设置 → 系统（本机）配一条",
    });
  });

  it("body 不是 JSON 也仍是 server，不退化成 network", () => {
    const failure = fromResponse(500, "not json");

    expect(failure.kind).toBe("server");
    expect(failure.kind === "server" && failure.code).toBeNull();
    // 没有结构化字段时把原文当 detail：用户至少能看见服务端说了什么
    expect(failure.kind === "server" && failure.detail).toBe("not json");
  });

  it("409 conversation_busy 只由 code 认定，status 单看不成立", () => {
    const busy = fromResponse(
      409,
      JSON.stringify({ code: "conversation_busy" }),
    );
    expect(busy.kind === "server" && busy.code).toBe("conversation_busy");
    expect(chatFailureToast(busy)?.title).toBe("这一轮还在跑，先停止再发");

    // 边界：同一个 409 换了 code，不得走「先停止再发」那套文案
    const other = fromResponse(409, JSON.stringify({ code: "other" }));
    expect(other.kind === "server" && other.code).toBe("other");
    expect(chatFailureToast(other)?.title).not.toBe("这一轮还在跑，先停止再发");
  });

  it("AbortError 不提示：无论有没有拿到响应", () => {
    const beforeResponse = fromError(abortError(), false);
    const midStream = fromError(abortError(), true);

    expect(beforeResponse).toEqual({ kind: "aborted" });
    expect(midStream).toEqual({ kind: "aborted" });
    expect(chatFailureToast(beforeResponse)).toBeNull();
  });

  it("没拿到响应才算连不上；拿到了就是流中断", () => {
    expect(fromError(new TypeError("Failed to fetch"), false)).toEqual({
      kind: "network",
      detail: "Failed to fetch",
    });
    expect(fromError(new Error("terminated"), true)).toEqual({
      kind: "stream",
      detail: "terminated",
    });
  });
});

describe("失败 toast 的文案", () => {
  it("按 code 给出可读标题，details 原样接在描述里", () => {
    const unconfigured = chatFailureToast(
      fromResponse(500, JSON.stringify({ code: "provider_unconfigured" })),
    );
    expect(unconfigured?.title).toBe("没有可用的模型 provider");

    const dangling = chatFailureToast(
      fromResponse(
        500,
        JSON.stringify({
          code: "provider_dangling_default",
          details: "default_provider = 'gw' 在合并结果里不存在；请重新指定默认",
        }),
      ),
    );
    expect(dangling?.title).toContain("不存在");
    expect(dangling?.description).toContain("请重新指定默认");
  });

  it("连不上、流中断、无 code 三档各有独立标题", () => {
    expect(
      chatFailureToast({ kind: "network", detail: "Failed to fetch" })?.title,
    ).toContain("连不上 daemon 服务");
    expect(
      chatFailureToast({ kind: "stream", detail: "terminated" })?.title,
    ).toBe("连接中断");
    expect(
      chatFailureToast(
        fromResponse(503, JSON.stringify({ error: "unavailable" })),
      )?.title,
    ).toContain("HTTP 503");
  });

  it("未知 code 退到通用标题，而不是把 code 甩给用户", () => {
    const toast = chatFailureToast(
      fromResponse(500, JSON.stringify({ code: "something_new" })),
    );
    expect(toast?.title).toBe("服务端拒绝了这次请求");
    expect(toast?.description).toBeUndefined();
  });

  it("服务端原文里的配置文件路径不进 toast", () => {
    const toast = chatFailureToast(
      fromResponse(
        500,
        JSON.stringify({
          error:
            "failed to load config /home/u/.peco-daemon/providers.toml: bad toml",
        }),
      ),
    );
    expect(toast?.description).not.toContain("/home");
    expect(toast?.description).toContain("bad toml");
  });

  it("清空路径后没剩下内容就不给描述", () => {
    expect(userFacingDetail("/home/u/.peco-daemon/providers.toml")).toBe(
      undefined,
    );
    expect(userFacingDetail(null)).toBeUndefined();
  });
});
