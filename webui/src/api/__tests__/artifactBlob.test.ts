// 产物 blob 取流：同 id 去重 / 调用方 abort / 重试策略 / 队列放弃 / LRU 淘汰 / 下载上限
//
// jsdom 只缺 object URL：**只 stub `URL.createObjectURL` 与 `URL.revokeObjectURL`
// 两个静态方法**，保留原生构造器 —— 取流 URL 构造内部用 `new URL(path, origin)`。

import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import api from "../client";
import { absoluteArtifactUrl } from "../artifactUrl";
import {
  ARTIFACT_BLOB_MAX_BYTES,
  ArtifactBlobError,
  __testCacheStats,
  classifyPreview,
  downloadArtifact,
  getArtifactObjectUrl,
  release,
  releaseAll,
  retain,
} from "../artifactBlob";
import type { ArtifactItem } from "@/types/artifact";

let seq = 0;

function item(overrides: Partial<ArtifactItem> = {}): ArtifactItem {
  seq += 1;
  return {
    id: `a${seq}`,
    conversation_id: "c1",
    kind: "image",
    mime: "image/png",
    title: `图${seq}.png`,
    size_bytes: 1024,
    url: `/api/artifacts/a${seq}/content`,
    download_url: `/api/artifacts/a${seq}/download`,
    created_at: "2026-10-20 12:00:00",
    available: true,
    ...overrides,
  };
}

/** 只需 `size` / `type`：object URL 已被 stub，不会真的读数据。 */
function fakeBlob(bytes: number, type = "image/png"): Blob {
  return { size: bytes, type } as unknown as Blob;
}

function ok(bytes: number, type = "image/png") {
  return {
    data: fakeBlob(bytes, type),
    headers: { "content-type": type },
  } as never;
}

function pending(): Promise<never> {
  return new Promise<never>(() => {});
}

const getSpy = vi.spyOn(api, "get");

beforeAll(() => {
  URL.createObjectURL = vi.fn(() => `blob:mock-${(seq += 1)}`);
  URL.revokeObjectURL = vi.fn();
});

beforeEach(() => {
  releaseAll();
  getSpy.mockReset();
  vi.mocked(URL.createObjectURL).mockClear();
  vi.mocked(URL.revokeObjectURL).mockClear();
  // 超界无候选时实现会打一条 warn —— 测试里静音，避免污染输出
  vi.spyOn(console, "warn").mockImplementation(() => {});
});

afterEach(() => {
  vi.useRealTimers();
});

describe("取流 URL", () => {
  it("转成绝对地址，只带一个 /api", () => {
    // 变异点：把 item.url 直接交给 api 实例 → baseURL 叠加 → 断言出现 /api/api
    expect(absoluteArtifactUrl("/api/artifacts/x/content")).toBe(
      "http://localhost:3000/api/artifacts/x/content",
    );
  });

  it("守护：实例 baseURL 为 /api 且未关闭绝对 URL 支持", () => {
    // 一旦有人给实例设 allowAbsoluteUrls:false，绝对地址会被重新拼上 baseURL
    expect(api.defaults.baseURL).toBe("/api");
    expect(api.defaults.allowAbsoluteUrls).not.toBe(false);
  });
});

