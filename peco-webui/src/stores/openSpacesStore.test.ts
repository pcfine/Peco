import { beforeEach, describe, expect, it, vi } from "vitest";
import { useOpenSpacesStore } from "@/stores/openSpacesStore";
import { useSpaceStore } from "@/stores/spaceStore";

const STORAGE_KEY = "peco-webui:open-spaces";

beforeEach(() => {
  localStorage.clear();
  useOpenSpacesStore.setState({ openIds: [], activeWsId: null, seeded: false });
  useSpaceStore.setState({ currentWsId: null });
});

describe("打开集", () => {
  it("按加入顺序累积，重复打开不产生第二行", () => {
    const { open } = useOpenSpacesStore.getState();
    open("ws_a");
    open("ws_b");
    open("ws_a");

    expect(useOpenSpacesStore.getState().openIds).toEqual(["ws_a", "ws_b"]);
    expect(useOpenSpacesStore.getState().activeWsId).toBe("ws_a");
  });

  it("加入打开集同时成为请求归属的空间", () => {
    useOpenSpacesStore.getState().open("ws_a");

    expect(useSpaceStore.getState().currentWsId).toBe("ws_a");
  });

  it("移除只动集合，不发任何请求", () => {
    const fetchSpy = vi.spyOn(globalThis, "fetch");
    const { open, close } = useOpenSpacesStore.getState();
    open("ws_a");
    open("ws_b");
    close("ws_a");

    expect(useOpenSpacesStore.getState().openIds).toEqual(["ws_b"]);
    expect(fetchSpy).not.toHaveBeenCalled();
    fetchSpy.mockRestore();
  });

  it("移除激活行后不再有激活项，但请求归属不被改动", () => {
    const { open, close } = useOpenSpacesStore.getState();
    open("ws_a");
    close("ws_a");

    expect(useOpenSpacesStore.getState().activeWsId).toBeNull();
    expect(useSpaceStore.getState().currentWsId).toBe("ws_a");
  });

  it("其它入口改当前空间时，激活态跟着走", () => {
    useOpenSpacesStore.getState().open("ws_a");
    useSpaceStore.getState().setCurrent("ws_b");

    expect(useOpenSpacesStore.getState().activeWsId).toBe("ws_b");
  });

  it("落盘：openIds / activeWsId / seeded 都进 localStorage", () => {
    useOpenSpacesStore.getState().open("ws_a");

    const raw = localStorage.getItem(STORAGE_KEY);
    expect(raw).not.toBeNull();
    expect(JSON.parse(raw as string).state).toMatchObject({
      openIds: ["ws_a"],
      activeWsId: "ws_a",
      seeded: true,
    });
  });

  it("从存档恢复打开集", async () => {
    localStorage.setItem(
      STORAGE_KEY,
      JSON.stringify({
        state: { openIds: ["ws_x", "ws_y"], activeWsId: "ws_y", seeded: true },
        version: 0,
      }),
    );
    await useOpenSpacesStore.persist.rehydrate();

    expect(useOpenSpacesStore.getState().openIds).toEqual(["ws_x", "ws_y"]);
    expect(useOpenSpacesStore.getState().activeWsId).toBe("ws_y");
  });

  it("播种只发生一次：之后再调不会把移除过的行加回来", () => {
    useOpenSpacesStore.getState().seedOnce("ws_a");
    useOpenSpacesStore.getState().close("ws_a");
    useOpenSpacesStore.getState().seedOnce("ws_a");

    expect(useOpenSpacesStore.getState().openIds).toEqual([]);
    expect(useOpenSpacesStore.getState().seeded).toBe(true);
  });
});
