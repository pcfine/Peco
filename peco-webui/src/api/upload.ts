import { api } from "@/lib/api";

/** 上传图片，返回 `/uploads/...` URL。 */
export async function uploadImage(blob: Blob): Promise<string> {
  const form = new FormData();
  form.append("file", blob, "icon.png");
  const res = await api.post<{ url: string }>("/upload", form);
  return res.data.url;
}
