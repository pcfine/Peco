// 产物 blob 取流：模块级缓存 + 并发队列 + 引用计数 + LRU 淘汰
//
// 浏览器给 `<img src>` / `<video src>` 不会带 Authorization，所以取流必须走
// axios 实例（带 Bearer、401 登出拦截器、下载进度），再包成 object URL。
// 不 import React、不 import store：调用方（hook）负责把"404 该不该改列表状态"
// 写回 store，缓存模块只回答"这份字节能不能拿到"。

import api from "./client";
import { absoluteArtifactUrl } from "./artifactUrl";
import type { ArtifactItem } from "@/types/artifact";

/** 超过此体积的视频/音频不取流播放，只给下载（整份文件进内存太贵）。 */
export const ARTIFACT_INLINE_BLOB_MAX_BYTES = 50 * 1024 * 1024;
/** 下载硬上限：blob 方案下整个文件进内存，超过就不再让浏览器尝试。 */
export const ARTIFACT_BLOB_MAX_BYTES = 200 * 1024 * 1024;

/** 同源并发约 6，SSE 长连接要占位；面板一次进视口十几张图，不设限会把流式文字挤到队尾。 */
const MAX_CONCURRENT_REQUESTS = 3;
const CACHE_MAX_BYTES = 64 * 1024 * 1024;
const CACHE_MAX_ENTRIES = 40;
const RETRY_DELAY_MS = 500;
const MAX_RETRIES = 1;

export type ArtifactBlobErrorKind = "unavailable" | "network";

/**
 * 取流失败。`kind` 是可判别契约：`unavailable` 表示文件确实取不到（永不重试，
 * 调用方应把条目标成已过期），`network` 表示可再试。
 */
export class ArtifactBlobError extends Error {
  readonly kind: ArtifactBlobErrorKind;

  constructor(kind: ArtifactBlobErrorKind, message: string) {
    super(message);
    this.name = "ArtifactBlobError";
    this.kind = kind;
  }
}

export interface ArtifactBlob {
  url: string;
  mime: string;
}

export interface ArtifactBlobOptions {
  /** 只影响调用方是否继续等待；底层请求不受它影响（见 `getArtifactObjectUrl`）。 */
  signal?: AbortSignal;
  /**
   * 本次请求由用户主动发起（打开查看器 / 下载）。缺省视为主动请求。
   *
   * 滚动预取必须显式传 `false`：只有标了预取的请求才会在排队期间无人持有时
   * 被丢弃（否则滚过几百张图会把整份文件逐个下完）。缺省为 true 是因为取不到
   * 数据是调用方无法解释的结果，而"忘记标预取"最多多占一份缓存。
   */
  requestedByUser?: boolean;
  onProgress?: (percent: number) => void;
}

interface BlobEntry {
  id: string;
  status: "loading" | "ready" | "error";
  /** 同 id 只有一条在飞 promise，缓存与所有调用方共享它。 */
  promise?: Promise<ArtifactBlob>;
  url?: string;
  mime?: string;
  bytes?: number;
  /** 单调时钟读数，仅用于 LRU 排序。 */
  lastUsed: number;
  requestedByUser: boolean;
  error?: ArtifactBlobErrorKind;
}

interface QueuedRequest {
  id: string;
  url: string;
  requestedByUser: boolean;
  onProgress?: (percent: number) => void;
  resolve: (blob: ArtifactBlob) => void;
  reject: (err: Error) => void;
}

const cache = new Map<string, BlobEntry>();
/**
 * 引用计数独立于缓存条目：进视口即 retain、随后才入队取流，条目的创建往往
 * 晚于第一次 retain，计数若挂在条目上就会丢掉这段"先持有后请求"的时序。
 */
const refs = new Map<string, number>();
const queue: QueuedRequest[] = [];
let activeRequests = 0;
let overBoundWarned = false;

function refCount(id: string): number {
  return refs.get(id) ?? 0;
}

function cacheBytes(): number {
  let total = 0;
  for (const entry of cache.values()) total += entry.bytes ?? 0;
  return total;
}

function baseMime(raw: string): string {
  return raw.split(";")[0].trim().toLowerCase();
}

/** 从 `readonly` 的角度淘汰：只 revoke，不动其它状态（调用方负责从 Map 删除）。 */
function revoke(entry: BlobEntry): void {
  if (entry.url !== undefined) URL.revokeObjectURL(entry.url);
  entry.url = undefined;
}

