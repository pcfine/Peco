import { api } from "@/lib/api";

/** 左栏品牌图片；`image` 为空串表示未设置，用默认标记。 */
export interface BrandConfig {
  image: string;
}

export async function getBrand(): Promise<BrandConfig> {
  const res = await api.get<BrandConfig>("/brand");
  return res.data;
}

/** 空串 = 恢复默认。 */
export async function setBrand(image: string): Promise<BrandConfig> {
  const res = await api.put<BrandConfig>("/brand", { image });
  return res.data;
}
