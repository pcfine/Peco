// useArtifactBlob：视口进出配对 retain/release，卸载必须归还持有
//
// jsdom 缺 IntersectionObserver；`URL` 只 stub `createObjectURL` / `revokeObjectURL`
// 两个静态方法（取流 URL 构造内部要用原生 `new URL`）。

import {
  afterEach,
  beforeAll,
  beforeEach,
  describe,
  expect,
  it,
  vi,
} from "vitest";
import { act, useRef } from "react";
import { createRoot, type Root } from "react-dom/client";
import api from "@/api/client";
import {
  __testCacheStats,
  getArtifactObjectUrl,
  release,
  releaseAll,
  retain,
} from "@/api/artifactBlob";
import { usePecoArtifactStore } from "@/stores/pecoArtifactStore";
import type { ArtifactItem } from "@/types/artifact";
import { useArtifactBlob } from "../useArtifactBlob";

class IntersectionObserverStub {
  static instances: IntersectionObserverStub[] = [];
  readonly root: Element | null = null;
  readonly rootMargin = "200px";
  readonly thresholds: number[] = [];
  private targets = new Set<Element>();

  constructor(private readonly callback: IntersectionObserverCallback) {
    IntersectionObserverStub.instances.push(this);
  }

  observe(target: Element) {
    this.targets.add(target);
  }

  unobserve(target: Element) {
    this.targets.delete(target);
  }

  disconnect() {
    this.targets.clear();
  }

  takeRecords(): IntersectionObserverEntry[] {
    return [];
  }

  emit(isIntersecting: boolean) {
    const entries = [...this.targets].map(
      (target) => ({ target, isIntersecting }) as IntersectionObserverEntry,
    );
    if (entries.length === 0) return;
    this.callback(entries, this as unknown as IntersectionObserver);
  }
}

let seq = 0;

function item(): ArtifactItem {
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
  };
}

function Probe({ target }: { target: ArtifactItem }) {
  const hostRef = useRef<HTMLDivElement>(null);
  const { status, url } = useArtifactBlob(target, { containerRef: hostRef });
  return (
    <div
      ref={hostRef}
      data-testid="artifact-host"
      data-status={status}
      data-url={url ?? ""}
    />
  );
}

const getSpy = vi.spyOn(api, "get");

let container: HTMLDivElement;
let root: Root;
let mounted = false;

async function flush() {
  await act(async () => {
    await new Promise((resolve) => setTimeout(resolve, 0));
    await new Promise((resolve) => setTimeout(resolve, 0));
  });
}

async function render(target: ArtifactItem) {
  mounted = true;
  await act(async () => {
    root.render(<Probe target={target} />);
  });
  await flush();
}

/** 显式触发"进入视口"，不依赖真实滚动与计时。 */
async function enterViewport() {
  await act(async () => {
    for (const observer of IntersectionObserverStub.instances) {
      observer.emit(true);
    }
  });
  await flush();
}

async function unmount() {
  await act(async () => {
    root.unmount();
  });
  mounted = false;
  await flush();
}

beforeAll(() => {
  URL.createObjectURL = vi.fn(() => `blob:mock-${(seq += 1)}`);
  URL.revokeObjectURL = vi.fn();
});

beforeEach(() => {
  (
    globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT: boolean }
  ).IS_REACT_ACT_ENVIRONMENT = true;
  globalThis.IntersectionObserver =
    IntersectionObserverStub as unknown as typeof IntersectionObserver;
  IntersectionObserverStub.instances = [];

  releaseAll();
  usePecoArtifactStore.getState().__resetForTests();
  vi.mocked(URL.createObjectURL).mockClear();
  vi.mocked(URL.revokeObjectURL).mockClear();
  getSpy.mockReset();
  getSpy.mockResolvedValue({
    data: { size: 1024, type: "image/png" } as unknown as Blob,
    headers: { "content-type": "image/png" },
  } as never);

  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
});

afterEach(async () => {
  if (mounted) await unmount();
  container.remove();
  releaseAll();
  usePecoArtifactStore.getState().__resetForTests();
  IntersectionObserverStub.instances = [];
});

describe("useArtifactBlob 的持有生命周期", () => {
  it("进入视口持有、卸载归还：归还后条目可被淘汰", async () => {
    const target = item();
    await render(target);
    await enterViewport();

    const blobUrl = __testCacheStats().urls[0];
    expect(blobUrl).toBeDefined();
    expect(__testCacheStats().refs).toBe(1);

    await unmount();

    expect(__testCacheStats().refs).toBe(0);
    // 变异点：cleanup 不调用 release → refs 仍为 1，这条永远不是淘汰候选
    for (let i = 0; i < 40; i += 1) await getArtifactObjectUrl(item());
    expect(URL.revokeObjectURL).toHaveBeenCalledWith(blobUrl);
  });

  it("未进入视口就卸载：不抛错、持有计数不为负", async () => {
    const target = item();
    await render(target);
    // 从未 enter：既没 retain，也不该取流
    expect(getSpy).not.toHaveBeenCalled();

    await unmount();

    // 判别力来自"从未持有/从未取流"，不来自计数是否被减：release() 内部把计数钳在
    // 0 以上，cleanup 里多做一次无条件 release 也观察不到差异
    expect(__testCacheStats().refs).toBe(0);
    expect(__testCacheStats().entries).toBe(0);
    retain(target.id);
    expect(__testCacheStats().refs).toBe(1);
    release(target.id);
    expect(__testCacheStats().refs).toBe(0);
  });

  it("卸载不 revoke：再次取流仍复用同一 URL", async () => {
    const target = item();
    await render(target);
    await enterViewport();
    const blobUrl = __testCacheStats().urls[0];
    expect(blobUrl).toBeDefined();

    await unmount();

    // 变异点：卸载即 revoke → 下面 createObjectURL 会被再调一次、URL 也换了
    expect(URL.revokeObjectURL).not.toHaveBeenCalled();
    retain(target.id);
    const again = await getArtifactObjectUrl(target);
    expect(again.url).toBe(blobUrl);
    expect(URL.createObjectURL).toHaveBeenCalledTimes(1);
    release(target.id);
  });
});
