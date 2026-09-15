// MemoryConsolidationCard 组件测试 —— opt-in 回显 / 乐观更新 / 失败回滚 / 状态展示

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { MemoryConsolidationCard } from "../MemoryConsolidationCard";
import {
  getConsolidationOptin,
  getConsolidationState,
  setConsolidationOptin,
} from "@/api/peco";
import { toast } from "sonner";

vi.mock("@/api/peco", () => ({
  getConsolidationOptin: vi.fn(),
  setConsolidationOptin: vi.fn(),
  getConsolidationState: vi.fn(),
}));

vi.mock("sonner", () => ({
  toast: { error: vi.fn(), success: vi.fn() },
}));

// jsdom 未实现 ResizeObserver，radix Switch 的 useSize 直接引用它。
class ResizeObserverStub {
  observe() {}
  unobserve() {}
  disconnect() {}
}

const OPTIN = vi.mocked(getConsolidationOptin);
const SET_OPTIN = vi.mocked(setConsolidationOptin);
const STATE = vi.mocked(getConsolidationState);

const FULL_STATS = {
  scanned: 120,
  candidates: 30,
  clustered_groups: 4,
  merged: 3,
  dedup_deleted: 2,
  ttl_deleted: 1,
  audit_purged: 0,
  llm_calls: 5,
  machine_steps_skipped: null,
};

let container: HTMLDivElement;
let root: Root;

/** 挂载卡片并等待首屏两个 GET 落地。 */
async function renderCard() {
  await act(async () => {
    root.render(<MemoryConsolidationCard />);
  });
  await flush();
}

/** 让已 resolve 的 promise 链跑完。 */
async function flush() {
  await act(async () => {
    await new Promise((resolve) => setTimeout(resolve, 0));
  });
}

function switchEl(): HTMLButtonElement {
  const el = container.querySelector('[role="switch"]');
  if (!el) throw new Error("switch not found");
  return el as HTMLButtonElement;
}

/** 按统计项标签取同一格内的数值文本。 */
function statValue(label: string): string {
  const labelEl = Array.from(container.querySelectorAll("p")).find(
    (el) => el.textContent === label && el.className.includes("text-xs"),
  );
  if (!labelEl) throw new Error(`stat label not found: ${label}`);
  return labelEl.parentElement?.lastElementChild?.textContent ?? "";
}

beforeEach(() => {
  (
    globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT: boolean }
  ).IS_REACT_ACT_ENVIRONMENT = true;
  globalThis.ResizeObserver ??=
    ResizeObserverStub as unknown as typeof ResizeObserver;

  OPTIN.mockReset().mockResolvedValue({ enabled: false });
  SET_OPTIN.mockReset();
  STATE.mockReset().mockResolvedValue({
    last_run_at: null,
    last_run_stats: null,
  });
  vi.mocked(toast.error).mockReset();

  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
});

afterEach(() => {
  act(() => root.unmount());
  container.remove();
});

describe("MemoryConsolidationCard", () => {
  it("挂载时按后端返回回显开关状态", async () => {
    OPTIN.mockResolvedValue({ enabled: true });

    await renderCard();

    expect(OPTIN).toHaveBeenCalledTimes(1);
    expect(switchEl()).toHaveAttribute("aria-checked", "true");
    expect(switchEl()).not.toBeDisabled();
  });

  it("切换开关时以目标值 PUT 并乐观置位", async () => {
    OPTIN.mockResolvedValue({ enabled: true });
    SET_OPTIN.mockResolvedValueOnce({ enabled: false }).mockResolvedValueOnce({
      enabled: true,
    });

    await renderCard();

    // 关：载荷 false，开关立即跟随
    await act(async () => {
      switchEl().click();
    });
    expect(SET_OPTIN).toHaveBeenLastCalledWith(false);
    expect(switchEl()).toHaveAttribute("aria-checked", "false");

    await flush();

    // 开：载荷 true
    await act(async () => {
      switchEl().click();
    });
    expect(SET_OPTIN).toHaveBeenLastCalledWith(true);
    expect(switchEl()).toHaveAttribute("aria-checked", "true");

    await flush();
    expect(SET_OPTIN).toHaveBeenCalledTimes(2);
    expect(switchEl()).toHaveAttribute("aria-checked", "true");
  });

  it("PUT 进行中禁用开关，防连点", async () => {
    let resolvePut: (v: { enabled: boolean }) => void = () => {};
    SET_OPTIN.mockReturnValue(
      new Promise((resolve) => {
        resolvePut = resolve;
      }),
    );

    await renderCard();
    await act(async () => {
      switchEl().click();
    });

    expect(switchEl()).toBeDisabled();

    await act(async () => {
      resolvePut({ enabled: true });
    });
    await flush();

    expect(switchEl()).not.toBeDisabled();
    expect(switchEl()).toHaveAttribute("aria-checked", "true");
  });

  it("PUT 失败时回滚开关并提示错误", async () => {
    SET_OPTIN.mockRejectedValue(new Error("boom"));

    await renderCard();
    await act(async () => {
      switchEl().click();
    });
    await flush();

    expect(switchEl()).toHaveAttribute("aria-checked", "false");
    expect(vi.mocked(toast.error)).toHaveBeenCalledTimes(1);
  });

  it("state 为 null 时显示「从未运行」与空统计", async () => {
    await renderCard();

    expect(container.textContent).toContain("从未运行");
    expect(container.textContent).toContain("上次统计");
    expect(container.textContent).toContain("—");
  });

  it("有统计时逐项展示各字段", async () => {
    STATE.mockResolvedValue({
      last_run_at: "2026-09-15T10:30:00Z",
      last_run_stats: FULL_STATS,
    });

    await renderCard();

    expect(container.textContent).not.toContain("从未运行");
    expect(container.textContent).toContain("上次整理");
    expect(statValue("扫描")).toBe("120");
    expect(statValue("候选")).toBe("30");
    expect(statValue("聚类组")).toBe("4");
    expect(statValue("沉淀")).toBe("3");
    expect(statValue("去重删除")).toBe("2");
    expect(statValue("TTL 删除")).toBe("1");
    expect(statValue("审计清理")).toBe("0");
    expect(statValue("模型调用")).toBe("5");
  });

  it("单字段缺失时该格显示「—」并附机器判定跳过原因", async () => {
    STATE.mockResolvedValue({
      last_run_at: "2026-09-15T10:30:00Z",
      last_run_stats: {
        ...FULL_STATS,
        merged: null,
        machine_steps_skipped: "嵌入模型不可用",
      },
    });

    await renderCard();

    expect(statValue("沉淀")).toBe("—");
    expect(container.textContent).toContain("嵌入模型不可用");
  });

  it("状态读取失败时不谎报「从未运行」", async () => {
    STATE.mockRejectedValue(new Error("boom"));

    await renderCard();

    expect(container.textContent).toContain("状态读取失败");
    expect(container.textContent).not.toContain("从未运行");
  });
});
