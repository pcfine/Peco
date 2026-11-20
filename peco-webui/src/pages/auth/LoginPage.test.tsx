import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { LoginPage } from "@/pages/auth/LoginPage";
import { login } from "@/api/auth";
import { ApiError } from "@/lib/api";
import { useAuthStore } from "@/stores/authStore";

// 登录页只翻译三种服务端结果：凭据不对、尝试过多、其它。前两种的文案与
// 可用性（能不能再点）是这一页唯一的承诺。

vi.mock("@/api/auth", () => ({
  getAuthStatus: vi.fn(),
  login: vi.fn(),
  register: vi.fn(),
  logout: vi.fn(),
  changePassword: vi.fn(),
}));

function submitWith(username: string, password: string) {
  fireEvent.change(screen.getByLabelText("用户名"), {
    target: { value: username },
  });
  fireEvent.change(screen.getByLabelText("密码"), {
    target: { value: password },
  });
  fireEvent.click(screen.getByRole("button", { name: "登录" }));
}

beforeEach(() => {
  vi.mocked(login).mockReset();
  useAuthStore.setState({
    registered: true,
    authenticated: false,
    username: null,
    checking: false,
  });
});

describe("登录页", () => {
  it("空字段在提交时给行内错误并把焦点送到第一个错处", () => {
    render(<LoginPage />);

    fireEvent.click(screen.getByRole("button", { name: "登录" }));

    expect(screen.getByText("请填写用户名")).toBeInTheDocument();
    expect(screen.getByText("请填写密码")).toBeInTheDocument();
    expect(login).not.toHaveBeenCalled();
    expect(document.activeElement).toBe(screen.getByLabelText("用户名"));
    expect(screen.getByLabelText("用户名")).toHaveAttribute(
      "aria-invalid",
      "true",
    );
  });

  it("凭据不对：一句话说清，密码清空，用户名留着", async () => {
    vi.mocked(login).mockRejectedValue(
      new ApiError("invalid credentials", {
        status: 401,
        code: "invalid_credentials",
      }),
    );
    render(<LoginPage />);

    submitWith("  alice  ", "secret");

    expect(await screen.findByText("用户名或密码不对")).toBeInTheDocument();
    expect(login).toHaveBeenCalledWith({
      username: "alice",
      password: "secret",
    });
    expect(screen.getByLabelText("密码")).toHaveValue("");
    expect(screen.getByLabelText("用户名")).toHaveValue("  alice  ");
  });

  it("尝试过多：按 retry_after 数秒数，冷却期内不能再点", async () => {
    vi.mocked(login).mockRejectedValue(
      new ApiError("too many attempts", {
        status: 429,
        code: "too_many_attempts",
        retryAfter: 30,
      }),
    );
    render(<LoginPage />);

    submitWith("alice", "secret");

    expect(
      await screen.findByText("尝试过多，请 30 秒后再试"),
    ).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "登录" })).toBeDisabled();
  });

  it("冷却结束后按钮自己回来：不留一条只能刷新的死路", async () => {
    vi.mocked(login).mockRejectedValue(
      new ApiError("too many attempts", {
        status: 429,
        code: "too_many_attempts",
        retryAfter: 1,
      }),
    );
    render(<LoginPage />);

    submitWith("alice", "secret");

    expect(
      await screen.findByText("尝试过多，请 1 秒后再试"),
    ).toBeInTheDocument();
    await waitFor(
      () => expect(screen.getByRole("button", { name: "登录" })).toBeEnabled(),
      { timeout: 3000 },
    );
    expect(screen.queryByText(/尝试过多/)).toBeNull();
  });

  it("提交中：按钮禁用、aria-busy 为真、标题不改", async () => {
    let resolveLogin!: (value: { username: string }) => void;
    vi.mocked(login).mockReturnValue(
      new Promise((resolve) => {
        resolveLogin = resolve;
      }),
    );
    render(<LoginPage />);

    submitWith("alice", "secret");

    const button = screen.getByRole("button", { name: "登录" });
    await waitFor(() => expect(button).toHaveAttribute("aria-busy", "true"));
    expect(button).toBeDisabled();

    resolveLogin({ username: "alice" });
    await waitFor(() =>
      expect(useAuthStore.getState().authenticated).toBe(true),
    );
    expect(useAuthStore.getState().username).toBe("alice");
  });

  it("其它失败（daemon 不在）原样给出原因", async () => {
    vi.mocked(login).mockRejectedValue(
      new Error("连不上 daemon，请确认它还在运行"),
    );
    render(<LoginPage />);

    submitWith("alice", "secret");

    expect(
      await screen.findByText("连不上 daemon，请确认它还在运行"),
    ).toBeInTheDocument();
  });
});
