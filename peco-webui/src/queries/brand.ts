import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { getBrand, setBrand } from "@/api/brand";

/** 品牌图只有一张、改动只来自设置页，没必要频繁重取。 */
export const brandKey = ["brand"] as const;

export function useBrand() {
  return useQuery({
    queryKey: brandKey,
    queryFn: getBrand,
    staleTime: 60_000,
  });
}

export function useSetBrand() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (image: string) => setBrand(image),
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: brandKey });
    },
  });
}
