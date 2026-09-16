// ProviderSection 测试 —— 类型默认值预填 / 草稿连接测试载荷 / 保存载荷语义

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import {
  ProviderSection,
  blankForm,
  formForTypeChange,
  formFromProvider,
  toTestRequest,
  toUpsertRequest,
  uniqueName,
  validateForm,
} from "../ProviderSection";
import type { FormState } from "../ProviderSection";
import {
  listProviders,
  listProviderTypes,
  testProviderDraft,
  upsertProvider,
} from "@/api/providers";
import type { ProviderInfo, ProviderTypeInfo } from "@/api/providers";
import { toast } from "sonner";

vi.mock("@/api/providers", () => ({
  listProviders: vi.fn(),
  listProviderTypes: vi.fn(),
  upsertProvider: vi.fn(),
  deleteProvider: vi.fn(),
  testProviderConnection: vi.fn(),
  testProviderDraft: vi.fn(),
}));

vi.mock("sonner", () => ({
  toast: { error: vi.fn(), success: vi.fn() },
}));

// jsdom 未实现 ResizeObserver，radix 的 Switch / Select 直接引用它。
class ResizeObserverStub {
  observe() {}
  unobserve() {}
  disconnect() {}
}

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

const EXISTING: ProviderInfo = {
  name: "deepseek",
  provider_type: "deepseek",
  base_url: "https://api.deepseek.com",
  default_model: "deepseek-v4-flash",
  has_api_key: true,
  is_default: true,
};

const LIST_PROVIDERS = vi.mocked(listProviders);
const LIST_TYPES = vi.mocked(listProviderTypes);
const UPSERT = vi.mocked(upsertProvider);
const TEST_DRAFT = vi.mocked(testProviderDraft);

let container: HTMLDivElement;
let root: Root;

async function flush() {
  await act(async () => {
    await new Promise((resolve) => setTimeout(resolve, 0));
  });
}

async function renderSection() {
  await act(async () => {
    root.render(<ProviderSection />);
  });
  await flush();
}

/** 按可见文本点按钮 —— 对话框内容挂在 document.body 的 portal 里。 */
function clickByText(text: string) {
  const btn = Array.from(document.querySelectorAll("button")).find(
    (b) => b.textContent?.trim() === text,
  );
  if (!btn) throw new Error(`button not found: ${text}`);
  btn.click();
}

/** 取对话框里的输入框（portal 挂在 document.body 上）。 */
function inputById(id: string): HTMLInputElement {
  const el = document.getElementById(id);
  if (!el) throw new Error(`input not found: ${id}`);
  return el as HTMLInputElement;
}

/** 绕过 React 的 value 追踪，触发一次真实的 onChange。 */
function typeInto(el: HTMLInputElement, value: string) {
  const setter = Object.getOwnPropertyDescriptor(
    HTMLInputElement.prototype,
    "value",
  )?.set;
  setter?.call(el, value);
  el.dispatchEvent(new Event("input", { bubbles: true }));
}

beforeEach(() => {
  (
    globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT: boolean }
  ).IS_REACT_ACT_ENVIRONMENT = true;
  globalThis.ResizeObserver ??=
    ResizeObserverStub as unknown as typeof ResizeObserver;

  // 默认从一个空的配置列表起步（新用户首启路径）。需要"已存在同名条目"的用例
  // 自行 mockResolvedValue([EXISTING])，否则默认名字避让逻辑会把默认名顺延。
  LIST_PROVIDERS.mockReset().mockResolvedValue([]);
  LIST_TYPES.mockReset().mockResolvedValue(TYPES);
  UPSERT.mockReset().mockResolvedValue({ success: true, message: "saved" });
  TEST_DRAFT.mockReset();
  vi.mocked(toast.error).mockReset();
  vi.mocked(toast.success).mockReset();

  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
});

afterEach(() => {
  act(() => root.unmount());
  container.remove();
});

