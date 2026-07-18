import { useForm } from 'react-hook-form'
import { zodResolver } from '@hookform/resolvers/zod'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { Textarea } from '@/components/ui/textarea'
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@/components/ui/select'
import { Slider } from '@/components/ui/slider'
import { Card, CardContent } from '@/components/ui/card'
import { agentSchema } from '@/lib/validators'
import { MODELS, PROVIDERS, TOOLS } from '@/lib/constants'
import type { AgentDetail, CreateAgentRequest } from '@/types/agent'

interface Props {
  defaultValues?: AgentDetail
  onSubmit: (data: CreateAgentRequest) => Promise<void>
}

export function AgentForm({ defaultValues, onSubmit }: Props) {
  const { register, handleSubmit, setValue, watch, formState: { errors } } = useForm({
    resolver: zodResolver(agentSchema),
    defaultValues: {
      name: defaultValues?.name ?? '',
      description: defaultValues?.description ?? '',
      system_prompt: defaultValues?.system_prompt ?? '',
      model: defaultValues?.model ?? 'deepseek-v4-flash',
      provider: defaultValues?.provider ?? 'deepseek',
      icon: defaultValues?.icon ?? '🤖',
      color: defaultValues?.color ?? '#6366f1',
      temperature: defaultValues?.temperature,
      max_tokens: defaultValues?.max_tokens,
    },
  })

  const icon = watch('icon')
  const color = watch('color')
  const name = watch('name')

  const handleFormSubmit = async (data: Record<string, unknown>) => {
    await onSubmit({
      name: data.name as string,
      description: data.description as string,
      system_prompt: data.system_prompt as string,
      model: data.model as string,
      provider: data.provider as string,
      icon: data.icon as string,
      color: data.color as string,
      temperature: data.temperature as number | undefined,
      max_tokens: data.max_tokens as number | undefined,
    })
  }

  return (
    <form onSubmit={handleSubmit(handleFormSubmit)} className="space-y-6">
      {/* Preview card */}
      <Card>
        <CardContent className="flex items-center gap-4 p-4">
          <div className="flex h-14 w-14 items-center justify-center rounded-lg text-2xl" style={{ background: (color || '#6366f1') + '20' }}>
            {icon || '🤖'}
          </div>
          <div>
            <p className="font-semibold text-lg">{name || 'Agent 名称'}</p>
            <p className="text-sm text-muted-foreground">预览效果</p>
          </div>
        </CardContent>
      </Card>

      <div className="grid gap-4 md:grid-cols-2">
        <div className="space-y-2">
          <Label htmlFor="name">名称 *</Label>
          <Input id="name" {...register('name')} />
          {errors.name && <p className="text-sm text-destructive">{errors.name.message}</p>}
        </div>
        <div className="space-y-2 md:col-span-2">
          <Label htmlFor="desc">描述</Label>
          <Input id="desc" {...register('description')} placeholder="简要描述 Agent 的用途" />
        </div>
      </div>

      <div className="space-y-2">
        <Label htmlFor="prompt">System Prompt</Label>
        <Textarea id="prompt" rows={8} className="font-mono" {...register('system_prompt')} />
      </div>

      <div className="grid gap-4 md:grid-cols-2">
        <div className="space-y-2">
          <Label>模型</Label>
          <Select defaultValue={defaultValues?.model ?? 'deepseek-v4-flash'} onValueChange={(v) => setValue('model', v)}>
            <SelectTrigger><SelectValue /></SelectTrigger>
            <SelectContent>
              {MODELS.map((m) => <SelectItem key={m.value} value={m.value}>{m.label}</SelectItem>)}
            </SelectContent>
          </Select>
        </div>
        <div className="space-y-2">
          <Label>Provider</Label>
          <Select defaultValue={defaultValues?.provider ?? 'deepseek'} onValueChange={(v) => setValue('provider', v)}>
            <SelectTrigger><SelectValue /></SelectTrigger>
            <SelectContent>
              {PROVIDERS.map((p) => <SelectItem key={p.value} value={p.value}>{p.label}</SelectItem>)}
            </SelectContent>
          </Select>
        </div>
      </div>

      <div className="grid gap-4 md:grid-cols-2">
        <div className="space-y-2">
          <Label>图标 (emoji)</Label>
          <Input {...register('icon')} />
        </div>
        <div className="space-y-2">
          <Label>主题色</Label>
          <div className="flex gap-2 items-center">
            <Input type="color" className="w-14 h-10" {...register('color')} />
            <Input {...register('color')} />
          </div>
        </div>
      </div>

      <div className="space-y-2">
        <Label>工具选择</Label>
        <div className="flex flex-wrap gap-2">
          {TOOLS.map((t) => (
            <label key={t.value} className="flex items-center gap-1 rounded-md border px-3 py-1 text-sm cursor-pointer hover:bg-accent">
              <input type="checkbox" defaultChecked={defaultValues?.tools?.includes(t.value)} className="h-3 w-3" />
              {t.label}
            </label>
          ))}
        </div>
      </div>

      <div className="grid gap-4 md:grid-cols-2">
        <div className="space-y-2">
          <Label>Temperature ({watch('temperature') ?? 0.7})</Label>
          <Slider min={0} max={2} step={0.1} defaultValue={[defaultValues?.temperature ?? 0.7]}
            onValueChange={([v]) => setValue('temperature', v)} />
        </div>
        <div className="space-y-2">
          <Label htmlFor="maxTokens">Max Tokens</Label>
          <Input id="maxTokens" type="number" {...register('max_tokens', { valueAsNumber: true })} />
        </div>
      </div>

      <Button type="submit" className="w-full">{defaultValues ? '保存修改' : '创建 Agent'}</Button>
    </form>
  )
}
