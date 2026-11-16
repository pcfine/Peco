import { api } from "@/lib/api";

/** 上传用途：服务端按它选落盘目录，路径不经过调用方。 */
export type UploadPurpose = "teams" | "brand";

/** 上传图片，返回 `/uploads/...` URL。 */
export async function uploadImage(
  blob: Blob,
  purpose: UploadPurpose = "teams",
): Promise<string> {
  const form = new FormData();
  form.append("file", blob, "icon.png");
  form.append("purpose", purpose);
  const res = await api.post<{ url: string }>("/upload", form);
  return res.data.url;
}
