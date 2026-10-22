// 产物 store：双数组合并规则 / 认领 / 游标 / total 自增 / 重置
//
// 断言都看真实 state（ingested / pages / nextOffset / total 的组合结果），
// 不看"调用了几次 set"。

import { beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("@/api/peco", () => ({ listPecoArtifacts: vi.fn() }));
vi.mock("@/api/artifactBlob", () => ({ releaseAll: vi.fn() }));

import { listPecoArtifacts } from "@/api/peco";
import { releaseAll } from "@/api/artifactBlob";
import {
  mergeArtifacts,
  selectArtifactItems,
  usePecoArtifactStore,
} from "../pecoArtifactStore";
import type { ArtifactItem, ArtifactPage } from "@/types/artifact";

const list = vi.mocked(listPecoArtifacts);

function item(id: string, overrides: Partial<ArtifactItem> = {}): ArtifactItem {
  return {
    id,
    conversation_id: "c1",
    kind: "image",
    mime: "image/png",
    title: `${id}.png`,
    size_bytes: 1024,
    url: `/api/artifacts/${id}/content`,
    download_url: `/api/artifacts/${id}/download`,
    created_at: "2026-10-20 12:00:00",
    available: true,
    ...overrides,
  };
}

function page(
  artifacts: ArtifactItem[],
  offset = 0,
  total = artifacts.length,
): ArtifactPage {
  return { artifacts, total, limit: artifacts.length, offset };
}

const state = () => usePecoArtifactStore.getState();
const items = () => selectArtifactItems(state());

beforeEach(() => {
  vi.clearAllMocks();
  state().__resetForTests();
});

describe("mergeArtifacts", () => {
  it("ingested 整体在前，同 id 只留 ingested 的版本", () => {
    const merged = mergeArtifacts(
      [item("x", { available: false })],
      [item("a"), item("x", { available: true })],
    );

    expect(merged.map((a) => a.id)).toEqual(["x", "a"]);
    // 同 id 取 ingested 的字段（这里的 available 差异是唯一判据）
    expect(merged[0].available).toBe(false);
  });

  it("两入参皆空返回空数组，且不修改入参", () => {
    expect(mergeArtifacts([], [])).toEqual([]);

    const ingested = [item("x")];
    const pages = [item("a"), item("x")];
    const ingestedSnapshot = [...ingested];
    const pagesSnapshot = [...pages];
    mergeArtifacts(ingested, pages);
    expect(ingested).toEqual(ingestedSnapshot);
    expect(pages).toEqual(pagesSnapshot);
  });
});

describe("ingest", () => {
  it("同一批整批反转后前插（服务端对同批声明返回倒序）", () => {
    state().ingest([item("a"), item("b")]);

    // 变异点：不反转 → 变成 [a, b]
    expect(items().map((a) => a.id)).toEqual(["b", "a"]);
    expect(state().ingested.map((a) => a.id)).toEqual(["b", "a"]);
  });

  it("只对新 id 增加 total", () => {
    state().ingest([item("a"), item("b")]);
    expect(state().total).toBe(2);

    // 其中 a 已存在 → 只 +1
    state().ingest([item("a"), item("c")]);
    expect(state().total).toBe(3);
    // 变异点：每批 +len → 这里会变成 2+2=4
    expect(items().map((a) => a.id)).toEqual(["c", "a", "b"]);
  });

  it("已在 pages 里的 id 不重复计 total、列表里也只出现一次", async () => {
    list.mockResolvedValue(page([item("a")], 0, 1));
    await state().load();

    state().ingest([item("a")]);

    expect(state().total).toBe(1);
    expect(items().map((a) => a.id)).toEqual(["a"]);
  });
});

describe("load / refreshHead", () => {
  it("load 落地后保留在途增量、置 loaded", async () => {
    let resolvePage!: (value: ArtifactPage) => void;
    list.mockReturnValue(
      new Promise<ArtifactPage>((resolve) => {
        resolvePage = resolve;
      }),
    );

    const loading = state().load();
    state().ingest([item("x")]);
    resolvePage(page([item("a")], 0, 1));
    await loading;

    // 响应没提到 x → 它是本窗口新产物，必须留在最前
    expect(items().map((a) => a.id)).toEqual(["x", "a"]);
    expect(state().ingested.map((a) => a.id)).toEqual(["x"]);
    expect(state().loaded).toBe(true);
  });

  it("load 在已 loaded 时短路（切回页面不重复拉第一页）", async () => {
    list.mockResolvedValue(page([item("a")])); 
    await state().load();
    await state().load();

    expect(list).toHaveBeenCalledTimes(1);
  });

  it("refreshHead 替换头部页但不丢更早的页", async () => {
    list.mockResolvedValueOnce(page([item("a")], 0, 2));
    await state().load();
    list.mockResolvedValueOnce(page([item("b")], 1, 2));
    await state().loadMore();
    expect(items().map((a) => a.id)).toEqual(["a", "b"]);

    list.mockResolvedValueOnce(page([item("c")], 0, 3));
    await state().refreshHead();

    // 新的头部项顶到最前，翻页已加载的 [a, b] 不丢
    expect(items().map((a) => a.id)).toEqual(["c", "a", "b"]);
  });

  it("refreshHead 认领 ingested：被响应包含的项交回 pages 排序", async () => {
    state().ingest([item("x")]);
    list.mockResolvedValueOnce(page([item("x"), item("a")], 0, 2));

    await state().refreshHead();

    expect(state().ingested).toEqual([]);
    expect(state().pages.map((a) => a.id)).toEqual(["x", "a"]);
    // 变异点：不认领 → 列表里 x 出现两次
    expect(items().map((a) => a.id)).toEqual(["x", "a"]);
  });
});

describe("loadMore", () => {
  it("按 nextOffset 翻页并去重追加，total 取较大值", async () => {
    list.mockResolvedValueOnce(page([item("a"), item("b")], 0, 3));
    await state().load();

    list.mockResolvedValueOnce(page([item("b"), item("c")], 2, 3));
    await state().loadMore();

    expect(list).toHaveBeenLastCalledWith({ offset: 2 });
    expect(items().map((a) => a.id)).toEqual(["a", "b", "c"]);
    expect(state().nextOffset).toBe(4);
  });

  it("无更多（nextOffset >= total）时不发请求", async () => {
    list.mockResolvedValueOnce(page([item("a")], 0, 1));
    await state().load();

    await state().loadMore();

    expect(list).toHaveBeenCalledTimes(1);
  });
});

describe("游标", () => {
  it("单调不减：刷新不得让已加载的页被重拉", async () => {
    const many = (prefix: string, n: number) =>
      Array.from({ length: n }, (_, i) => item(`${prefix}${i}`));

    list.mockResolvedValueOnce(page(many("a", 200), 0, 500));
    await state().load();
    expect(state().nextOffset).toBe(200);

    // offset=0 的一页回来不能让游标倒退
    list.mockResolvedValueOnce(page(many("b", 200), 0, 500));
    await state().refreshHead();
    expect(state().nextOffset).toBe(200);

    list.mockResolvedValueOnce(page(many("c", 100), 200, 500));
    await state().loadMore();
    expect(state().nextOffset).toBe(300);

    // 已经拉到 400 时刷新，游标仍是 400
    state().__resetForTests();
    list.mockResolvedValueOnce(page(many("d", 400), 0, 500));
    await state().load();
    const before = state().nextOffset;
    expect(before).toBe(400);
    list.mockResolvedValueOnce(page(many("e", 200), 0, 500));
    await state().refreshHead();
    expect(state().nextOffset).toBe(400);
  });
});

describe("身份代际", () => {
  it("登出后在飞响应落地不写回：列表仍空、loaded 仍 false", async () => {
    let resolvePage!: (value: ArtifactPage) => void;
    list.mockReturnValue(
      new Promise<ArtifactPage>((resolve) => {
        resolvePage = resolve;
      }),
    );

    const loading = state().load();
    // 登出：身份已换，这一页属于上一账号
    state().reset();
    resolvePage(page([item("old")], 0, 1));
    await loading;

    // 变异点：删掉 gen 校验 → 旧页写进 pages、loaded 变 true，新账号 load() 变 no-op
    expect(state().pages).toEqual([]);
    expect(state().ingested).toEqual([]);
    expect(state().total).toBe(0);
    expect(state().loaded).toBe(false);
    expect(state().loading).toBe(false);
  });

  it("登出后新身份的 load() 仍能真正发请求并落地", async () => {
    let resolveOld!: (value: ArtifactPage) => void;
    list.mockReturnValueOnce(
      new Promise<ArtifactPage>((resolve) => {
        resolveOld = resolve;
      }),
    );
    const old = state().load();
    state().reset();

    list.mockResolvedValueOnce(page([item("new")], 0, 1));
    await state().load();

    resolveOld(page([item("old")], 0, 1));
    await old;

    expect(state().pages.map((a) => a.id)).toEqual(["new"]);
    expect(state().loaded).toBe(true);
  });

  it("加载更多在飞时登出，落地结果不追加", async () => {
    list.mockResolvedValueOnce(page([item("a")], 0, 3));
    await state().load();

    let resolveMore!: (value: ArtifactPage) => void;
    list.mockReturnValueOnce(
      new Promise<ArtifactPage>((resolve) => {
        resolveMore = resolve;
      }),
    );
    const more = state().loadMore();
    state().reset();
    resolveMore(page([item("b"), item("c")], 1, 3));
    await more;

    // 变异点：loadMore 不校验代际 → 旧账号的下一页被追加进新身份的列表
    expect(state().pages).toEqual([]);
    expect(state().nextOffset).toBe(0);
    expect(state().loadingMore).toBe(false);
  });
});

describe("markUnavailable / reset", () => {
  it("两个数组里同 id 的项一起标成不可用", async () => {
    list.mockResolvedValueOnce(page([item("a"), item("b")], 0, 2));
    await state().load();
    state().ingest([item("x"), item("a")]);

    state().markUnavailable("a");

    const a = items().find((it) => it.id === "a");
    expect(a?.available).toBe(false);
    expect(items().filter((it) => it.id === "b")[0].available).toBe(true);
    expect(state().ingested.some((it) => it.id === "x")).toBe(true);
  });

  it("reset 清空状态并释放全部 blob", async () => {
    list.mockResolvedValueOnce(page([item("a")], 0, 1));
    await state().load();
    state().ingest([item("x")]);

    state().reset();

    const s = state();
    expect(s.ingested).toEqual([]);
    expect(s.pages).toEqual([]);
    expect(s.total).toBe(0);
    expect(s.nextOffset).toBe(0);
    expect(s.loaded).toBe(false);
    // 变异点：不调 releaseAll → 换账号后上一账号的 blob 仍存活
    expect(releaseAll).toHaveBeenCalledTimes(1);
  });

  it("markUnavailable 不会凭空造出条目", () => {
    state().markUnavailable("nope");
    expect(items()).toEqual([]);
  });
});
