import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { AuthGate } from "@/components/auth/AuthGate";
import { getAuthStatus } from "@/api/auth";
import { useAuthStore } from "@/stores/authStore";

// 闸门的三条去路与两处等待：未注册 → 注册页，已注册未登录 → 登录页，
// 已登录 → 应用；状态未回话时既不能先画应用也不能先画登录页。

vi.mock("@/api/auth", () => ({
  getAuthStatus: vi.fn(),
  login: vi.fn(),
  register: vi.fn(),
  logout: vi.fn(),
  changePassword: vi.fn(),
}));

function renderGate() {
  return render(
    <AuthGate>
      <p data-testid="app">应用</p>
    </AuthGate>,
  );
}

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((settle) => {
    resolve = settle;
  });
  return { promise, resolve };
}

beforeEach(() => {
  vi.mocked(getAuthStatus).mockReset();
  useAuthStore.setState({
    registered: false,
    authenticated: false,
    username: null,
    checking: true,
  });
});

afterEach(() => {
  vi.mocked(getAuthStatus).mockReset();
});

describe("AuthGate", () => {
  it("未注册：默认弹注册页，应用不渲染", async () => {
    vi.mocked(getAuthStatus).mockResolvedValue({
      registered: false,
      authenticated: false,
      username: null,
    });
    renderGate();

    expect(
      await screen.findByRole("heading", { level: 1, name: "创建账号" }),
    ).toBeInTheDocument();
    expect(screen.queryByTestId("app")).toBeNull();
    expect(screen.queryByRole("heading", { name: "登录" })).toBeNull();
  });

  it("已注册未登录：给登录页", async () => {
    vi.mocked(getAuthStatus).mockResolvedValue({
      registered: true,
      authenticated: false,
      username: null,
    });
    renderGate();

    expect(
      await screen.findByRole("heading", { level: 1, name: "登录" }),
    ).toBeInTheDocument();
    expect(screen.queryByTestId("app")).toBeNull();
    expect(screen.queryByRole("heading", { name: "创建账号" })).toBeNull();
  });

  it("已登录：渲染原应用", async () => {
    vi.mocked(getAuthStatus).mockResolvedValue({
      registered: true,
      authenticated: true,
      username: "pcfine",
    });
    renderGate();

    expect(await screen.findByTestId("app")).toBeInTheDocument();
    expect(screen.queryByRole("heading", { name: "登录" })).toBeNull();
    expect(useAuthStore.getState().username).toBe("pcfine");
  });

  it("状态未回话前只给占位：不闪应用，也不闪登录页", async () => {
    const pending = deferred<{
      registered: boolean;
      authenticated: boolean;
      username: string | null;
    }>();
    vi.mocked(getAuthStatus).mockReturnValue(pending.promise);
    renderGate();

    expect(screen.getByRole("status")).toHaveTextContent("正在确认登录状态…");
    expect(screen.queryByTestId("app")).toBeNull();
    expect(screen.queryByRole("heading", { name: "登录" })).toBeNull();
    expect(screen.queryByRole("heading", { name: "创建账号" })).toBeNull();

    pending.resolve({
      registered: true,
      authenticated: true,
      username: "pcfine",
    });
    expect(await screen.findByTestId("app")).toBeInTheDocument();
  });

  it("状态查不到时给错误态与重试，不把 daemon 掉线说成未注册", async () => {
    vi.mocked(getAuthStatus)
      .mockRejectedValueOnce(new Error("连不上 daemon，请确认它还在运行"))
      .mockResolvedValueOnce({
        registered: true,
        authenticated: true,
        username: "pcfine",
      });
    renderGate();

    expect(
      await screen.findByRole("heading", { name: "无法确认登录状态" }),
    ).toBeInTheDocument();
    expect(
      screen.getByText("连不上 daemon，请确认它还在运行"),
    ).toBeInTheDocument();
    expect(screen.queryByTestId("app")).toBeNull();

    fireEvent.click(screen.getByRole("button", { name: "重试" }));

    await waitFor(() => expect(screen.getByTestId("app")).toBeInTheDocument());
  });
});
