import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import {
  AgentModelPicker,
  modelNames,
  providerOptions,
} from "@/components/teams/AgentModelPicker";
import type { ProviderInfo, ProviderTypeInfo } from "@/api/providers";
import { MODELS, PROVIDERS } from "@/lib/constants";

const listProviders = vi.fn<(teamId: string) => Promise<ProviderInfo[]>>();
const listProviderTypes =
  vi.fn<(teamId: string) => Promise<ProviderTypeInfo[]>>();

vi.mock("@/api/providers", async (importOriginal) => {
  const actual = await importOriginal<typeof import("@/api/providers")>();
  return {
    ...actual,
    listProviders: (teamId: string) => listProviders(teamId),
    listProviderTypes: (teamId: string) => listProviderTypes(teamId),
  };
});

const DEEPSEEK: ProviderTypeInfo = {
  provider_type: "deepseek",
  display_name: "DeepSeek",
  default_base_url: "https://api.deepseek.com",
  api_modes: ["responses", "chat"],
  suggested_models: ["deepseek-v4-flash", "deepseek-v4-pro"],
  default_model: "deepseek-v4-flash",
};

const QWEN: ProviderTypeInfo = {
  provider_type: "qwen",
  display_name: "通义千问（DashScope）",
  default_base_url: "https://dashscope.aliyuncs.com/compatible-mode/v1",
  api_modes: ["responses"],
  suggested_models: ["qwen3.7-max"],
  default_model: "qwen3.7-max",
};

const TYPES = [DEEPSEEK, QWEN];

/** 团队自己的别名条目：常量表里没有它，正是 G6 里选不到的那种。 */
const ALIAS: ProviderInfo = {
  name: "deepseek-2",
  provider_type: "deepseek",
  base_url: "https://proxy.internal/v1",
  default_model: "my-finetune",
  api: null,
  has_api_key: true,
  is_default: false,
  source: "team",
};

/** 从 Peco 继承来的 qwen：同样得能选。 */
const INHERITED: ProviderInfo = {
  name: "qwen",
  provider_type: "qwen",
  base_url: null,
  default_model: null,
  api: null,
  has_api_key: true,
  is_default: false,
  source: "peco",
};

const TEAM = "t1";

/**
 * 候选表先落进缓存再渲染：组件挂载时就有一份确定的数据，断言不用等网络那一步。
 * 请求本身有没有发出去由 `listProviders` 的调用记录单独断言。
 */
function renderPicker(
  props: Partial<Parameters<typeof AgentModelPicker>[0]>,
  seed: ProviderInfo[] = [],
) {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  });
  client.setQueryData(["providers", TEAM], seed);
  client.setQueryData(["provider-types", TEAM], TYPES);
  return render(
    <QueryClientProvider client={client}>
      <AgentModelPicker
        teamId={TEAM}
        provider={null}
        model={null}
        onProviderChange={() => {}}
        onModelChange={() => {}}
        {...props}
      />
    </QueryClientProvider>,
  );
}

/** Radix Select 用键盘打开：jsdom 里的指针事件它不认（要 `pointerType === mouse`）。 */
async function optionsOf(name: string): Promise<string[]> {
  fireEvent.keyDown(screen.getByRole("combobox", { name }), { key: "Enter" });
  const options = await screen.findAllByRole("option");
  const labels = options.map((option) => option.textContent ?? "");
  fireEvent.keyDown(document.activeElement ?? document.body, {
    key: "Escape",
  });
  return labels;
}

beforeEach(() => {
  listProviders.mockReset().mockResolvedValue([]);
  listProviderTypes.mockReset().mockResolvedValue(TYPES);
});

describe("候选表（纯函数）", () => {
  it("provider 候选取生效配置的名字，读不到时回退常量表", () => {
    expect(
      providerOptions([ALIAS, INHERITED], null).map((o) => o.value),
    ).toEqual(["deepseek-2", "qwen"]);
    expect(providerOptions([], null).map((o) => o.value)).toEqual(
      PROVIDERS.map((option) => option.value),
    );
  });

  it("当前值不在候选里时补一格，否则一保存就把它丢了", () => {
    const kept = providerOptions([ALIAS], "legacy-proxy");

    expect(kept.map((option) => option.value)).toEqual([
      "deepseek-2",
      "legacy-proxy",
    ]);
    expect(kept[1].label).toBe("legacy-proxy（自定义）");
    // 已经在候选里的不重复补。
    expect(providerOptions([ALIAS], "deepseek-2")).toHaveLength(1);
  });

  it("模型候选 = 该 provider 的默认模型 + 同类型目录候选，去重保序", () => {
    expect(modelNames(ALIAS, TYPES)).toEqual([
      "my-finetune",
      "deepseek-v4-flash",
      "deepseek-v4-pro",
    ]);
    // 没设默认模型时只剩目录候选。
    expect(modelNames(INHERITED, TYPES)).toEqual(["qwen3.7-max"]);
    // 类型不认识时只有它自己的默认模型。
    expect(modelNames({ ...ALIAS, provider_type: "groq" }, TYPES)).toEqual([
      "my-finetune",
    ]);
    expect(modelNames(undefined, TYPES)).toEqual([]);
  });
});

describe("AgentModelPicker", () => {
  it("provider 下拉用配置里的名字（含别名与继承条目），不是常量表", async () => {
    listProviders.mockResolvedValue([ALIAS, INHERITED]);
    renderPicker({}, [ALIAS, INHERITED]);

    expect(await optionsOf("provider")).toEqual(["deepseek-2", "qwen"]);
    await waitFor(() => expect(listProviders).toHaveBeenCalledWith(TEAM));
  });

  it("模型候选取自选中 provider 的默认模型与类型目录", async () => {
    renderPicker({ provider: "deepseek-2" }, [ALIAS, INHERITED]);

    expect(await optionsOf("model")).toEqual([
      "my-finetune",
      "deepseek-v4-flash",
      "deepseek-v4-pro",
    ]);
  });

  it("agent.md 里的未知模型仍保留一格，不被候选表挤掉", async () => {
    renderPicker({ provider: "deepseek-2", model: "model-from-md" }, [ALIAS]);

    expect(await optionsOf("model")).toContain("model-from-md（自定义）");
  });

  it("配置为空时回退常量表", async () => {
    renderPicker({});

    expect(await optionsOf("provider")).toEqual(
      PROVIDERS.map((option) => option.label),
    );
    expect(await optionsOf("model")).toEqual(
      MODELS.map((option) => option.label),
    );
  });

  it("请求失败时也回退常量表，下拉不留空", async () => {
    listProviders.mockRejectedValue(new Error("boom"));
    renderPicker({});

    expect(await optionsOf("provider")).toEqual(
      PROVIDERS.map((option) => option.label),
    );
    await waitFor(() => expect(listProviders).toHaveBeenCalledWith(TEAM));
  });
});
