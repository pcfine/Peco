// 回收站整条链路的落点：按钮 → 确认弹窗 → 真实 HTTP 请求 → 缓存失效。
// 只换 axios 的 adapter，api/queries 两层照原样跑 —— 要断言的正是「请求长什么样」
// （有没有 purge、走没走 restore），把 api 模块 mock 掉就看不见了。

import { AxiosError, type InternalAxiosRequestConfig } from "axios";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { MemoryRouter } from "react-router-dom";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { api } from "@/lib/api";
import { SpaceListPage } from "@/pages/SpaceListPage";
import { useSpaceStore } from "@/stores/spaceStore";
import { useOpenSpacesStore } from "@/stores/openSpacesStore";
import type { Space, TrashSpace } from "@/types/daemon";

const mocks = vi.hoisted(() => ({
  success: vi.fn(),
  error: vi.fn(),
}));

vi.mock("sonner", () => ({
  toast: { success: mocks.success, error: mocks.error },
}));

function space(overrides: Partial<Space> = {}): Space {
  return {
    ws_id: "ws_a",
    work_dir: "/tmp/spaces/a/work",
    team_id: "research",
    label: "空间 A",
    name: null,
    exit_policy: "inherit",
    effective_exit_policy: "inherit",
    status: "closed",
    closed_at: "2026-11-05 10:00:00",
    last_opened_at: "2026-11-05 09:00:00",
    reserved: false,
    inflight_dispatch: 0,
    inflight_foreign: 0,
    marker: null,
    ...overrides,
  };
}

function trashRow(overrides: Partial<TrashSpace> = {}): TrashSpace {
  return {
    ws_id: "ws_a",
    name: "空间 A",
    team_id: "research",
    work_dir: "/tmp/spaces/a/work",
    trashed_at: "2026-11-05 12:00:00",
    ...overrides,
  };
}

interface FakeBackend {
  spaces: Space[];
  trash: TrashSpace[];
  /** 非空时 `/spaces/trash` 回 500，正文就是这句话 —— 前端必须原样摆出来。 */
  trashError: string | null;
  requests: { method: string; uri: string }[];
}

let backend: FakeBackend;

function reply(config: InternalAxiosRequestConfig, data: unknown) {
  return { data, status: 200, statusText: "ok", headers: {}, config };
}

/** 非 2xx 必须由 adapter 自己 reject：axios 只把 adapter 的 resolve 当成功。 */
function fail(
  config: InternalAxiosRequestConfig,
  data: unknown,
  status: number,
) {
  return Promise.reject(
    new AxiosError("request failed", "ERR_BAD_RESPONSE", config, null, {
      data,
      status,
      statusText: "error",
      headers: {},
      config,
    }),
  );
}

/** 只实现列表页用得到的几条：多写一条路由就多一处会和后端一起漂的假实现。 */
const adapter = vi.fn(async (config: InternalAxiosRequestConfig) => {
  const method = (config.method ?? "get").toUpperCase();
  const url = config.url ?? "";
  backend.requests.push({ method, uri: api.getUri(config) });

  if (method === "GET" && url === "/peco") {
    return reply(config, {
      ws_id: "ws_peco",
      work_dir: "/tmp/peco",
      team_id: "peco",
      reserved: true,
      from_marker: true,
    });
  }
  if (method === "GET" && url === "/tasks") return reply(config, []);
  if (method === "GET" && url === "/spaces")
    return reply(config, backend.spaces);
  if (method === "GET" && url === "/spaces/trash") {
    if (backend.trashError !== null) {
      return fail(config, { details: backend.trashError }, 500);
    }
    return reply(config, backend.trash);
  }

  const restore = /^\/spaces\/([^/]+)\/restore$/.exec(url);
  if (restore && method === "POST") {
    const wsId = decodeURIComponent(restore[1]);
    backend.trash = backend.trash.filter((row) => row.ws_id !== wsId);
    backend.spaces = [space(), ...backend.spaces];
    return reply(config, { restored: true, ws_id: wsId });
  }

  const removal = /^\/spaces\/([^/]+)$/.exec(url);
  if (removal && method === "DELETE") {
    const wsId = decodeURIComponent(removal[1]);
    if (config.params?.purge === true) {
      backend.trash = backend.trash.filter((row) => row.ws_id !== wsId);
      return reply(config, { purged: true });
    }
    const gone = backend.spaces.find((row) => row.ws_id === wsId);
    backend.spaces = backend.spaces.filter((row) => row.ws_id !== wsId);
    backend.trash = [
      trashRow({ ws_id: wsId, name: gone?.label ?? null }),
      ...backend.trash,
    ];
    return reply(config, { trashed: true });
  }

  throw new Error(`未预料的请求：${method} ${url}`);
});

function renderList() {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  });
  return render(
    <QueryClientProvider client={client}>
      <MemoryRouter initialEntries={["/spaces"]}>
        <SpaceListPage />
      </MemoryRouter>
    </QueryClientProvider>,
  );
}

