// provider / model 两个下拉：候选表沿用 `lib/constants`，agent.md 里存的未知值
// 补一项进去 —— 否则下拉显示空白，一保存就把那个值静默丢了。

import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { MODELS, PROVIDERS } from "@/lib/constants";

interface AgentModelPickerProps {
  provider: string | null;
  model: string | null;
  onProviderChange: (provider: string) => void;
  onModelChange: (model: string) => void;
  disabled?: boolean;
}

export function AgentModelPicker({
  provider,
  model,
  onProviderChange,
  onModelChange,
  disabled,
}: AgentModelPickerProps) {
  const options: { value: string; label: string }[] = [...MODELS];
  if (model && !MODELS.some((option) => option.value === model)) {
    options.push({ value: model, label: `${model}（自定义）` });
  }

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
            {PROVIDERS.map((option) => (
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
            {options.map((option) => (
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
