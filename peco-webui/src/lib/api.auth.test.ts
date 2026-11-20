import { describe, expect, it } from "vitest";
import { AxiosError, type InternalAxiosRequestConfig } from "axios";
import { ApiError, api } from "@/lib/api";
import { useAuthStore } from "@/stores/authStore";

// 会话失效的唯一权威信号是 401 + `code: "unauthenticated"`。认证端点自己发这个
// 状态码时含义不同（密码不对、正在注册），不能顺手把会话态翻掉。

function rejectWith(status: number, body: Record<string, unknown>) {
  return async (config: InternalAxiosRequestConfig) => {
    const response = {
      data: body,
      status,
      statusText: "",
      headers: {},
      config,
    };
    throw new AxiosError(
      "request failed",
      "ERR_BAD_REQUEST",
      config,
      null,
      response,
    );
  };
}

function markSignedIn() {
  useAuthStore.setState({
    registered: true,
    authenticated: true,
    username: "pcfine",
    checking: false,
  });
}

describe("响应拦截器的会话失效判定", () => {
  it("普通接口 401 unauthenticated 会翻掉会话态，错误范围不变", async () => {
    markSignedIn();
    await expect(
      api.get("/spaces", {
        adapter: rejectWith(401, { code: "unauthenticated" }),
      }),
    ).rejects.toBeInstanceOf(ApiError);

    expect(useAuthStore.getState().authenticated).toBe(false);
    expect(useAuthStore.getState().username).toBeNull();
  });

  it.each(["/auth/status", "/auth/login", "/auth/register"])(
    "%s 的 401 是业务语义，不动会话态",
    async (url) => {
      markSignedIn();
      await expect(
        api.get(url, { adapter: rejectWith(401, { code: "unauthenticated" }) }),
      ).rejects.toBeInstanceOf(ApiError);

      expect(useAuthStore.getState().authenticated).toBe(true);
    },
  );

  it("401 但不是 unauthenticated（密码不对）不翻会话态", async () => {
    markSignedIn();
    await expect(
      api.post("/auth/password", undefined, {
        adapter: rejectWith(401, { code: "invalid_credentials" }),
      }),
    ).rejects.toBeInstanceOf(ApiError);

    expect(useAuthStore.getState().authenticated).toBe(true);
  });

  it("429 把 retry_after 带到 ApiError 上：登录页要数秒数", async () => {
    const error = await api
      .post("/auth/login", undefined, {
        adapter: rejectWith(429, {
          code: "too_many_attempts",
          retry_after: 42,
        }),
      })
      .catch((caught: unknown) => caught);

    expect(error).toBeInstanceOf(ApiError);
    expect((error as ApiError).code).toBe("too_many_attempts");
    expect((error as ApiError).retryAfter).toBe(42);
  });
});