function requestsTo(
  predicate: (request: { method: string; uri: string }) => boolean,
) {
  return backend.requests.filter(predicate);
}

function deletes() {
  return requestsTo((request) => request.method === "DELETE");
}

async function openTrashTab() {
  fireEvent.click(await screen.findByRole("tab", { name: /回收站/ }));
}

beforeEach(() => {
  mocks.success.mockClear();
  mocks.error.mockClear();
  adapter.mockClear();
  api.defaults.adapter = adapter;
  backend = { spaces: [space()], trash: [], trashError: null, requests: [] };
  useSpaceStore.setState({ currentWsId: "ws_peco" });
  useOpenSpacesStore.setState({ openIds: [], activeWsId: null, seeded: true });
});

describe("移入回收站", () => {
  it("删除走 DELETE，且不带 purge —— 会话与记忆留在回收站", async () => {
    renderList();

    fireEvent.click(
      await screen.findByRole("button", {
        name: "删除空间 空间 A（移入回收站）",
      }),
    );

    expect(
      screen.getByText(
        "会话与记忆将保留在回收站；工作目录里的文件不会被删除。",
      ),
    ).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "移入回收站" }));

    await waitFor(() => expect(deletes()).toHaveLength(1));
    expect(deletes()[0].uri).toContain("/spaces/ws_a");
    expect(deletes()[0].uri).not.toContain("purge");
    await waitFor(() =>
      expect(mocks.success).toHaveBeenCalledWith("已移入回收站"),
    );
  });

  it("回收站已满时提示最早的一项被自动销毁", async () => {
    backend.trash = Array.from({ length: 100 }, (_, index) =>
      trashRow({ ws_id: `ws_old_${index}`, name: `旧空间 ${index}` }),
    );
    renderList();

    // 页签上的计数要先到：满不满由本地这份清单决定，回执里没有这个数。
    await waitFor(() =>
      expect(screen.getByRole("tab", { name: /100/ })).toBeTruthy(),
    );

    fireEvent.click(
      await screen.findByRole("button", {
        name: "删除空间 空间 A（移入回收站）",
      }),
    );
    fireEvent.click(screen.getByRole("button", { name: "移入回收站" }));

    await waitFor(() =>
      expect(mocks.success).toHaveBeenCalledWith(
        "已移入回收站，回收站已满 100 条，最早的一项已被自动销毁",
      ),
    );
  });
});

describe("回收站", () => {
  it("永久删除二次确认写明会话与记忆消失、目录文件保留，并带 purge=true", async () => {
    backend.spaces = [];
    backend.trash = [trashRow()];
    renderList();
    await openTrashTab();

    fireEvent.click(
      await screen.findByRole("button", { name: "永久删除空间 空间 A" }),
    );

    expect(
      screen.getByText(
        "会话与记忆将被删除；工作目录里的文件会保留。此操作不可撤销。",
      ),
    ).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "永久删除" }));

    await waitFor(() => expect(deletes()).toHaveLength(1));
    expect(deletes()[0].uri).toContain("/spaces/ws_a");
    expect(deletes()[0].uri).toContain("purge=true");
  });

  it("恢复调 restore，并让清单与回收站一起重取", async () => {
    backend.spaces = [];
    backend.trash = [trashRow()];
    renderList();
    await openTrashTab();

    const trashReads = () =>
      requestsTo(
        (request) =>
          request.method === "GET" && request.uri.includes("/spaces/trash"),
      ).length;
    const before = trashReads();

    fireEvent.click(
      await screen.findByRole("button", { name: "恢复空间 空间 A" }),
    );

    await waitFor(() =>
      expect(
        requestsTo(
          (request) =>
            request.method === "POST" &&
            request.uri.includes("/spaces/ws_a/restore"),
        ),
      ).toHaveLength(1),
    );
    // 缓存失效是可观测的：回收站被重取，恢复回来的行也从空列表里消失。
    await waitFor(() => expect(trashReads()).toBeGreaterThan(before));
    await waitFor(() =>
      expect(
        screen.queryByRole("button", { name: "恢复空间 空间 A" }),
      ).toBeNull(),
    );
    expect(
      requestsTo(
        (request) =>
          request.method === "GET" && /^\/api\/spaces\?/.test(request.uri),
      ),
    ).not.toHaveLength(0);
  });

  it("空态给一句事实", async () => {
    backend.spaces = [];
    renderList();
    await openTrashTab();

    expect(
      await screen.findByText("回收站是空的：移入回收站的空间会列在这里"),
    ).toBeInTheDocument();
  });

  it("错误态摆出响应体原文，不吞成一句通用失败", async () => {
    backend.spaces = [];
    backend.trashError = "回收站读取失败：数据库忙";
    renderList();
    await openTrashTab();

    expect(
      await screen.findByText("回收站读取失败：数据库忙"),
    ).toBeInTheDocument();
  });
});
