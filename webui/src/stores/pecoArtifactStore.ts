// 会话产物列表：跨路由存活的模块级 store
//
// 两个数据源各自成数组：`ingested`（本窗口 SSE 增量）与 `pages`（服务端分页）。
// 合并成一个数组就做不到"本次响应没提到、但刚经 SSE 进来的项要留在最前"——
// 条目上没有来源标记，二元 upsert 无法区分"新项"与"翻页拉回的旧项"。

import { create } from "zustand";
import { listPecoArtifacts } from "@/api/peco";
import { releaseAll } from "@/api/artifactBlob";
import type { ArtifactItem, ArtifactPage } from "@/types/artifact";

/**
 * 渲染列表：`ingested` 在前、`pages` 在后，同 id 取 `ingested` 的版本。
 *
 * 纯函数（不读 store），入参不被修改。
 */
export function mergeArtifacts(
  ingested: ArtifactItem[],
  pages: ArtifactItem[],
): ArtifactItem[] {
  const ingestedIds = new Set(ingested.map((a) => a.id));
  return [...ingested, ...pages.filter((a) => !ingestedIds.has(a.id))];
}

/**
 * 派生列表不是状态：三份数组各维护一遍必然出现"更新漏一处"。
 *
 * 每次调用都新建数组，别直接喂给 `usePecoArtifactStore(selector)`（引用每帧都变），
 * 组件内应包一层 `useMemo`。
 */
export function selectArtifactItems(state: PecoArtifactState): ArtifactItem[] {
  return mergeArtifacts(state.ingested, state.pages);
}

export interface PecoArtifactState {
  /** 本窗口 SSE 增量引入、尚未被服务端响应认领的项，最新的在前。 */
  ingested: ArtifactItem[];
  /** 服务端分页结果（DESC）。`load`/`refreshHead` 替换头部页，`loadMore` 追加。 */
  pages: ArtifactItem[];
  total: number;
  loading: boolean;
  loadingMore: boolean;
  error: string | null;
  loaded: boolean;
  /** 已请求到的最远偏移，单调不减；"无更多"判据是 `nextOffset >= total`。 */
  nextOffset: number;

  load: () => Promise<void>;
  refreshHead: () => Promise<void>;
  loadMore: () => Promise<void>;
  ingest: (next: ArtifactItem[]) => void;
  markUnavailable: (id: string) => void;
  reset: () => void;
  __resetForTests: () => void;
}

function emptyState() {
  return {
    ingested: [] as ArtifactItem[],
    pages: [] as ArtifactItem[],
    total: 0,
    loading: false,
    loadingMore: false,
    error: null as string | null,
    loaded: false,
    nextOffset: 0,
  };
}

/**
 * 身份代际：`reset()` 递增。登出与在飞请求天然并发，响应落地前必须比对代际，
 * 否则旧账号的一页会写进新账号的列表，`loaded = true` 还会让新账号的 `load()`
 * 变成 no-op。
 */
let generation = 0;

/**
 * 首屏与获焦追平都会替换头部页，重叠请求没有意义。
 *
 * 记下发起时的代际而不是布尔值：旧代际的收尾不得清掉新代际刚立起的标记，
 * 否则同一时刻会有两条头部请求在飞。
 */
let headInFlightGen: number | null = null;

function errorMessage(err: unknown, fallback: string): string {
  return err instanceof Error && err.message ? err.message : fallback;
}

type Setter = (partial: Partial<PecoArtifactState>) => void;

/**
 * 头部页落地：新页顶掉同 id 的旧项，但**保留**更早的已加载页（响应只覆盖
 * offset=0 那一段，整表替换会把翻页拉回来的旧产物全丢掉），并把已被服务端
 * 认领的 `ingested` 项移除（认领后排序权交回服务端）。游标取两边最大值，
 * 刷新不得让游标倒退。
 */
function applyHead(page: ArtifactPage, prev: PecoArtifactState) {
  const pageIds = new Set(page.artifacts.map((a) => a.id));
  return {
    pages: [...page.artifacts, ...prev.pages.filter((a) => !pageIds.has(a.id))],
    ingested: prev.ingested.filter((a) => !pageIds.has(a.id)),
    total: page.total,
    nextOffset: Math.max(prev.nextOffset, page.offset + page.artifacts.length),
    loaded: true,
  };
}

