import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { AuthSection } from "@/components/settings/AuthSection";
import { changePassword, logout } from "@/api/auth";
import { ApiError } from "@/lib/api";
import { useAuthStore } from "@/stores/authStore";

// 账号区只有两件事：改密码与登出。登出必须真发请求 —— 前端读不到那个
// HttpOnly cookie，本地删什么都等于没登出。

vi.mock("@/api/auth", () => ({
  getAuthStatus: vi.fn(),
  login: vi.fn(),
  register: vi.fn(),
  logout: vi.fn(),
  changePassword: vi.fn(),
}));

function fill(current: string, next: string, confirm = next) {
  fireEvent.change(screen.getByLabelText("当前密码"), {
    target: { value: current },
  });
  fireEvent.change(screen.getByLabelText("新密码"), {
    target: { value: next },
  });
  fireEvent.change(screen.getByLabelText("确认新密码"), {
    target: { value: confirm },
  });
}

beforeEach(() => {
  vi.mocked(changePassword).mockReset();
  vi.mocked(logout).mockReset();
  useAuthStore.setState({
    registered: true,
    authenticated: true,
    username: "pcfine",
    checking: false,
  });
});

describe("设置页账号区", () => {
  it("两次新密码不一致：本地拦下，不发请求", () => {
    render(<AuthSection />);

    fill("old-password", "new-password", "new-password2");
    fireEvent.click(screen.getByRole("button", { name: "修改密码" }));

    expect(screen.getByText("两次输入的密码不一致")).toBeInTheDocument();
    expect(changePassword).not.toHaveBeenCalled();
  });

  it("改密码成功：清空字段并给一句成功提示", async () => {
    vi.mocked(changePassword).mockResolvedValue(undefined);
    render(<AuthSection />);

    fill("old-password", "new-password");
    fireEvent.click(screen.getByRole("button", { name: "修改密码" }));

    expect(await screen.findByText("密码已更新")).toBeInTheDocument();
    expect(changePassword).toHaveBeenCalledWith({
      current_password: "old-password",
      new_password: "new-password",
    });
    expect(screen.getByLabelText("当前密码")).toHaveValue("");
    expect(screen.getByLabelText("新密码")).toHaveValue("");
  });

  it("当前密码不对：错误挂在该字段上，会话态不动", async () => {
    vi.mocked(changePassword).mockRejectedValue(
      new ApiError("invalid credentials", {
        status: 401,
        code: "invalid_credentials",
      }),
    );
    render(<AuthSection />);

    fill("wrong-password", "new-password");
    fireEvent.click(screen.getByRole("button", { name: "修改密码" }));

    expect(await screen.findByText("当前密码不对")).toBeInTheDocument();
    expect(document.activeElement).toBe(screen.getByLabelText("当前密码"));
    expect(useAuthStore.getState().authenticated).toBe(true);
  });

  it("退出登录：发请求后才翻会话态", async () => {
    vi.mocked(logout).mockResolvedValue(undefined);
    render(<AuthSection />);

    fireEvent.click(screen.getByRole("button", { name: "退出登录" }));

    await waitFor(() => expect(logout).toHaveBeenCalledTimes(1));
    await waitFor(() =>
      expect(useAuthStore.getState().authenticated).toBe(false),
    );
    expect(useAuthStore.getState().username).toBeNull();
  });

  it("退出登录失败：保留会话态，把原因说出来", async () => {
    vi.mocked(logout).mockRejectedValue(new Error("连不上 daemon"));
    render(<AuthSection />);

    fireEvent.click(screen.getByRole("button", { name: "退出登录" }));

    expect(await screen.findByText("连不上 daemon")).toBeInTheDocument();
    expect(useAuthStore.getState().authenticated).toBe(true);
  });
});
