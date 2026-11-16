// 全站唯一的 axios 实例：请求头归属与错误形状都从这里出，调用方不各拼一份。

import axios, { AxiosError } from "axios";
import { useSpaceStore } from "@/stores/spaceStore";

/**
 * 服务端 `{code, message, ...}` 的统一投影：UI 只需要一句可展示的话。
 *
 * `inflightDispatch` / `inflightForeign` 只随 `inflight_tasks` 下发 —— 关闭空间
 * 的 409 要把这两个数摆给用户看，从这里取比再去查一遍空间详情更准（那一刻的数）。
 */
export class ApiError extends Error {
  readonly status?: number;
  readonly code?: string;
  readonly inflightDispatch?: number;
  readonly inflightForeign?: number;

  constructor(
    message: string,
    options: {
      status?: number;
      code?: string;
      inflightDispatch?: number;
      inflightForeign?: number;
    } = {},
  ) {
    super(message);
    this.name = "ApiError";
    this.status = options.status;
    this.code = options.code;
    this.inflightDispatch = options.inflightDispatch;
    this.inflightForeign = options.inflightForeign;
  }
}

interface ErrorBody {
  error?: string;
  details?: string;
  message?: string;
  code?: string;
  inflight_dispatch?: number;
  inflight_foreign?: number;
}

export const api = axios.create({
  baseURL: "/api",
  timeout: 30_000,
});

api.interceptors.request.use((config) => {
  const wsId = useSpaceStore.getState().currentWsId;
  // 请求已自带头时不覆盖：dock 固定落在 Peco 空间，与「当前空间」无关。
  // 团队头也算自带头 —— 补一个空间头会让资源请求落到当前空间所属的团队。
  const scoped = Boolean(
    config.headers["X-Peco-Space"] || config.headers["X-Peco-Team"],
  );
  if (wsId && !scoped) {
    config.headers["X-Peco-Space"] = wsId;
  }
  return config;
});

/** 显式指定请求空间；不传就由拦截器按「当前空间」决定。 */
export function spaceScope(wsId?: string | null) {
  return wsId ? { headers: { "X-Peco-Space": wsId } } : undefined;
}

/**
 * 资源请求的落点：团队能力根。
 *
 * `wsId` 只表示「该团队有一个已打开的空间可借道」；能力根本身是团队级的，
 * 没有空间照样能读写，所以缺省走团队选择器。
 */
export interface ResourceScope {
  teamId: string;
  wsId: string | null;
}

/** 资源模块的请求头：有已打开空间时用空间寻址，否则直接按团队寻址。 */
export function resourceScope(scope: ResourceScope) {
  return {
    headers: scope.wsId
      ? { "X-Peco-Space": scope.wsId }
      : { "X-Peco-Team": scope.teamId },
  };
}

api.interceptors.response.use(
  (response) => response,
  (error: AxiosError<ErrorBody>) => {
    const body = error.response?.data;
    const message =
      body?.details ||
      body?.error ||
      body?.message ||
      (error.response
        ? `请求失败（HTTP ${error.response.status}）`
        : "连不上 daemon，请确认它还在运行");
    return Promise.reject(
      new ApiError(message, {
        status: error.response?.status,
        code: body?.code,
        inflightDispatch: body?.inflight_dispatch,
        inflightForeign: body?.inflight_foreign,
      }),
    );
  },
);

/** 非 axios 来源的错误也要能进 UI 文案。 */
export function errorText(error: unknown): string {
  if (error instanceof Error && error.message) return error.message;
  return "请求失败";
}
