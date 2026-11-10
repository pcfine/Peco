// provider / model 两个下拉：候选来自这个团队的生效 provider 配置（含从 Peco 与
// 系统层继承来的条目）。agent.md 里存的未知值补一项进去 —— 否则下拉显示空白，
// 一保存就把那个值静默丢了。
//
// 配置读不到时回退到 `lib/constants` 的常量表：下拉不能因为一次失败就变成空的。

import { useQuery } from "@tanstack/react-query";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import {
  listProviderTypes,
  listProviders,
  type ProviderInfo,
  type ProviderTypeInfo,
} from "@/api/providers";
import { MODELS, PROVIDERS } from "@/lib/constants";

interface PickerOption {
  value: string;
  label: string;
}

/** 库里存着但候选里没有的值补一格，避免下拉一片空白。 */
export function withCurrent(
  options: PickerOption[],
  current: string | null,
): PickerOption[] {
  if (!current || options.some((option) => option.value === current)) {
    return options;
  }
  return [...options, { value: current, label: `${current}（自定义）` }];
}

/** provider 候选 = 生效配置里的名字；没有配置时回退常量表。 */
export function providerOptions(
  configured: ProviderInfo[],
  current: string | null,
): PickerOption[] {
  const options =
    configured.length > 0
      ? configured.map((provider) => ({
          value: provider.name,
          label: provider.name,
        }))
      : PROVIDERS.map((provider) => ({
          value: provider.value,
          label: provider.label,
        }));
  return withCurrent(options, current);
}

/**
 * 某个 provider 的模型候选：它的 `default_model` 打头，然后是类型目录里的常用模型。
 *
 * 键是 provider 名字而不是类型 —— 同一个类型可以有多个别名，各自的默认模型不同。
 */
export function modelNames(
  provider: ProviderInfo | undefined,
  types: ProviderTypeInfo[],
): string[] {
  if (!provider) return [];
  const meta = types.find(
    (type) => type.provider_type === provider.provider_type,
  );
  const seen = new Set<string>();
  const names: string[] = [];
  for (const candidate of [
    provider.default_model,
    ...(meta?.suggested_models ?? []),
  ]) {
    const name = candidate?.trim();
    if (!name || seen.has(name)) continue;
    seen.add(name);
    names.push(name);
  }
  return names;
}

interface AgentModelPickerProps {
  /** provider 配置挂在团队能力根下，与「当前空间」无关。 */
  teamId: string;
  provider: string | null;
  model: string | null;
  onProviderChange: (provider: string) => void;
  onModelChange: (model: string) => void;
  disabled?: boolean;
}

export function AgentModelPicker({
  teamId,
  provider,
  model,
  onProviderChange,
  onModelChange,
  disabled,
}: AgentModelPickerProps) {
  const providers = useQuery({
    queryKey: ["providers", teamId],
    queryFn: () => listProviders(teamId),
  });
  // 目录是一份编译期常量，按团队维度缓存，别为它反复打网络。
  const types = useQuery({
    queryKey: ["provider-types", teamId],
    queryFn: () => listProviderTypes(teamId),
    staleTime: 5 * 60_000,
  });

  const configured = providers.data ?? [];
  const selected = configured.find((item) => item.name === provider);
  const names =
    configured.length > 0 ? modelNames(selected, types.data ?? []) : [];
  const models: PickerOption[] =
    names.length > 0
      ? names.map((name) => ({ value: name, label: name }))
      : MODELS.map((entry) => ({ value: entry.value, label: entry.label }));

  return (
    <>
      <div className="space-y-1">
        <span className="text-2xs text-ink-faint">provider</span>
        <Select
          value={provider ?? undefined}
          onValueChange={onProviderChange}
          disabled={disabled}
        >
          <SelectTrigger className="h-8 w-full" aria-label="provider">
            <SelectValue placeholder="默认" />
          </SelectTrigger>
          <SelectContent>
            {providerOptions(configured, provider).map((option) => (
              <SelectItem key={option.value} value={option.value}>
                {option.label}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
      </div>
      <div className="space-y-1">
        <span className="text-2xs text-ink-faint">model</span>
        <Select
          value={model ?? undefined}
          onValueChange={onModelChange}
          disabled={disabled}
        >
          <SelectTrigger className="h-8 w-full" aria-label="model">
            <SelectValue placeholder="默认" />
          </SelectTrigger>
          <SelectContent>
            {withCurrent(models, model).map((option) => (
              <SelectItem key={option.value} value={option.value}>
                {option.label}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
      </div>
    </>
  );
}