describe("getArtifactObjectUrl", () => {
  it("同 id 两处挂载只取一次，URL 不含 /api/api 且要求 blob 响应", async () => {
    const a = item();
    getSpy.mockResolvedValue(ok(1024));

    const [first, second] = await Promise.all([
      getArtifactObjectUrl(a),
      getArtifactObjectUrl(a),
    ]);

    expect(getSpy).toHaveBeenCalledTimes(1);
    expect(URL.createObjectURL).toHaveBeenCalledTimes(1);
    expect(second).toEqual(first);
    expect(__testCacheStats().entries).toBe(1);

    const [url, config] = getSpy.mock.calls[0];
    expect(url).toBe(`http://localhost:3000/api/artifacts/${a.id}/content`);
    expect(String(url)).not.toContain("/api/api");
    expect(config?.responseType).toBe("blob");
  });

  it("调用方 abort 只结束自己的等待，共享请求与另一方不受影响", async () => {
    const a = item();
    let resolveGet!: (value: never) => void;
    getSpy.mockReturnValue(
      new Promise((resolve) => {
        resolveGet = resolve as (value: never) => void;
      }) as never,
    );
    const controller = new AbortController();

    const abandoned = getArtifactObjectUrl(a, {
      signal: controller.signal,
      requestedByUser: true,
    });
    const survivor = getArtifactObjectUrl(a);

    controller.abort();
    await expect(abandoned).rejects.toMatchObject({ name: "AbortError" });

    resolveGet(ok(1024));
    await expect(survivor).resolves.toMatchObject({ mime: "image/png" });

    // 变异点：把调用方 signal 透传给 axios → 底层请求被取消 → survivor 失败
    expect(getSpy).toHaveBeenCalledTimes(1);
    expect(getSpy.mock.calls[0][1]).not.toHaveProperty("signal");
    expect(__testCacheStats().entries).toBe(1);
  });

  it("404 标记 unavailable 且永不重试", async () => {
    const a = item();
    getSpy.mockRejectedValue({ response: { status: 404 } });

    await expect(getArtifactObjectUrl(a)).rejects.toMatchObject({
      kind: "unavailable",
    });
    expect(getSpy).toHaveBeenCalledTimes(1);

    // 重试按钮走同一条调用：已知取不到就不再发请求
    await expect(getArtifactObjectUrl(a)).rejects.toBeInstanceOf(
      ArtifactBlobError,
    );
    expect(getSpy).toHaveBeenCalledTimes(1);
    // 变异点：把 404 纳入重试 → 次数变 2
  });

  it("5xx 自动重试一次后成功", async () => {
    const a = item();
    getSpy
      .mockRejectedValueOnce({ response: { status: 500 } })
      .mockResolvedValueOnce(ok(1024));

    await expect(getArtifactObjectUrl(a)).resolves.toMatchObject({
      mime: "image/png",
    });
    // 变异点：去掉重试 → 次数 1 且 reject
    expect(getSpy).toHaveBeenCalledTimes(2);
    expect(__testCacheStats().entries).toBe(1);
  }, 10_000);

  it("排队期间被放弃的预取在出队前丢弃，不发请求", async () => {
    const held: ArtifactItem[] = [item(), item(), item()];
    const resolvers: ((value: never) => void)[] = [];
    getSpy.mockImplementation(
      () =>
        new Promise((resolve) => {
          resolvers.push(resolve as (value: never) => void);
        }) as never,
    );
    // 占满 3 个并发槽位
    for (const h of held) {
      void getArtifactObjectUrl(h, { requestedByUser: true });
    }
    expect(getSpy).toHaveBeenCalledTimes(3);

    const prefetched = Array.from({ length: 20 }, () => item());
    const abandoned = prefetched.map((p) => {
      retain(p.id);
      // 处理器必须当场挂上：丢弃发生在 pump() 里的同步 reject
      return getArtifactObjectUrl(p, { requestedByUser: false }).then(
        () => null,
        (err: Error) => err,
      );
    });
    expect(getSpy).toHaveBeenCalledTimes(3);

    // 快速滚过：20 条预取全部失去持有者
    for (const p of prefetched) release(p.id);

    for (const resolve of resolvers) resolve(ok(1024));
    await vi.waitFor(() =>
      expect(__testCacheStats().entries).toBe(held.length),
    );

    // 变异点：出队不复核 refs → 20 份完整文件被下载，次数变 23
    expect(getSpy).toHaveBeenCalledTimes(3);
    // 被丢弃的任务按取消收尾，调用方不会等一个永不落定的 promise
    for (const outcome of abandoned) {
      expect((await outcome)?.name).toBe("AbortError");
    }
  });

  it("超界后循环淘汰直到回到界内（不是只减一条）", async () => {
    getSpy.mockResolvedValue(ok(1.5 * 1024 * 1024));
    const small = Array.from({ length: 40 }, () => item());
    for (const s of small) await getArtifactObjectUrl(s);

    const before = __testCacheStats();
    expect(before.entries).toBe(40);
    expect(before.bytes).toBe(40 * 1.5 * 1024 * 1024);

    // 插入时先挤掉一条，落盘后再按字节上界继续挤
    getSpy.mockResolvedValueOnce(ok(10 * 1024 * 1024));
    await getArtifactObjectUrl(item({ size_bytes: 10 * 1024 * 1024 }));

    const after = __testCacheStats();
    // 变异点：每次只淘汰 1 条 → bytes 停在 64MB 之上 → 断言失败
    expect(after.bytes).toBeLessThanOrEqual(64 * 1024 * 1024);
    expect(URL.revokeObjectURL).toHaveBeenCalledTimes(4);
    expect(after.entries).toBe(37);
  });

  it("按 lastUsed 淘汰最久未使用的条目", async () => {
    vi.useFakeTimers();
    getSpy.mockResolvedValue(ok(1024));
    const small = Array.from({ length: 40 }, () => item());
    for (const s of small) {
      await getArtifactObjectUrl(s);
      vi.advanceTimersByTime(10);
    }
    const oldestUrl = __testCacheStats().urls[0];

    await getArtifactObjectUrl(item());

    // 变异点：按插入顺序/随机挑 → 换掉的不一定是最早那条
    expect(URL.revokeObjectURL).toHaveBeenCalledWith(oldestUrl);
    expect(__testCacheStats().entries).toBe(40);
  });

  it("持有中的条目即使超界也不 revoke，释放后立刻可淘汰", async () => {
    getSpy.mockResolvedValue(ok(1024));
    const items = Array.from({ length: 41 }, () => item());
    for (const i of items) {
      retain(i.id);
      await getArtifactObjectUrl(i);
    }

    // 全在视口里：宁可暂时超界，也不能 revoke 正在渲染的 URL
    expect(__testCacheStats().entries).toBe(41);
    expect(URL.revokeObjectURL).not.toHaveBeenCalled();

    release(items[0].id);
    expect(URL.revokeObjectURL).toHaveBeenCalledTimes(1);
    expect(__testCacheStats().entries).toBe(40);
    // 变异点：不看 refs 直接淘汰 → 上面那条 not.toHaveBeenCalled 就失败
  });

  it("loading 且无人持有的条目：超界时不淘汰，完成后仍超界则当场淘汰", async () => {
    const loading = item();
    let resolveLoading!: (value: never) => void;
    getSpy.mockImplementationOnce(
      () =>
        new Promise((resolve) => {
          resolveLoading = resolve as (value: never) => void;
        }) as never,
    );
    getSpy.mockResolvedValue(ok(1024));

    // 有意不 retain：refs === 0，保证"不淘汰"只可能来自 loading 判据
    const loadingPromise = getArtifactObjectUrl(loading, {
      requestedByUser: true,
    });
    // loading 是最早的一条：允许淘汰 loading 的实现会先挑中它（无 URL，revoke 静默）
    for (let i = 0; i < 40; i += 1) {
      await getArtifactObjectUrl(item());
    }
    expect(__testCacheStats().entries).toBe(40);
    expect(URL.revokeObjectURL).toHaveBeenCalledTimes(1);

    resolveLoading(ok(70 * 1024 * 1024));
    const resolved = await loadingPromise;

    // 变异点：允许淘汰 loading → 条目在超界时被删 → 完成时当孤儿 revoke 并 reject
    expect(resolved.url).toBeDefined();
    // 变异点：完成时不复查（检查点②）→ 超界条目留在缓存里，urls 仍含它
    expect(__testCacheStats().urls).not.toContain(resolved.url);
    expect(URL.revokeObjectURL).toHaveBeenCalledWith(resolved.url);
  });
});