/** `load`（首载）与 `refreshHead`（获焦追平）共用一次头部请求。 */
async function loadHead(
  set: Setter,
  get: () => PecoArtifactState,
  fallbackError: string,
): Promise<void> {
  if (headInFlightGen !== null) return;
  const gen = generation;
  headInFlightGen = gen;
  set({ loading: true, error: null });
  try {
    const page = await listPecoArtifacts();
    if (gen !== generation) return;
    set(applyHead(page, get()));
  } catch (err) {
    if (gen !== generation) return;
    set({ error: errorMessage(err, fallbackError) });
  } finally {
    if (headInFlightGen === gen) headInFlightGen = null;
    if (gen === generation) set({ loading: false });
  }
}

export const usePecoArtifactStore = create<PecoArtifactState>()((set, get) => ({
  ...emptyState(),

  load: async () => {
    // 切回页面不得重复拉第一页；追平只由获焦 refreshHead 负责
    if (get().loaded) return;
    await loadHead(set, get, "加载产物失败");
  },

  refreshHead: async () => {
    await loadHead(set, get, "刷新产物失败");
  },

  loadMore: async () => {
    const { loadingMore, nextOffset, total } = get();
    // 判据用游标而非本地条目数：SSE 前插项让条目数偏大，会让"加载更多"提前消失
    if (loadingMore || nextOffset >= total) return;
    const gen = generation;
    set({ loadingMore: true, error: null });
    try {
      const page = await listPecoArtifacts({ offset: nextOffset });
      if (gen !== generation) return;
      set((s) => {
        const seen = new Set([
          ...s.ingested.map((a) => a.id),
          ...s.pages.map((a) => a.id),
        ]);
        return {
          pages: [
            ...s.pages,
            ...page.artifacts.filter((a) => !seen.has(a.id)),
          ],
          // 本地可能刚吸收 SSE 增量，用滞后的 total 覆盖会让"无更多"提前成立
          total: Math.max(s.total, page.total),
          nextOffset: Math.max(
            s.nextOffset,
            page.offset + page.artifacts.length,
          ),
        };
      });
    } catch (err) {
      if (gen !== generation) return;
      set({ error: errorMessage(err, "加载更多失败") });
    } finally {
      if (gen === generation) set({ loadingMore: false });
    }
  },

  ingest: (next) => {
    if (next.length === 0) return;
    set((s) => {
      const known = new Set([
        ...s.ingested.map((a) => a.id),
        ...s.pages.map((a) => a.id),
      ]);
      const added = new Set<string>();
      // 同一批声明逐条落库（rowid 递增），服务端 DESC 会倒着回；整批反转后
      // 才与首屏顺序一致
      const prefix: ArtifactItem[] = [];
      for (const item of [...next].reverse()) {
        const at = prefix.findIndex((a) => a.id === item.id);
        if (at >= 0) prefix[at] = item;
        else prefix.push(item);
        if (!known.has(item.id)) added.add(item.id);
      }
      const prefixIds = new Set(prefix.map((a) => a.id));
      return {
        ingested: [
          ...prefix,
          ...s.ingested.filter((a) => !prefixIds.has(a.id)),
        ],
        total: s.total + added.size,
      };
    });
  },

  markUnavailable: (id) => {
    const patch = (list: ArtifactItem[]) =>
      list.map((a) => (a.id === id ? { ...a, available: false } : a));
    set((s) => ({ ingested: patch(s.ingested), pages: patch(s.pages) }));
  },

  reset: () => {
    // 身份变化：列表与已取到的 blob 一起作废，否则换账号会看到上一账号的产物；
    // 代际递增让在飞响应落地时被丢弃，而不是写进新身份的列表
    generation += 1;
    headInFlightGen = null;
    releaseAll();
    set(emptyState());
  },

  __resetForTests: () => {
    generation += 1;
    headInFlightGen = null;
    set(emptyState());
  },
}));
