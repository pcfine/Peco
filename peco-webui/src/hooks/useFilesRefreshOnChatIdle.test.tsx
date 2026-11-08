import { beforeEach, describe, expect, it, vi } from "vitest";
import { act, render } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { useFilesRefreshOnChatIdle } from "@/hooks/useFilesRefreshOnChatIdle";
import { useConnectionStore } from "@/stores/connectionStore";

// 一轮聊天结束才失效一次：挂在渲染上的失效会让文件面板永远重取不完，
// 而挂在「生成中 → 空闲」翻转上则是每次落盘刚好一次。

function Harness({ tick }: { tick: number }) {
  useFilesRefreshOnChatIdle();
  return <span>{tick}</span>;
}

function setup() {
  const client = new QueryClient();
  const invalidate = vi.spyOn(client, "invalidateQueries");
  const view = render(
    <QueryClientProvider client={client}>
      <Harness tick={0} />
    </QueryClientProvider>,
  );
  const rerender = (tick: number) =>
    view.rerender(
      <QueryClientProvider client={client}>
        <Harness tick={tick} />
      </QueryClientProvider>,
    );
  return { invalidate, rerender };
}

describe("useFilesRefreshOnChatIdle", () => {
  beforeEach(() => {
    useConnectionStore.setState({ chat: { kind: "idle" } });
  });

  it("挂载时不在翻转点上，不失效", () => {
    const { invalidate } = setup();
    expect(invalidate).not.toHaveBeenCalled();
  });

  it("只在一轮结束时失效一次，重渲染不算", () => {
    const { invalidate, rerender } = setup();

    act(() => {
      useConnectionStore.setState({ chat: { kind: "running" } });
    });
    expect(invalidate).not.toHaveBeenCalled();

    rerender(1);
    expect(invalidate).not.toHaveBeenCalled();

    act(() => {
      useConnectionStore.setState({ chat: { kind: "idle" } });
    });
    expect(invalidate).toHaveBeenCalledTimes(1);
    expect(invalidate).toHaveBeenCalledWith({ queryKey: ["space-files"] });

    rerender(2);
    expect(invalidate).toHaveBeenCalledTimes(1);
  });
});
