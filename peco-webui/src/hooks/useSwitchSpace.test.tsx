import { describe, expect, it } from "vitest";
import { fireEvent, render, screen } from "@testing-library/react";
import {
  MemoryRouter,
  Route,
  Routes,
  useLocation,
  useParams,
} from "react-router-dom";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { useSwitchSpace } from "@/hooks/useSwitchSpace";
import { useSpaceStore } from "@/stores/spaceStore";

function Probe() {
  const { wsId } = useParams();
  const location = useLocation();
  const switchSpace = useSwitchSpace();
  return (
    <>
      <span data-testid="route">{`${wsId}|${location.pathname}${location.search}`}</span>
      <button type="button" onClick={() => switchSpace("ws_b")}>
        切到 ws_b
      </button>
    </>
  );
}

function renderAt(path: string) {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  });
  return render(
    <QueryClientProvider client={client}>
      <MemoryRouter initialEntries={[path]}>
        <Routes>
          <Route path="/spaces/:wsId" element={<Probe />} />
          <Route path="/spaces" element={<Probe />} />
          <Route path="/spaces/new" element={<Probe />} />
        </Routes>
      </MemoryRouter>
    </QueryClientProvider>,
  );
}

describe("切换当前空间", () => {
  it("详情页切空间后跟到新 ws，并保留同一 tab", () => {
    useSpaceStore.setState({ currentWsId: "ws_a" });
    renderAt("/spaces/ws_a?tab=tasks");

    fireEvent.click(screen.getByRole("button", { name: "切到 ws_b" }));

    expect(screen.getByTestId("route")).toHaveTextContent("ws_b|/spaces/ws_b");
    expect(screen.getByTestId("route")).toHaveTextContent("tab=tasks");
    expect(useSpaceStore.getState().currentWsId).toBe("ws_b");
  });

  it("列表页切空间不跳路由", () => {
    useSpaceStore.setState({ currentWsId: "ws_a" });
    renderAt("/spaces");

    fireEvent.click(screen.getByRole("button", { name: "切到 ws_b" }));

    expect(screen.getByTestId("route")).toHaveTextContent("|/spaces");
    expect(useSpaceStore.getState().currentWsId).toBe("ws_b");
  });
});
