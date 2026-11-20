import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { RegisterPage } from "@/pages/auth/RegisterPage";
import { getAuthStatus, register } from "@/api/auth";
import { ApiError } from "@/lib/api";
import { useAuthStore } from "@/stores/authStore";

// 注册页的校验与错误映射：前端只拦「明显无效」，其余交给服务端；
// setup token 的出处必须写在文案里，否则用户拿到 403 也无从下手。

vi.mock("@/api/auth", () => ({
  getAuthStatus: vi.fn(),
  login: vi.fn(),
  register: vi.fn(),
  logout: vi.fn(),
  changePassword: vi.fn(),
}));

const VALID = {
  username: "alice",
  password: "password123",
  confirm: "password123",
  token: "a".repeat(64),
};

function fill(overrides: Partial<typeof VALID> = {}) {
  const values = { ...VALID, ...overrides };
  fireEvent.change(screen.getByLabelText("用户名"), {
    target: { value: values.username },
  });
  fireEvent.change(screen.getByLabelText("密码"), {
    target: { value: values.password },
  });
  fireEvent.change(screen.getByLabelText("确认密码"), {
    target: { value: values.confirm },
  });
  fireEvent.change(screen.getByLabelText("setup token"), {
    target: { value: values.token },
  });
}

function submit() {
  fireEvent.click(screen.getByRole("button", { name: "创建账号" }));
}

beforeEach(() => {
  vi.mocked(register).mockReset();
  vi.mocked(getAuthStatus).mockReset();
  useAuthStore.setState({
    registered: false,
    authenticated: false,
    username: null,
    checking: false,
  });
});

describe("注册页", () => {
  it("空表单：字段级错误各自就位，焦点落在第一个错上，一个请求都不发", () => {
    render(<RegisterPage />);

    submit();

    expect(screen.getByText("用户名不能为空")).toBeInTheDocument();
    expect(screen.getByText("密码要 8 到 128 个字符")).toBeInTheDocument();
    expect(screen.getByText("请粘贴 setup token")).toBeInTheDocument();
    expect(register).not.toHaveBeenCalled();
    expect(document.activeElement).toBe(screen.getByLabelText("用户名"));
  });

  it("密码太短与两次不一致都是本地拦截", () => {
    render(<RegisterPage />);

    fill({ password: "short", confirm: "short" });
    submit();
    expect(screen.getByText("密码要 8 到 128 个字符")).toBeInTheDocument();

    fill({ password: "password123", confirm: "password124" });
    submit();
    expect(screen.getByText("两次输入的密码不一致")).toBeInTheDocument();
    expect(document.activeElement).toBe(screen.getByLabelText("确认密码"));
    expect(register).not.toHaveBeenCalled();
  });

  it("成功：写进会话态，用户名与 token 去空白后上送", async () => {
    vi.mocked(register).mockResolvedValue({ username: "alice" });
    render(<RegisterPage />);

    fill({ username: "  alice  ", token: `  ${"a".repeat(64)}  ` });
    submit();

    await waitFor(() =>
      expect(useAuthStore.getState().authenticated).toBe(true),
    );
    expect(useAuthStore.getState().username).toBe("alice");
    expect(register).toHaveBeenCalledWith({
      username: "alice",
      password: "password123",
      setup_token: "a".repeat(64),
    });
  });

  it("setup token 不对：错误挂在字段上并给出 token 的取法", async () => {
    vi.mocked(register).mockRejectedValue(
      new ApiError("invalid setup token", {
        status: 403,
        code: "invalid_setup_token",
      }),
    );
    render(<RegisterPage />);

    fill();
    submit();

    expect(
      await screen.findByText(
        "setup token 不对，请在 daemon 机器上执行 cat ~/.peco-daemon/token",
      ),
    ).toBeInTheDocument();
    expect(document.activeElement).toBe(screen.getByLabelText("setup token"));
    expect(useAuthStore.getState().authenticated).toBe(false);
  });

  it("invalid_input：服务端给的 details 原样展示", async () => {
    vi.mocked(register).mockRejectedValue(
      new ApiError("密码不能与用户名相同", {
        status: 400,
        code: "invalid_input",
      }),
    );
    render(<RegisterPage />);

    fill();
    submit();

    expect(await screen.findByText("密码不能与用户名相同")).toBeInTheDocument();
  });

  it("already_registered：本地的「未注册」过期了，重问状态而不是把人卡在注册页", async () => {
    vi.mocked(register).mockRejectedValue(
      new ApiError("already registered", {
        status: 409,
        code: "already_registered",
      }),
    );
    vi.mocked(getAuthStatus).mockResolvedValue({
      registered: true,
      authenticated: false,
      username: null,
    });
    render(<RegisterPage />);

    fill();
    submit();

    await waitFor(() => expect(getAuthStatus).toHaveBeenCalledTimes(1));
    expect(useAuthStore.getState().registered).toBe(true);
    expect(useAuthStore.getState().authenticated).toBe(false);
  });
});
