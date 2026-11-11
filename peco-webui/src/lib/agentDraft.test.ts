import { describe, expect, it } from "vitest";
import {
  EMPTY_NEW_AGENT,
  createRequestFromDraft,
  diffDraft,
  draftFromDetail,
  toggleValue,
} from "@/lib/agentDraft";
import type { AgentDetail } from "@/types/daemon";

function agentDetail(overrides: Partial<AgentDetail> = {}): AgentDetail {
  return {
    id: "a_1",
    name: "助手",
    description: "干活的",
    system_prompt: "你是助手",
    model: "deepseek-flash",
    provider: "deepseek",
    status: "idle",
    tools: ["shell"],
    mcp_servers: [],
    skills: ["review"],
    knowledge_bases: [],
    temperature: 0.7,
    max_tokens: null,
    stream: true,
    reasoning_effort: null,
    max_iterations: 500,
    supports_images: false,
    created_at: "",
    updated_at: "",
    ...overrides,
  };
}

describe("agentDraft", () => {
  it("刚加载的详情 diff 出空请求体：一进来就没有可保存的东西", () => {
    const detail = agentDetail();
    expect(diffDraft(detail, draftFromDetail(detail))).toEqual({
      body: {},
      invalid: [],
    });
  });

  it("只改描述时请求体只有 description", () => {
    const detail = agentDetail();
    const draft = { ...draftFromDetail(detail), description: "换了个说法" };

    const diff = diffDraft(detail, draft);

    expect(diff.invalid).toEqual([]);
    expect(diff.body).toEqual({ description: "换了个说法" });
    expect(diff.body).not.toHaveProperty("system_prompt");
  });

  it("清空已设置的 temperature 发显式 null，原本就没值的字段不出现在请求体", () => {
    const detail = agentDetail({ temperature: 0.7, max_tokens: null });
    const draft = {
      ...draftFromDetail(detail),
      temperature: "",
      max_tokens: "",
    };

    const diff = diffDraft(detail, draft);

    expect(diff.body).toEqual({ temperature: null });
  });

  it("勾选顺序不同不算改动，增删才算", () => {
    const detail = agentDetail({ tools: ["shell", "fetch"] });

    const reordered = { ...draftFromDetail(detail), tools: ["fetch", "shell"] };
    expect(diffDraft(detail, reordered).body).toEqual({});

    const removed = { ...draftFromDetail(detail), tools: ["fetch"] };
    expect(diffDraft(detail, removed).body).toEqual({ tools: ["fetch"] });
  });

  it("id 清空与非法数字都不进请求体，而是给出原因", () => {
    const detail = agentDetail();
    const draft = {
      ...draftFromDetail(detail),
      name: "   ",
      max_tokens: "1.5",
    };

    const diff = diffDraft(detail, draft);

    expect(diff.body).toEqual({});
    expect(diff.invalid).toHaveLength(2);
    expect(diff.invalid[0]).toContain("id");
  });

  it("max_iterations 留空算不改：服务端没有清空它的形态", () => {
    const detail = agentDetail({ max_iterations: 500 });
    const draft = { ...draftFromDetail(detail), max_iterations: "" };

    expect(diffDraft(detail, draft).body).toEqual({});
  });

  it("新建请求体只带填过的项", () => {
    expect(createRequestFromDraft(EMPTY_NEW_AGENT)).toEqual({ name: "" });

    expect(
      createRequestFromDraft({
        ...EMPTY_NEW_AGENT,
        name: " 审查员 ",
        description: " 审查代码 ",
        model: "deepseek-v4-pro",
      }),
    ).toEqual({
      name: "审查员",
      description: "审查代码",
      model: "deepseek-v4-pro",
    });
  });

  it("toggleValue 只碰目标项", () => {
    expect(toggleValue(["a"], "b")).toEqual(["a", "b"]);
    expect(toggleValue(["a", "b"], "a")).toEqual(["b"]);
  });
});
