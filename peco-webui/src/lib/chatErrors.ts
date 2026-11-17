// 对话失败的分类与文案：纯函数，不碰 DOM、不碰 toast。
//
// 分类必须发生在观测点：拿到响应就当场分档（`fromResponse`），只有压根没拿到
// 响应才是「连不上」（`fromError` 的 `responded === false`）。把两者塞进同一个
// catch 是「服务端拒绝」被说成「连不上 daemon」的根源。

export type ChatFailure =
  /** 用户自己停的：不提示、不回滚。 */
  | { kind: "aborted" }
  | {
      kind: "server";
      status: number;
      code: string | null;
      detail: string | null;
    }
  /** 没拿到响应：daemon 大概没起来。 */
  | { kind: "network"; detail: string }
  /** 拿到了响应，流在中途断掉。 */
  | { kind: "stream"; detail: string };

/** 「服务端拒绝」档的兜底标题。ChatView 在流一帧未出时也用这一句。 */
export const REJECTED_TITLE = "服务端拒绝了这次请求";

const SERVER_TITLES: Record<string, string> = {
  provider_unconfigured: "没有可用的模型 provider",
  provider_key_unresolved: "provider 的密钥解析不出来",
  provider_dangling_default: "默认 provider 已经不存在",
  provider_not_found: "这个会话指定的 provider 不存在",
};

function parseBody(body: string): {
  code: string | null;
  detail: string | null;
} {
  try {
    const parsed = JSON.parse(body) as {
      code?: unknown;
      details?: unknown;
      error?: unknown;
    };
    const detail =
      typeof parsed.details === "string" && parsed.details !== ""
        ? parsed.details
        : typeof parsed.error === "string" && parsed.error !== ""
          ? parsed.error
          : null;
    return {
      code: typeof parsed.code === "string" ? parsed.code : null,
      detail,
    };
  } catch {
    // 不是 JSON 也仍是「服务端拒绝了这次请求」：绝不能退化成 network。
    const text = body.trim();
    return { code: null, detail: text === "" ? null : text };
  }
}

/** 入口 1：拿到了响应且 `!response.ok`。不会返回 network。 */
export function fromResponse(status: number, body: string): ChatFailure {
  const { code, detail } = parseBody(body);
  return { kind: "server", status, code, detail };
}

function isAbortError(err: unknown): boolean {
  return (err as { name?: unknown } | null)?.name === "AbortError";
}

function detailOf(err: unknown): string {
  if (err instanceof Error) return err.message;
  return typeof err === "string" ? err : "";
}

/** 入口 2：catch 到的异常。`responded` 由调用方局部标志给出。 */
export function fromError(err: unknown, responded: boolean): ChatFailure {
  if (isAbortError(err)) return { kind: "aborted" };
  const detail = detailOf(err);
  return responded ? { kind: "stream", detail } : { kind: "network", detail };
}

/**
 * 服务端原文 → 可展示的一句话。
 *
 * 原文可能带上配置文件路径（core 的报错里会带）。路径既不帮用户做决定，又是
 * 宿主环境细节，摆进 toast 只会误导 —— 删掉。
 */
export function userFacingDetail(text: string | null): string | undefined {
  if (!text) return undefined;
  const cleaned = text
    .replace(/(?:^|\s)[/~][^\s]*/g, " ")
    .replace(/\s{2,}/g, " ")
    .trim();
  return cleaned === "" ? undefined : cleaned;
}

/** 分类 → toast 标题 + 描述。中文、可操作。 */
export function chatFailureToast(
  failure: ChatFailure,
): { title: string; description?: string } | null {
  if (failure.kind === "aborted") return null;

  if (failure.kind === "network") {
    return {
      title: "连不上 daemon 服务（127.0.0.1:6971）",
      description: "服务可能已经停了；确认 peco-daemon 在运行后重试",
    };
  }

  if (failure.kind === "stream") {
    return { title: "连接中断", description: "这一轮的结果以下次打开会话为准" };
  }

  // 在途轮专属：与「服务端拒绝」无关，用户要做的是先停再发。
  if (failure.status === 409 && failure.code === "conversation_busy") {
    return { title: "这一轮还在跑，先停止再发" };
  }

  const title = failure.code
    ? (SERVER_TITLES[failure.code] ?? REJECTED_TITLE)
    : `${REJECTED_TITLE}（HTTP ${failure.status}）`;
  const description = userFacingDetail(failure.detail);
  return description ? { title, description } : { title };
}