describe("表单纯函数", () => {
  it("默认名字避让已存在的 provider", () => {
    expect(uniqueName("deepseek", [])).toBe("deepseek");
    expect(uniqueName("deepseek", ["deepseek"])).toBe("deepseek-2");
    expect(uniqueName("deepseek", ["deepseek", "deepseek-2"])).toBe(
      "deepseek-3",
    );
  });

  it("新建表单直接带上该类型的默认地址与默认模型", () => {
    const form = blankForm(TYPES, []);

    expect(form.providerType).toBe("deepseek");
    expect(form.name).toBe("deepseek");
    expect(form.baseUrl).toBe("https://api.deepseek.com");
    expect(form.model).toBe("deepseek-v4-flash");
    // 空串 = API 风格跟随类型默认
    expect(form.api).toBe("");
  });

  it("切类型时重填默认值", () => {
    const next = formForTypeChange(blankForm(TYPES, []), DEEPSEEK, QWEN, {
      isNew: true,
      taken: [],
    });

    expect(next.providerType).toBe("qwen");
    expect(next.name).toBe("qwen");
    expect(next.baseUrl).toBe(
      "https://dashscope.aliyuncs.com/compatible-mode/v1",
    );
    expect(next.model).toBe("qwen3.7-max");
  });

  it("切类型不冲掉用户手改的地址与模型", () => {
    const custom: FormState = {
      ...blankForm(TYPES, []),
      baseUrl: "https://proxy.internal/v1",
      model: "my-finetune",
    };

    const next = formForTypeChange(custom, DEEPSEEK, QWEN, {
      isNew: true,
      taken: [],
    });

    expect(next.baseUrl).toBe("https://proxy.internal/v1");
    expect(next.model).toBe("my-finetune");
  });

  it("切到不支持当前 API 档位的类型时退回跟随默认", () => {
    const chat: FormState = { ...blankForm(TYPES, []), api: "chat" };

    const next = formForTypeChange(chat, DEEPSEEK, QWEN, {
      isNew: true,
      taken: [],
    });

    // qwen 只有 responses 档
    expect(next.api).toBe("");
  });

  it("编辑时名字不跟随类型（它是主键）", () => {
    const form: FormState = {
      ...blankForm(TYPES, []),
      name: "gateway",
      providerType: "deepseek",
    };

    const next = formForTypeChange(form, DEEPSEEK, QWEN, {
      isNew: false,
      taken: ["qwen"],
    });

    expect(next.name).toBe("gateway");
  });

  it("留空 api_key 时不发送该字段（保存保留已存密钥）", () => {
    const req = toUpsertRequest(blankForm(TYPES, []));

    expect(req).not.toHaveProperty("api_key");
    expect(req).not.toHaveProperty("set_default");
    expect(req.name).toBe("deepseek");
    expect(req.base_url).toBe("https://api.deepseek.com");
    expect(req.default_model).toBe("deepseek-v4-flash");
    expect(req.api).toBe("");
  });

  it("填了 api_key / 勾了默认位才带上对应字段", () => {
    const req = toUpsertRequest({
      ...blankForm(TYPES, []),
      apiKey: "  sk-test  ",
      setDefault: true,
    });

    expect(req.api_key).toBe("sk-test");
    expect(req.set_default).toBe(true);
  });

  it("名称留空时回退为类型名", () => {
    expect(toUpsertRequest({ ...blankForm(TYPES, []), name: "  " }).name).toBe(
      "deepseek",
    );
    expect(toTestRequest({ ...blankForm(TYPES, []), name: "" }).name).toBe(
      "deepseek",
    );
  });

  it("新建要求 API Key 与模型，编辑允许留空密钥与模型", () => {
    const blank = blankForm(TYPES, []);

    expect(validateForm(blank, true)).toContain("API Key");
    expect(validateForm(blank, false)).toBeNull();

    const noModel = { ...blank, apiKey: "sk-test", model: "  " };
    expect(validateForm(noModel, true)).toContain("默认模型");
    // 已有条目本就可以没有默认模型（模型在 agent.md 里指定），
    // 编辑时必须放行，否则连换个密钥都保存不了
    expect(validateForm(noModel, false)).toBeNull();
  });

  it("编辑态只回显已存值，不用目录默认值补空", () => {
    // 条目没有自定义地址与默认模型 → 表单留空，保存时保持留空
    // （后端把空串解释为"不改动"）
    const bare = formFromProvider({
      name: "gateway",
      provider_type: "openai",
      has_api_key: true,
      is_default: false,
    });

    expect(bare.baseUrl).toBe("");
    expect(bare.model).toBe("");
    expect(bare.api).toBe("");
    expect(bare.apiKey).toBe("");
    // 留空保存 = 不改动已存配置
    const req = toUpsertRequest(bare);
    expect(req.base_url).toBe("");
    expect(req.default_model).toBe("");
    expect(req).not.toHaveProperty("api_key");
  });

  it("编辑态回显已存地址与模型", () => {
    const filled = formFromProvider(EXISTING);

    expect(filled.baseUrl).toBe("https://api.deepseek.com");
    expect(filled.model).toBe("deepseek-v4-flash");
    expect(filled.setDefault).toBe(true);
  });
});