/** 最久未使用、且能安全 revoke 的条目；`loading` 没有 URL 可撤销，不算候选。 */
function oldestEvictable(): BlobEntry | undefined {
  let oldest: BlobEntry | undefined;
  for (const entry of cache.values()) {
    if (entry.status !== "ready" || refCount(entry.id) > 0) continue;
    if (!oldest || entry.lastUsed < oldest.lastUsed) oldest = entry;
  }
  return oldest;
}

/** 循环淘汰至回到界内：只减一条会让"界外"停在那里等下一个事件。 */
function evict(): void {
  let bytes = cacheBytes();
  while (cache.size > CACHE_MAX_ENTRIES || bytes > CACHE_MAX_BYTES) {
    const victim = oldestEvictable();
    if (!victim) {
      // 剩下的都在视口里或在飞：revoke 正在渲染的 URL 会裂图，宁可暂时超出上界
      if (!overBoundWarned) {
        overBoundWarned = true;
        console.warn(
          `[artifactBlob] 缓存超界但无可淘汰条目：${cache.size} 条 / ${bytes} 字节`,
        );
      }
      return;
    }
    bytes -= victim.bytes ?? 0;
    revoke(victim);
    cache.delete(victim.id);
  }
  overBoundWarned = false;
}

function queueRequest(task: QueuedRequest): void {
  if (task.requestedByUser) {
    // 用户主动请求插队，否则点"播放"会排在十几张缩略图后面
    const firstPrefetch = queue.findIndex((t) => !t.requestedByUser);
    if (firstPrefetch < 0) queue.push(task);
    else queue.splice(firstPrefetch, 0, task);
  } else {
    queue.push(task);
  }
  pump();
}

function pump(): void {
  while (activeRequests < MAX_CONCURRENT_REQUESTS && queue.length > 0) {
    const task = queue.shift() as QueuedRequest;
    const entry = cache.get(task.id);
    // 出队前复核：预取任务排队期间可能已滚出视口，此时再下整份文件既没人要、
    // 又因为"loading 不淘汰"和"signal 不透传"而无法回收
    if (!entry || (refCount(task.id) === 0 && !entry.requestedByUser)) {
      cache.delete(task.id);
      // 按取消而非失败收尾：文件本身没问题，只是没人等了
      task.reject(abortError());
      continue;
    }
    activeRequests += 1;
    void runRequest(task, entry);
  }
}

const UNAVAILABLE_STATUSES = new Set([403, 404, 410]);

function classifyFailure(err: unknown): {
  error: ArtifactBlobError;
  retryable: boolean;
} {
  const status = (err as { response?: { status?: number } })?.response?.status;
  if (status !== undefined && UNAVAILABLE_STATUSES.has(status)) {
    return {
      error: new ArtifactBlobError(
        "unavailable",
        `产物已不可用（HTTP ${status}）`,
      ),
      retryable: false,
    };
  }
  // 5xx 与网络中断是瞬时的，重试一次；401 交拦截器登出，其余 4xx 重试也不会变
  return {
    error: new ArtifactBlobError(
      "network",
      `取流失败（${status !== undefined ? `HTTP ${status}` : "网络错误"}）`,
    ),
    retryable: status === undefined || status >= 500,
  };
}

function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

async function fetchBlobOnce(task: QueuedRequest): Promise<{
  url: string;
  mime: string;
  bytes: number;
}> {
  const resp = await api.get<Blob>(absoluteArtifactUrl(task.url), {
    responseType: "blob",
    // 有意不接受任何调用方的 signal：两个挂载点共享这条请求，任一方卸载都
    // 不能取消另一方正在用的加载
    onDownloadProgress: task.onProgress
      ? (event) => {
          if (event.total) {
            task.onProgress?.(
              Math.min(100, Math.round((event.loaded / event.total) * 100)),
            );
          }
        }
      : undefined,
  });
  const blob = resp.data;
  const header = (resp.headers as Record<string, unknown> | undefined)?.[
    "content-type"
  ];
  // 响应头为准（服务端会把非白名单类型降级成 octet-stream）；缺失时不拿
  // DTO 的 mime 补 —— 那等于绕过类型校验
  const mime = baseMime(
    typeof header === "string" && header ? header : (blob.type ?? ""),
  );
  return { url: URL.createObjectURL(blob), mime, bytes: blob.size };
}

