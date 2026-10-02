// 记忆页 API client 契约测试 —— 路径 / 查询参数与后端 E1/E2 逐字对齐（design §5.3 F8）

import { beforeEach, describe, expect, it, vi } from "vitest";
import api from "../client";
import { getMemoryGraph, listMemoryDocuments } from "../memory";

vi.mock("../client", () => ({
  default: { get: vi.fn() },
}));

const GET = vi.mocked(api.get);

beforeEach(() => {
  GET.mockReset();
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
});