describe("classifyPreview", () => {
  it("白名单内按 kind 内联，白名单外一律只能下载", () => {
    expect(classifyPreview("image/png", "image")).toBe("image");
    expect(classifyPreview("video/mp4; codecs=avc1", "video")).toBe("video");
    expect(classifyPreview("audio/mpeg", "audio")).toBe("audio");
    // 服务端会把非白名单降级成 octet-stream；svg/html 是同源存储型 XSS 入口
    expect(classifyPreview("application/octet-stream", "image")).toBe(
      "download-only",
    );
    expect(classifyPreview("image/svg+xml", "image")).toBe("download-only");
    expect(classifyPreview("text/html", "document")).toBe("download-only");
    expect(classifyPreview("", "image")).toBe("download-only");
  });
});

describe("downloadArtifact", () => {
  it("超过 200MiB 直接拒绝，不取流也不建下载链接", async () => {
    const clickSpy = vi
      .spyOn(HTMLAnchorElement.prototype, "click")
      .mockImplementation(() => {});
    const huge = item({ size_bytes: ARTIFACT_BLOB_MAX_BYTES + 1 });

    await expect(downloadArtifact(huge)).rejects.toThrow("200 MB");
    // 变异点：去掉上限 → 发起整份文件请求
    expect(getSpy).not.toHaveBeenCalled();
    expect(clickSpy).not.toHaveBeenCalled();
    expect(document.querySelector("a[download]")).toBeNull();
  });

  it("下载复用取流缓存，文件名缺扩展名时按 mime 补", async () => {
    const clickSpy = vi
      .spyOn(HTMLAnchorElement.prototype, "click")
      .mockImplementation(() => {});
    getSpy.mockResolvedValue(ok(1024));

    const a = item({ title: "红狐狸", mime: "image/png" });
    await downloadArtifact(a);

    const anchor = clickSpy.mock.instances[0] as unknown as HTMLAnchorElement;
    expect(clickSpy).toHaveBeenCalledTimes(1);
    expect(anchor.download).toBe("红狐狸.png");
    expect(anchor.href).toMatch(/^blob:/);
    // 下载不额外 revoke：同一个 blob 可能正被预览使用
    expect(URL.revokeObjectURL).not.toHaveBeenCalled();
    // 再次预览同一项不重新下载
    await getArtifactObjectUrl(a);
    expect(getSpy).toHaveBeenCalledTimes(1);
  });
});