async function fetchBlobWithRetry(task: QueuedRequest): Promise<{
  url: string;
  mime: string;
  bytes: number;
}> {
  for (let attempt = 0; ; attempt += 1) {
    try {
      return await fetchBlobOnce(task);
    } catch (err) {
      const { error, retryable } = classifyFailure(err);
      if (!retryable || attempt >= MAX_RETRIES) throw error;
      await sleep(RETRY_DELAY_MS);
    }
  }
}

async function runRequest(task: QueuedRequest, entry: BlobEntry): Promise<void> {
  try {
    const blob = await fetchBlobWithRetry(task);
    if (cache.get(task.id) !== entry) {
      // 条目已被清空（登出 / 淘汰）：产物属于上一份上下文，别留孤儿 blob；
      // 也不能把这个已被撤销的 URL 当成功结果交出去
      URL.revokeObjectURL(blob.url);
      task.reject(abortError());
      return;
    }
    entry.status = "ready";
    entry.url = blob.url;
    entry.mime = blob.mime;
    entry.bytes = blob.bytes;
    entry.error = undefined;
    task.resolve({ url: blob.url, mime: blob.mime });
  } catch (err) {
    const error =
      err instanceof ArtifactBlobError
        ? err
        : new ArtifactBlobError("network", "取流失败");
    if (cache.get(task.id) === entry) {
      entry.status = "error";
      entry.error = error.kind;
      // 可重试的失败清掉在飞 promise，下一次调用才会真的重发；
      // `unavailable` 保留已 reject 的 promise，天然做到"永不重试"
      if (error.kind !== "unavailable") entry.promise = undefined;
    }
    task.reject(error);
  } finally {
    entry.lastUsed = Date.now();
    activeRequests -= 1;
    // 检查点②：bytes 只在请求完成时增长，此刻不复查就再也不会被复查
    evict();
    pump();
  }
}

function startLoad(
  item: ArtifactItem,
  entry: BlobEntry,
  onProgress?: (percent: number) => void,
): Promise<ArtifactBlob> {
  entry.status = "loading";
  entry.promise = new Promise<ArtifactBlob>((resolve, reject) => {
    queueRequest({
      id: item.id,
      url: item.url,
      requestedByUser: entry.requestedByUser,
      onProgress,
      resolve,
      reject,
    });
  });
  return entry.promise;
}

function abortError(): Error {
  const err = new Error("请求已取消");
  err.name = "AbortError";
  return err;
}

/** 调用方的 `signal` 只决定"自己是否继续等待"，不改写共享请求的结局。 */
function awaitWithAbort<T>(promise: Promise<T>, signal?: AbortSignal): Promise<T> {
  if (!signal) return promise;
  if (signal.aborted) return Promise.reject(abortError());
  return new Promise<T>((resolve, reject) => {
    const onAbort = () => reject(abortError());
    signal.addEventListener("abort", onAbort, { once: true });
    promise.then(
      (value) => {
        signal.removeEventListener("abort", onAbort);
        resolve(value);
      },
      (err) => {
        signal.removeEventListener("abort", onAbort);
        reject(err);
      },
    );
  });
}

/**
 * 取产物的 object URL；同 id 复用同一条在飞请求与缓存。
 *
 * 失败时抛 `ArtifactBlobError`：`unavailable` 不再重试，`network` 可由调用方
 * 显式再调一次（重试按钮走同一条 URL 构造）。
 */
export async function getArtifactObjectUrl(
  item: ArtifactItem,
  opts: ArtifactBlobOptions = {},
): Promise<ArtifactBlob> {
  const { signal, requestedByUser = true, onProgress } = opts;

  let entry = cache.get(item.id);
  if (!entry) {
    entry = {
      id: item.id,
      status: "loading",
      lastUsed: Date.now(),
      requestedByUser,
    };
    cache.set(item.id, entry);
    // 检查点①：插入时
    evict();
  } else {
    if (requestedByUser) entry.requestedByUser = true;
    entry.lastUsed = Date.now();
  }

  if (entry.status === "ready" && entry.url !== undefined) {
    return { url: entry.url, mime: entry.mime ?? "" };
  }

  const pending = entry.promise ?? startLoad(item, entry, onProgress);
  return awaitWithAbort(pending, signal);
}

/** 进入视口 / 打开查看器。条目尚未创建时先记下持有数，取流时继承。 */
export function retain(id: string): void {
  refs.set(id, refCount(id) + 1);
  const entry = cache.get(id);
  if (entry) entry.lastUsed = Date.now();
}