describe("ProviderSection", () => {
  it("加载后展示类型、默认模型与默认标记", async () => {
    LIST_PROVIDERS.mockResolvedValue([EXISTING]);

    await renderSection();

    expect(container.textContent).toContain("deepseek");
    expect(container.textContent).toContain("DeepSeek");
    expect(container.textContent).toContain("默认");
    expect(container.textContent).toContain("deepseek-v4-flash");
    expect(container.textContent).not.toContain("未配置 API Key");
  });

  it("缺 API Key 的条目给出提示", async () => {
    LIST_PROVIDERS.mockResolvedValue([{ ...EXISTING, has_api_key: false }]);

    await renderSection();

    expect(container.textContent).toContain("未配置 API Key");
  });

  it("新建对话框按目录预填地址与模型", async () => {
    await renderSection();

    await act(async () => {
      clickByText("添加 Provider");
    });

    expect(inputById("provider-base-url").value).toBe(
      "https://api.deepseek.com",
    );
    expect(inputById("provider-model").value).toBe("deepseek-v4-flash");
    expect(inputById("provider-name").value).toBe("deepseek");
  });

  it("草稿测试用表单当前值请求，并展示失败原因", async () => {
    TEST_DRAFT.mockResolvedValue({
      success: false,
      message: "HTTP 401（API Key 无效）：invalid api key",
      provider_type: "deepseek",
      model: "deepseek-v4-flash",
    });

    await renderSection();
    await act(async () => {
      clickByText("添加 Provider");
    });
    await act(async () => {
      typeInto(inputById("provider-api-key"), "sk-test");
    });
    await act(async () => {
      clickByText("测试连接");
    });
    await flush();

    expect(TEST_DRAFT).toHaveBeenCalledWith({
      name: "deepseek",
      type: "deepseek",
      api_key: "sk-test",
      base_url: "https://api.deepseek.com",
      api: "",
      default_model: "deepseek-v4-flash",
    });
    expect(document.body.textContent).toContain("HTTP 401");
  });

  it("未填 API Key 时禁用草稿测试", async () => {
    await renderSection();
    await act(async () => {
      clickByText("添加 Provider");
    });

    const testBtn = Array.from(document.querySelectorAll("button")).find(
      (b) => b.textContent?.trim() === "测试连接",
    );
    expect(testBtn).toBeDisabled();
  });

  it("保存提交类型/地址/模型/key，不带上未勾选的默认位", async () => {
    await renderSection();
    await act(async () => {
      clickByText("添加 Provider");
    });
    await act(async () => {
      typeInto(inputById("provider-api-key"), "sk-test");
    });
    await act(async () => {
      clickByText("保存");
    });
    await flush();

    expect(UPSERT).toHaveBeenCalledWith({
      name: "deepseek",
      type: "deepseek",
      base_url: "https://api.deepseek.com",
      api: "",
      default_model: "deepseek-v4-flash",
      api_key: "sk-test",
    });
  });

  it("新建时默认名字避让同名条目，手改成同名则提示覆盖", async () => {
    LIST_PROVIDERS.mockResolvedValue([EXISTING]);

    await renderSection();
    await act(async () => {
      clickByText("添加 Provider");
    });

    // 已存在 deepseek → 默认名字顺延，不会静默覆盖
    expect(inputById("provider-name").value).toBe("deepseek-2");
    expect(document.body.textContent).not.toContain("保存将更新它的配置");

    await act(async () => {
      typeInto(inputById("provider-name"), "deepseek");
    });

    expect(document.body.textContent).toContain("保存将更新它的配置");
  });
});
