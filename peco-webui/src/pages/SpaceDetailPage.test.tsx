import { describe, expect, it, vi } from "vitest";
import { render, screen } from "@testing-library/react";
import { MemoryRouter, Route, Routes } from "react-router-dom";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { SpaceDetailPage } from "@/pages/SpaceDetailPage";

// 聊天页本阶段只有页头与占位：台账、产物、文件都在 `/spaces/:wsId/status`。
vi.mock("@/api/spaces", () => ({
  getSpace: async () => ({
    ws_id: "ws_a",
    work_dir: "/tmp/spaces/a/work",
    team_id: "research",
    label: "空间 A",
    exit_policy: "inherit",
    effective_exit_policy: "keep",
    status: "open",
    closed_at: null,
    last_opened_at: "2026-11-05 10:00:00",
    reserved: false,
    inflight_dispatch: 0,
    inflight_foreign: 0,
    marker: null,
  }),
  closeSpace: async () => ({ stopped: true }),
}));

function renderDetail() {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  });
  return render(
    <QueryClientProvider client={client}>
      <MemoryRouter initialEntries={["/spaces/ws_a"]}>
        <Routes>
          <Route path="/spaces/:wsId" element={<SpaceDetailPage />} />
        </Routes>
      </MemoryRouter>
    </QueryClientProvider>,
  );
}

describe("空间聊天页占位", () => {
  it("页头显示空间名，正文指向状态页而不是死路", async () => {
    renderDetail();

    expect(await screen.findByText("空间 A")).toBeInTheDocument();
    expect(screen.getByText(/下一阶段接到这里/)).toBeInTheDocument();
    expect(
      screen.getByRole("link", { name: "查看空间状态" }),
    ).toHaveAttribute("href", "/spaces/ws_a/status");
  });
});
