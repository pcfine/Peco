// 记忆页 API client 契约测试 —— 路径 / 查询参数与后端 E1/E2 逐字对齐

import { beforeEach, describe, expect, it, vi } from "vitest";
import api from "../client";
import {
  getMemoryDocument,
  getMemoryGraph,
  getSupersedeHealth,
  listMemoryAudit,
  listMemoryDocuments,
  restoreMemoryAudit,
  searchMemoryDocuments,
  triggerSupersedeReconcile,
} from "../memory";

vi.mock("../client", () => ({
  default: { get: vi.fn(), post: vi.fn() },
}));

const GET = vi.mocked(api.get);
const POST = vi.mocked(api.post);

beforeEach(() => {
  GET.mockReset();
  POST.mockReset();
});

describe("memory API", () => {
  it("getMemoryGraph() 无参时不带 params，直接 resolve data", async () => {
    const payload = {
      nodes: [{ id: "entity:Entity:6a913f2e89d26973", name: "小C" }],
      edges: [
        {
          source: "entity:Entity:6a913f2e89d26973",
          target: "entity:Entity:fec930c4733387a6",
          predicate: "朋友",
          weight: 0.95,
        },
      ],
      truncated: false,
    };
    GET.mockResolvedValue({ data: payload });

    await expect(getMemoryGraph()).resolves.toEqual(payload);
    expect(GET).toHaveBeenCalledWith("/peco/memory/graph");
  });

  it("getMemoryGraph(50,100) 传 node_limit / edge_limit", async () => {
    GET.mockResolvedValue({ data: { nodes: [], edges: [], truncated: false } });

    await getMemoryGraph(50, 100);

    expect(GET).toHaveBeenCalledWith("/peco/memory/graph", {
      params: { node_limit: 50, edge_limit: 100 },
    });
  });

  it("listMemoryDocuments(20,10) 传 offset / limit", async () => {
    const payload = {
      documents: [
        {
          id: "c6cd2285ca896c7d",
          title: "memory_1789_0",
          source: "ppa_profile",
          file_type: null,
        },
      ],
      offset: 20,
      limit: 10,
      has_more: true,
    };
    GET.mockResolvedValue({ data: payload });

    await expect(listMemoryDocuments(20, 10)).resolves.toEqual(payload);
    expect(GET).toHaveBeenCalledWith("/peco/memory/documents", {
      params: { offset: 20, limit: 10 },
    });
  });

  it("listMemoryDocuments(0,20) 不塞 source 键", async () => {
    GET.mockResolvedValue({
      data: { documents: [], offset: 0, limit: 20, has_more: false },
    });

    await listMemoryDocuments(0, 20);

    expect(GET).toHaveBeenCalledWith("/peco/memory/documents", {
      params: { offset: 0, limit: 20 },
    });
  });

  it("listMemoryDocuments(0,20,'ppa_profile') 透传 source", async () => {
    GET.mockResolvedValue({
      data: { documents: [], offset: 0, limit: 20, has_more: false },
    });

    await listMemoryDocuments(0, 20, "ppa_profile");

    expect(GET).toHaveBeenCalledWith("/peco/memory/documents", {
      params: { offset: 0, limit: 20, source: "ppa_profile" },
    });
  });

  it("getMemoryDocument('abc') GET 详情路径并 resolve data", async () => {
    const payload = {
      id: "abc",
      title: "t",
      source: "ppa_semantic",
      file_type: "txt",
      created_at: null,
      content: "正文",
    };
    GET.mockResolvedValue({ data: payload });

    await expect(getMemoryDocument("abc")).resolves.toEqual(payload);
    expect(GET).toHaveBeenCalledWith("/peco/memory/documents/abc");
  });

  it("getMemoryDocument('a/b') 对 id 做 URL 编码", async () => {
    GET.mockResolvedValue({ data: {} });

    await getMemoryDocument("a/b");

    expect(GET).toHaveBeenCalledWith("/peco/memory/documents/a%2Fb");
  });

  it("searchMemoryDocuments('小C',20) 不带 source 键", async () => {
    GET.mockResolvedValue({ data: { hits: [] } });

    await searchMemoryDocuments("小C", 20);

    expect(GET).toHaveBeenCalledWith("/peco/memory/search", {
      params: { q: "小C", limit: 20 },
    });
  });

  it("searchMemoryDocuments('小C',20,'ppa_semantic') 透传 source", async () => {
    GET.mockResolvedValue({ data: { hits: [] } });

    await searchMemoryDocuments("小C", 20, "ppa_semantic");

    expect(GET).toHaveBeenCalledWith("/peco/memory/search", {
      params: { q: "小C", limit: 20, source: "ppa_semantic" },
    });
  });

  // ── 审计 / 取代健康（历史 tab） ─────────────────────────────────────────

  const AUDIT_ROW = {
    id: 7,
    kb_name: "@private_memory",
    doc_id: "c6cd2285ca896c7d",
    title: "旧条",
    content: "正文",
    source: "ppa_semantic",
    reason: "superseded",
    deleted_by: "extraction",
    status: "done",
    deleted_at: "2026-09-20T10:30:00Z",
    successor_doc_id: "aabbccddeeff0011",
    successor_title: "新条",
    retention_days_remaining: 12,
  };

  it("listMemoryAudit(0,20) 不塞 reason 键，resolve 裸数组", async () => {
    GET.mockResolvedValue({ data: [AUDIT_ROW] });

    await expect(listMemoryAudit(0, 20)).resolves.toEqual([AUDIT_ROW]);
    expect(GET).toHaveBeenCalledWith("/peco/memory/audit", {
      params: { offset: 0, limit: 20 },
    });
  });

  it("listMemoryAudit(0,20,'superseded') 透传 reason", async () => {
    GET.mockResolvedValue({ data: [] });

    await listMemoryAudit(0, 20, "superseded");

    expect(GET).toHaveBeenCalledWith("/peco/memory/audit", {
      params: { offset: 0, limit: 20, reason: "superseded" },
    });
  });

  it("listMemoryAudit 空串 reason 不传键（口径同 source）", async () => {
    GET.mockResolvedValue({ data: [] });

    await listMemoryAudit(20, 20, "");

    expect(GET).toHaveBeenCalledWith("/peco/memory/audit", {
      params: { offset: 20, limit: 20 },
    });
  });

  it("restoreMemoryAudit(7) POST 回滚路径并 resolve data", async () => {
    const payload = {
      success: true,
      doc_id: "c6cd2285ca896c7d",
      restored_at: "2026-09-21T08:00:00Z",
    };
    POST.mockResolvedValue({ data: payload });

    await expect(restoreMemoryAudit(7)).resolves.toEqual(payload);
    expect(POST).toHaveBeenCalledWith("/peco/memory/audit/7/restore");
  });

  it("getSupersedeHealth() GET 健康计数", async () => {
    const payload = {
      pending: 2,
      processing: 1,
      failed: 0,
      degraded: 3,
      last_converged_at: null,
    };
    GET.mockResolvedValue({ data: payload });

    await expect(getSupersedeHealth()).resolves.toEqual(payload);
    expect(GET).toHaveBeenCalledWith("/peco/memory/supersede/health");
  });

  it("triggerSupersedeReconcile() POST 对账并 resolve 同形计数", async () => {
    const payload = {
      pending: 0,
      processing: 0,
      failed: 0,
      degraded: 0,
      last_converged_at: "2026-09-21T08:00:00Z",
    };
    POST.mockResolvedValue({ data: payload });

    await expect(triggerSupersedeReconcile()).resolves.toEqual(payload);
    expect(POST).toHaveBeenCalledWith("/peco/memory/supersede/reconcile");
  });
});
