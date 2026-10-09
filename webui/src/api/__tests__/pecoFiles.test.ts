// Peco 附件 API 契约测试 —— 表单字段 / 路径 / 流 URL 参数与后端逐字对齐

import { beforeEach, describe, expect, it, vi } from "vitest";
import api from "../client";
import { uploadPecoFile, pecoStreamUrl } from "../peco";

vi.mock("../client", () => ({
  default: { get: vi.fn(), post: vi.fn(), put: vi.fn(), delete: vi.fn() },
}));

const POST = vi.mocked(api.post);

beforeEach(() => {
  POST.mockReset();
});

describe("uploadPecoFile", () => {
  it("POST /peco/files 以 multipart 字段 file 上传并原样返回响应", async () => {
    const payload = {
      id: "u1",
      kind: "document" as const,
      name: "笔记.txt",
      size: 12,
    };
    POST.mockResolvedValue({ data: payload });

    const file = new File(["hello world\n"], "笔记.txt", {
      type: "text/plain",
    });
    await expect(uploadPecoFile(file)).resolves.toEqual(payload);

    expect(POST).toHaveBeenCalledTimes(1);
    const [path, body, config] = POST.mock.calls[0];
    expect(path).toBe("/peco/files");
    expect(body).toBeInstanceOf(FormData);
    expect((body as FormData).get("file")).toBe(file);
    expect(config).toEqual({
      headers: { "Content-Type": "multipart/form-data" },
    });
  });

  it("图片响应带 url，文档响应缺省 url（不进静态目录）", async () => {
    POST.mockResolvedValue({
      data: {
        id: "i1",
        kind: "image" as const,
        name: "a.png",
        size: 3,
        url: "/uploads/chat/u/i1",
      },
    });
    await expect(
      uploadPecoFile(new File(["x"], "a.png")),
    ).resolves.toMatchObject({
      kind: "image",
      url: "/uploads/chat/u/i1",
    });

    POST.mockResolvedValue({
      data: { id: "f1", kind: "document" as const, name: "a.pdf", size: 3 },
    });
    const doc = await uploadPecoFile(new File(["x"], "a.pdf"));
    expect(doc.kind).toBe("document");
    expect(doc.url).toBeUndefined();
  });
});

describe("pecoStreamUrl", () => {
  it("图片 id 走 image_ids、文档 id 走 file_ids", () => {
    expect(pecoStreamUrl("你好")).toBe(
      "/api/peco/stream?message=%E4%BD%A0%E5%A5%BD",
    );
    expect(pecoStreamUrl("看", ["a.png"])).toBe(
      "/api/peco/stream?message=%E7%9C%8B&image_ids=a.png",
    );
    expect(pecoStreamUrl("读", [], ["f1"])).toBe(
      "/api/peco/stream?message=%E8%AF%BB&file_ids=f1",
    );
    expect(pecoStreamUrl("混", ["a.png"], ["f1", "f2"])).toBe(
      "/api/peco/stream?message=%E6%B7%B7&image_ids=a.png&file_ids=f1,f2",
    );
  });
});
