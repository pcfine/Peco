import { beforeEach, describe, expect, it, vi } from "vitest";
import {
  fireEvent,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { ApiError } from "@/lib/api";
import {
  ProviderSection,
  blankForm,
  formForTypeChange,
  formFromProvider,
  toTestRequest,
  toUpsertRequest,
  uniqueName,
  validateForm,
  type FormState,
} from "@/components/settings/ProviderSection";
import type { ProviderInfo, ProviderTypeInfo } from "@/api/providers";

// 只替换 axios 实例：URL、请求体与「哪些字段被略过」都在这一层看得见。
const get = vi.fn();
const put = vi.fn();
const post = vi.fn();
const del = vi.fn();

vi.mock("@/lib/api", async (importOriginal) => {
  const actual = await importOriginal<typeof import("@/lib/api")>();
  return {
    ...actual,
    api: {
      get: (url: string, config?: unknown) => get(url, config),
      put: (url: string, body: unknown, config?: unknown) =>
        put(url, body, config),
      post: (url: string, body: unknown, config?: unknown) =>
        post(url, body, config),
      delete: (url: string, config?: unknown) => del(url, config),
    },
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

const OPENAI: ProviderTypeInfo = {
  provider_type: "openai",
  display_name: "OpenAI",
  default_base_url: "https://api.openai.com/v1",
  api_modes: ["responses", "chat"],
  suggested_models: ["gpt-5"],
  default_model: "gpt-5",
};

const TYPES = [DEEPSEEK, QWEN, OPENAI];

const EXISTING: ProviderInfo = {
  name: "deepseek",
  provider_type: "deepseek",
  base_url: "https://api.deepseek.com",
  default_model: "deepseek-v4-flash",
  api: null,
  has_api_key: true,
  is_default: true,
};

const GATEWAY: ProviderInfo = {
  name: "gateway",
  provider_type: "openai",
  base_url: null,
  default_model: null,
  api: "chat",
  has_api_key: false,
  is_default: false,
};

const TEAM = "t1";

let providers: ProviderInfo[];

function renderSection() {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  });
  return render(
    <QueryClientProvider client={client}>
      <ProviderSection teamId={TEAM} />
    </QueryClientProvider>,
  );
}

/** 打开新建对话框并等到类型目录到位（类型未到时按钮是禁用的）。 */
async function openCreate() {
  const button = await screen.findByRole("button", { name: /新建 Provider/ });
  await waitFor(() => expect(button).toBeEnabled());
  fireEvent.click(button);
  return screen.findByRole("dialog");
}

async function openEdit(name: string) {
  fireEvent.click(await screen.findByRole("button", { name: `编辑 ${name}` }));
  return screen.findByRole("dialog");
}

beforeEach(() => {
  providers = [];
  get.mockReset().mockImplementation(async (url: string) => {
    if (url === `/teams/${TEAM}/providers`) return { data: providers };
    if (url === `/teams/${TEAM}/providers/types`) return { data: TYPES };
    throw new Error(`unexpected GET ${url}`);
  });
  put
    .mockReset()
    .mockResolvedValue({ data: { success: true, message: "Provider saved" } });
  post.mockReset();
  del.mockReset().mockResolvedValue({
    data: { success: true, message: "Provider deleted" },
  });
});

describe("表单纯函数", () => {
  it("新建的默认名字避让已存在的 provider", () => {
    expect(uniqueName("deepseek", [])).toBe("deepseek");
    expect(uniqueName("deepseek", ["deepseek"])).toBe("deepseek-2");
    expect(uniqueName("deepseek", ["deepseek", "deepseek-2"])).toBe(
      "deepseek-3",
    );
  });

  it("新建表单带上该类型的默认地址与默认模型", () => {
    const form = blankForm(TYPES, []);

    expect(form).toMatchObject({
      providerType: "deepseek",
      name: "deepseek",
      apiKey: "",
      baseUrl: "https://api.deepseek.com",
      api: "",
      model: "deepseek-v4-flash",
      setDefault: false,
    });
  });

  it("切类型只重填还是默认值的字段，手改过的地址与模型不动", () => {
    const switched = formForTypeChange(blankForm(TYPES, []), DEEPSEEK, QWEN, {
      isNew: true,
      taken: [],
    });
    expect(switched.name).toBe("qwen");
    expect(switched.baseUrl).toBe(
      "https://dashscope.aliyuncs.com/compatible-mode/v1",
    );
    expect(switched.model).toBe("qwen3.7-max");

    const custom: FormState = {
      ...blankForm(TYPES, []),
      baseUrl: "https://proxy.internal/v1",
      model: "my-finetune",
    };
    const kept = formForTypeChange(custom, DEEPSEEK, QWEN, {
      isNew: true,
      taken: [],
    });
    expect(kept.baseUrl).toBe("https://proxy.internal/v1");
    expect(kept.model).toBe("my-finetune");
  });

  it("编辑时名字是主键，不跟随类型变化", () => {
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

  it("留空 api_key 时不发送该字段，填了才带上", () => {
    const blank = toUpsertRequest(blankForm(TYPES, []));
    expect(blank).not.toHaveProperty("api_key");
    expect(blank).not.toHaveProperty("set_default");
    expect(blank).toMatchObject({
      name: "deepseek",
      type: "deepseek",
      base_url: "https://api.deepseek.com",
      api: "",
      default_model: "deepseek-v4-flash",
    });

    const filled = toUpsertRequest({
      ...blankForm(TYPES, []),
      apiKey: "  sk-test  ",
      setDefault: true,
    });
    expect(filled.api_key).toBe("sk-test");
    expect(filled.set_default).toBe(true);
  });

  it("编辑态只回显已存值，空字段不拿目录默认值补", () => {
    const bare = formFromProvider({
      name: "gateway",
      provider_type: "openai",
      base_url: null,
      default_model: null,
      api: null,
      has_api_key: true,
      is_default: false,
    });

    expect(bare).toMatchObject({ apiKey: "", baseUrl: "", api: "", model: "" });
    const req = toUpsertRequest(bare);
    expect(req).not.toHaveProperty("api_key");
    expect(req.base_url).toBe("");
    expect(req.default_model).toBe("");
  });

  it("新建要求 API Key 与模型，编辑允许两者留空", () => {
    const blank = blankForm(TYPES, []);

    expect(validateForm(blank, true)).toContain("API Key");
    expect(validateForm(blank, false)).toBeNull();

    const noModel: FormState = { ...blank, apiKey: "sk-test", model: " " };
    expect(validateForm(noModel, true)).toContain("默认模型");
    expect(validateForm(noModel, false)).toBeNull();
  });

  it("草稿探针请求体直传表单值，名称留空回退为类型名", () => {
    expect(toTestRequest({ ...blankForm(TYPES, []), name: "" })).toEqual({
      name: "deepseek",
      type: "deepseek",
      api_key: "",
      base_url: "https://api.deepseek.com",
      api: "",
      default_model: "deepseek-v4-flash",
    });
  });
});

describe("ProviderSection", () => {
  it("列表展示类型、地址、模型与密钥/默认标记", async () => {
    providers = [EXISTING, GATEWAY];
    renderSection();

    expect(await screen.findByText("已配置密钥")).toBeInTheDocument();
    expect(screen.getByText("默认")).toBeInTheDocument();
    expect(screen.getByText("未配置密钥").className).toContain("text-danger");
    expect(screen.getByText("DeepSeek")).toBeInTheDocument();
    expect(screen.getByText("https://api.deepseek.com")).toBeInTheDocument();
    expect(
      screen.getByText("https://api.openai.com/v1（类型默认）"),
    ).toBeInTheDocument();
    expect(screen.getByText(/未设默认模型/)).toBeInTheDocument();
    expect(get).toHaveBeenCalledWith(`/teams/${TEAM}/providers`, undefined);
  });

  it("新建提交体带类型/地址/模型/密钥，不带上未勾选的默认位", async () => {
    renderSection();
    const dialog = await openCreate();

    fireEvent.change(within(dialog).getByLabelText("API Key"), {
      target: { value: "sk-test" },
    });
    fireEvent.click(within(dialog).getByRole("button", { name: "保存" }));

    await waitFor(() => expect(put).toHaveBeenCalledTimes(1));
    expect(put.mock.calls[0][0]).toBe(`/teams/${TEAM}/providers`);
    expect(put.mock.calls[0][1]).toEqual({
      name: "deepseek",
      type: "deepseek",
      base_url: "https://api.deepseek.com",
      api: "",
      default_model: "deepseek-v4-flash",
      api_key: "sk-test",
    });
  });

  it("编辑时 api_key 留空不发送，已存地址与模型原样回显", async () => {
    providers = [EXISTING];
    renderSection();
    const dialog = await openEdit("deepseek");

    expect(within(dialog).getByLabelText("名称")).toHaveValue("deepseek");
    expect(within(dialog).getByLabelText("Base URL")).toHaveValue(
      "https://api.deepseek.com",
    );
    expect(within(dialog).getByText(/已配置密钥/)).toBeInTheDocument();
    expect(within(dialog).getByLabelText("API Key")).toHaveValue("");

    fireEvent.click(within(dialog).getByRole("button", { name: "保存" }));

    await waitFor(() => expect(put).toHaveBeenCalledTimes(1));
    const body = put.mock.calls[0][1] as Record<string, unknown>;
    expect(body).toEqual({
      name: "deepseek",
      type: "deepseek",
      base_url: "https://api.deepseek.com",
      api: "",
      default_model: "deepseek-v4-flash",
      // 它本来就是默认 provider：勾选态回读，PUT 幂等地再申明一次
      set_default: true,
    });
    expect(body).not.toHaveProperty("api_key");
  });

  it("已保存配置试连失败时把服务端 message 摆在那一行", async () => {
    providers = [EXISTING];
    post.mockResolvedValue({
      data: {
        success: false,
        message: "HTTP 401（API Key 无效）：invalid api key",
        provider_type: "deepseek",
        model: "deepseek-v4-flash",
      },
    });
    renderSection();

    fireEvent.click(await screen.findByRole("button", { name: "试连" }));

    const line = await screen.findByText(/HTTP 401（API Key 无效）/);
    expect(line.className).toContain("text-danger");
    expect(post.mock.calls[0][0]).toBe(
      `/teams/${TEAM}/providers/deepseek/test`,
    );
  });

  it("保存前试连用的是表单当前值", async () => {
    post.mockResolvedValue({
      data: {
        success: true,
        message: "连接正常",
        provider_type: "deepseek",
        model: "deepseek-v4-flash",
      },
    });
    renderSection();
    const dialog = await openCreate();

    fireEvent.change(within(dialog).getByLabelText("API Key"), {
      target: { value: "sk-test" },
    });
    fireEvent.click(within(dialog).getByRole("button", { name: "试连" }));

    await waitFor(() => expect(post).toHaveBeenCalledTimes(1));
    expect(post.mock.calls[0][0]).toBe(`/teams/${TEAM}/providers/test`);
    expect(post.mock.calls[0][1]).toEqual({
      name: "deepseek",
      type: "deepseek",
      api_key: "sk-test",
      base_url: "https://api.deepseek.com",
      api: "",
      default_model: "deepseek-v4-flash",
    });
    expect(await screen.findByText(/试连成功/)).toBeInTheDocument();
  });

  it("删除先要确认，确认后才发 DELETE", async () => {
    providers = [EXISTING];
    renderSection();

    fireEvent.click(
      await screen.findByRole("button", { name: "删除 deepseek" }),
    );
    const dialog = await screen.findByRole("dialog");
    expect(
      within(dialog).getByText("删除 Provider「deepseek」？"),
    ).toBeInTheDocument();
    expect(del).not.toHaveBeenCalled();

    fireEvent.click(within(dialog).getByRole("button", { name: "确认删除" }));

    await waitFor(() => expect(del).toHaveBeenCalledTimes(1));
    expect(del.mock.calls[0][0]).toBe(`/teams/${TEAM}/providers/deepseek`);
  });

  it("保存被后端拒绝时原文摆在对话框里，对话框不关", async () => {
    put.mockRejectedValue(
      new ApiError("invalid provider type 'anthropic'", { status: 400 }),
    );
    renderSection();
    const dialog = await openCreate();

    fireEvent.change(within(dialog).getByLabelText("API Key"), {
      target: { value: "sk-test" },
    });
    fireEvent.click(within(dialog).getByRole("button", { name: "保存" }));

    expect(
      await screen.findByText("invalid provider type 'anthropic'"),
    ).toBeInTheDocument();
    expect(screen.getByRole("dialog")).toBeInTheDocument();
  });

  it("本地校验拦下缺密钥的新建，不发请求", async () => {
    renderSection();
    const dialog = await openCreate();

    fireEvent.click(within(dialog).getByRole("button", { name: "保存" }));

    expect(await screen.findByText(/请填写 API Key/)).toBeInTheDocument();
    expect(put).not.toHaveBeenCalled();
  });
});