/** 离开视口 / 关闭查看器：只让条目变"可淘汰"，不 revoke。 */
export function release(id: string): void {
  const count = refCount(id);
  if (count > 0) {
    if (count === 1) refs.delete(id);
    else refs.set(id, count - 1);
  }
  // 检查点③：刚刚变成可淘汰
  evict();
}

/** 身份变化：revoke 全部并清空。`clear()` 不调用（清空对话不删产物）。 */
export function releaseAll(): void {
  for (const entry of cache.values()) revoke(entry);
  cache.clear();
  refs.clear();
  // 排队中的请求不再发（登出后发出去只会 401），按取消收尾让调用方脱钩
  for (const task of queue.splice(0)) task.reject(abortError());
  overBoundWarned = false;
}

const EXTENSION_BY_MIME: Record<string, string> = {
  "image/jpeg": "jpg",
  "image/png": "png",
  "image/webp": "webp",
  "image/gif": "gif",
  "image/avif": "avif",
  "image/bmp": "bmp",
  "image/heic": "heic",
  "video/mp4": "mp4",
  "video/webm": "webm",
  "video/quicktime": "mov",
  "video/x-matroska": "mkv",
  "audio/mpeg": "mp3",
  "audio/mp4": "m4a",
  "audio/wav": "wav",
  "audio/ogg": "ogg",
  "application/pdf": "pdf",
  "text/plain": "txt",
};

/** blob 方案下拿不到 `Content-Disposition`，文件名只能前端拼。 */
function downloadName(item: ArtifactItem): string {
  if (/\.[a-z0-9]{1,8}$/i.test(item.title)) return item.title;
  const ext = EXTENSION_BY_MIME[baseMime(item.mime)];
  return ext ? `${item.title}.${ext}` : item.title;
}

/** 下载：与预览共用同一条取流与缓存，超上限直接拒绝（UI 已禁用按钮，这里兜底）。 */
export async function downloadArtifact(
  item: ArtifactItem,
  onProgress?: (percent: number) => void,
): Promise<void> {
  if (item.size_bytes > ARTIFACT_BLOB_MAX_BYTES) {
    throw new Error("文件超过 200 MB，暂不支持浏览器下载");
  }
  const { url } = await getArtifactObjectUrl(item, {
    requestedByUser: true,
    onProgress,
  });
  const anchor = document.createElement("a");
  anchor.href = url;
  anchor.download = downloadName(item);
  document.body.appendChild(anchor);
  anchor.click();
  anchor.remove();
  // 有意不 revoke：同一个 blob 可能正被预览使用，撤销会让画面当场裂掉
}

const PREVIEW_MIME: Record<"image" | "video" | "audio", ReadonlySet<string>> = {
  image: new Set([
    "image/jpeg",
    "image/png",
    "image/webp",
    "image/gif",
    "image/avif",
    "image/bmp",
    "image/heic",
  ]),
  video: new Set([
    "video/mp4",
    "video/webm",
    "video/quicktime",
    "video/x-matroska",
  ]),
  audio: new Set([
    "audio/mpeg",
    "audio/mp4",
    "audio/wav",
    "audio/ogg",
    "audio/webm",
  ]),
};

/**
 * 能否内联预览。服务端会把白名单外的类型降级成 `application/octet-stream`，
 * 所以必须按响应类型判断：塞进 `<img>` 是静默裂图，塞进 `<iframe>` 更糟。
 */
export function classifyPreview(
  contentType: string,
  kind: string,
): "image" | "video" | "audio" | "download-only" {
  const base = baseMime(contentType ?? "");
  if (kind === "image" && PREVIEW_MIME.image.has(base)) return "image";
  if (kind === "video" && PREVIEW_MIME.video.has(base)) return "video";
  if (kind === "audio" && PREVIEW_MIME.audio.has(base)) return "audio";
  return "download-only";
}

/**
 * 仅测试使用：让淘汰/去重/revoke 的断言看真实缓存状态，而不是数 mock 调用次数。
 *
 * `refs` 是全部持有计数之和 —— 卸载后是否归还只能从这里看出来（缓存条目本身
 * 不会因为无人持有而消失）。
 */
export function __testCacheStats(): {
  entries: number;
  bytes: number;
  urls: string[];
  refs: number;
} {
  const urls: string[] = [];
  for (const entry of cache.values()) {
    if (entry.url !== undefined) urls.push(entry.url);
  }
  let total = 0;
  for (const count of refs.values()) total += count;
  return { entries: cache.size, bytes: cacheBytes(), urls, refs: total };
}
