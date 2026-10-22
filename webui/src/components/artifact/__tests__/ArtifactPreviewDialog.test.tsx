// 产物查看器：只有回应了预期媒体类型才进播放器
//
// Dialog 走 portal 挂到 body，断言必须在 `document` 上查 —— container 里看不到它。

import {
  afterEach,
  beforeAll,
  beforeEach,
  describe,
  expect,
  it,
  vi,
} from "vitest";
import { act, createRef } from "react";
import { createRoot, type Root } from "react-dom/client";
import api from "@/api/client";
import { releaseAll } from "@/api/artifactBlob";
import type { ArtifactItem } from "@/types/artifact";
import { ArtifactPreviewDialog } from "../ArtifactPreviewDialog";

const getSpy = vi.spyOn(api, "get");

function item(overrides: Partial<ArtifactItem> = {}): ArtifactItem {
  return {
    id: "a1",
    conversation_id: "c1",
    kind: "document",
    mime: "application/pdf",
    title: "报告.pdf",
    size_bytes: 1024,
    url: "/api/artifacts/a1/content",
    download_url: "/api/artifacts/a1/download",
    created_at: "2026-10-20 12:00:00",
    available: true,
    ...overrides,
  };
}

let container: HTMLDivElement;
let root: Root;

async function flush() {
  await act(async () => {
    await new Promise((resolve) => setTimeout(resolve, 0));
    await new Promise((resolve) => setTimeout(resolve, 0));
  });
}

const triggerElRef = createRef<HTMLButtonElement>();
const fallbackFocusRef = createRef<HTMLDivElement>();

async function render(target: ArtifactItem) {
  await act(async () => {
    root.render(
      <ArtifactPreviewDialog
        item={target}
        triggerElRef={triggerElRef}
        fallbackFocusRef={fallbackFocusRef}
        onClose={() => {}}
      />,
    );
  });
  await flush();
}

function dialog(): Element | null {
  return document.querySelector('[data-slot="dialog-content"]');
}

beforeAll(() => {
  URL.createObjectURL = vi.fn(() => "blob:mock-dialog");
  URL.revokeObjectURL = vi.fn();
});

beforeEach(() => {
  (
    globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT: boolean }
  ).IS_REACT_ACT_ENVIRONMENT = true;
  releaseAll();
  getSpy.mockReset();
  getSpy.mockResolvedValue({
    data: { size: 1024, type: "application/pdf" } as unknown as Blob,
    headers: { "content-type": "application/pdf" },
  } as never);

  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
});

afterEach(async () => {
  await act(async () => root.unmount());
  container.remove();
  releaseAll();
});

describe("ArtifactPreviewDialog 的媒体类型兜底", () => {
  it("文档：拿到字节也不进播放器，显示无法预览", async () => {
    await render(item());

    expect(dialog()).not.toBeNull();
    // 变异点：只用 status/url 判"可播放"（去掉媒体类型判定）→ 文档字节落进 <audio>
    expect(dialog()?.querySelector("audio, video, img")).toBeNull();
    expect(dialog()?.textContent).toContain("无法预览");
  });

  it("图片：响应类型命中时正常内联", async () => {
    getSpy.mockResolvedValue({
      data: { size: 1024, type: "image/png" } as unknown as Blob,
      headers: { "content-type": "image/png" },
    } as never);
    await render(item({ kind: "image", mime: "image/png", title: "照片.png" }));

    expect(dialog()?.querySelector("img")).not.toBeNull();
    expect(dialog()?.textContent).not.toContain("无法预览");
  });
});
