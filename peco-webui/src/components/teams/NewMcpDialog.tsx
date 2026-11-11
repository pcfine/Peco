// 新建 MCP server：字段与 webui 的「添加 MCP 服务器」弹窗同构 —— stdio / sse /
// streamable_http 三种形态差异大，表单比裸 JSON 更适合新增。提交时只往现有配置里
// 加一项，其余 server 与顶层其它键原样带回去（PUT 是整份替换）。

import { useState } from "react";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { toast } from "sonner";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { Switch } from "@/components/ui/switch";
import { Textarea } from "@/components/ui/textarea";
import { updateMcpConfig, type McpConfig } from "@/api/mcp";
import { errorText } from "@/lib/api";

type Transport = "stdio" | "sse" | "streamable_http";

const TRANSPORT_LABELS: Record<Transport, string> = {
  stdio: "stdio（本地进程）",
  sse: "sse（远程 SSE）",
  streamable_http: "streamable_http（远程 HTTP）",
};

/** 表单里 args / env / headers 都是多行文本，提交时再拆成结构。 */
interface FormState {
  name: string;
  transport: Transport;
  enabled: boolean;
  command: string;
  argsText: string;
  envText: string;
  url: string;
  headersText: string;
  timeoutSecs: number;
  maxRetries: number;
}

const EMPTY_FORM: FormState = {
  name: "",
  transport: "stdio",
  enabled: true,
  command: "",
  argsText: "",
  envText: "",
  url: "",
  headersText: "",
  timeoutSecs: 30,
  maxRetries: 3,
};

/** 去掉值两侧成对的引号（`KEY="v"` 与环境变量文件的写法保持一致）。 */
function unquote(value: string): string {
  const quote = value[0];
  if (
    (quote === '"' || quote === "'") &&
    value.endsWith(quote) &&
    !value.slice(1, -1).includes(quote)
  ) {
    return value.slice(1, -1);
  }
  return value;
}

function parseLines(text: string): string[] {
  return text
    .split("\n")
    .map((line) => line.trim())
    .filter(Boolean);
}

function parsePairs(
  text: string,
  separator: "=" | ":",
): Record<string, string> {
  const pairs: Record<string, string> = {};
  for (const line of parseLines(text)) {
    const at = line.indexOf(separator);
    if (at > 0) {
      pairs[line.slice(0, at).trim()] = unquote(line.slice(at + 1).trim());
    }
  }
  return pairs;
}

function toServerConfig(form: FormState): Record<string, unknown> {
  const stdio = form.transport === "stdio";
  return {
    transport: form.transport,
    enabled: form.enabled,
    ...(stdio
      ? {
          command: form.command.trim(),
          args: parseLines(form.argsText),
          env: parsePairs(form.envText, "="),
        }
      : {
          url: form.url.trim(),
          headers: parsePairs(form.headersText, ":"),
        }),
    timeoutSecs: form.timeoutSecs,
    maxRetries: form.maxRetries,
  };
}

interface NewMcpFormProps {
  wsId: string;
  config: McpConfig;
  onCancel: () => void;
  onCreated: () => void;
}

function NewMcpForm({ wsId, config, onCancel, onCreated }: NewMcpFormProps) {
  const queryClient = useQueryClient();
  const [form, setForm] = useState<FormState>(EMPTY_FORM);

  const set = <K extends keyof FormState>(key: K, value: FormState[K]) =>
    setForm((prev) => ({ ...prev, [key]: value }));

  const name = form.name.trim();
  const nameError =
    name === ""
      ? null
      : config.mcpServers[name] !== undefined
        ? "该名称已存在"
        : null;
  const requiredMissing =
    form.transport === "stdio"
      ? form.command.trim() === ""
      : form.url.trim() === "";

  const create = useMutation({
    mutationFn: (server: Record<string, unknown>) =>
      updateMcpConfig(wsId, {
        ...config,
        mcpServers: { ...config.mcpServers, [name]: server },
      }),
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: ["mcp", wsId] });
      // agent 编辑器的 MCP 候选也读这份配置，扇出刷新一次。
      void queryClient.invalidateQueries({ queryKey: ["team"] });
      toast.success(`已添加 MCP server「${name}」`);
      onCreated();
    },
    onError: (error) => toast.error(errorText(error)),
  });

  return (
    <form
      className="space-y-3"
      onSubmit={(event) => {
        event.preventDefault();
        create.mutate(toServerConfig(form));
      }}
    >
      <label className="block space-y-1">
        <span className="text-caption font-medium text-ink-faint">
          名称（必填）
        </span>
        <Input
          value={form.name}
          onChange={(event) => set("name", event.target.value)}
          placeholder="my-mcp-server"
          disabled={create.isPending}
          autoComplete="off"
          spellCheck={false}
          className="h-8 font-mono text-xs"
        />
      </label>
      {nameError && (
        <p className="text-caption font-medium text-danger">{nameError}</p>
      )}

      <div className="space-y-1">
        <span className="text-caption font-medium text-ink-faint">
          传输类型
        </span>
        <Select
          value={form.transport}
          onValueChange={(value) => {
            const transport = value as Transport;
            // 切换形态时清掉另一侧的字段，免得旧值跟着提交。
            setForm((prev) => ({
              ...prev,
              transport,
              ...(transport === "stdio"
                ? { url: "", headersText: "" }
                : { command: "", argsText: "", envText: "" }),
            }));
          }}
        >
          <SelectTrigger className="h-8 w-full" aria-label="传输类型">
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            {(Object.keys(TRANSPORT_LABELS) as Transport[]).map((transport) => (
              <SelectItem key={transport} value={transport}>
                {TRANSPORT_LABELS[transport]}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
      </div>

      <div className="flex items-center gap-2">
        <Switch
          id="new-mcp-enabled"
          checked={form.enabled}
          onCheckedChange={(checked) => set("enabled", checked)}
          size="sm"
        />
        <label
          htmlFor="new-mcp-enabled"
          className="text-caption font-medium text-ink-faint"
        >
          启用
        </label>
      </div>

      {form.transport === "stdio" ? (
        <>
          <label className="block space-y-1">
            <span className="text-caption font-medium text-ink-faint">
              命令（必填）
            </span>
            <Input
              value={form.command}
              onChange={(event) => set("command", event.target.value)}
              placeholder="npx 或可执行文件路径"
              disabled={create.isPending}
              autoComplete="off"
              spellCheck={false}
              className="h-8 font-mono text-xs"
            />
          </label>
          <label className="block space-y-1">
            <span className="text-caption font-medium text-ink-faint">
              参数（每行一个）
            </span>
            <Textarea
              value={form.argsText}
              onChange={(event) => set("argsText", event.target.value)}
              placeholder={"-y\n@scope/server\n--port=8080"}
              disabled={create.isPending}
              spellCheck={false}
              className="min-h-[64px] font-mono text-mono-sm"
            />
          </label>
          <label className="block space-y-1">
            <span className="text-caption font-medium text-ink-faint">
              环境变量（KEY=VALUE，每行一个）
            </span>
            <Textarea
              value={form.envText}
              onChange={(event) => set("envText", event.target.value)}
              placeholder={"NODE_ENV=production\nDEBUG=true"}
              disabled={create.isPending}
              spellCheck={false}
              className="min-h-[64px] font-mono text-mono-sm"
            />
          </label>
        </>
      ) : (
        <>
          <label className="block space-y-1">
            <span className="text-caption font-medium text-ink-faint">
              URL（必填）
            </span>
            <Input
              value={form.url}
              onChange={(event) => set("url", event.target.value)}
              placeholder="http://localhost:8000/mcp"
              disabled={create.isPending}
              autoComplete="off"
              spellCheck={false}
              className="h-8 font-mono text-xs"
            />
          </label>
          <label className="block space-y-1">
            <span className="text-caption font-medium text-ink-faint">
              Headers（KEY: VALUE，每行一个）
            </span>
            <Textarea
              value={form.headersText}
              onChange={(event) => set("headersText", event.target.value)}
              placeholder={"Authorization: Bearer token\nX-API-Key: abc123"}
              disabled={create.isPending}
              spellCheck={false}
              className="min-h-[64px] font-mono text-mono-sm"
            />
          </label>
        </>
      )}

      <div className="grid grid-cols-2 gap-3">
        <label className="block space-y-1">
          <span className="text-caption font-medium text-ink-faint">
            超时（秒）
          </span>
          <Input
            type="number"
            min={1}
            max={300}
            value={form.timeoutSecs}
            onChange={(event) =>
              set("timeoutSecs", Number(event.target.value) || 30)
            }
            disabled={create.isPending}
            className="h-8 font-mono text-xs"
          />
        </label>
        <label className="block space-y-1">
          <span className="text-caption font-medium text-ink-faint">
            最大重试次数
          </span>
          <Input
            type="number"
            min={0}
            max={10}
            value={form.maxRetries}
            onChange={(event) =>
              set("maxRetries", Number(event.target.value) || 3)
            }
            disabled={create.isPending}
            className="h-8 font-mono text-xs"
          />
        </label>
      </div>

      <DialogFooter>
        <Button
          type="button"
          variant="ghost"
          size="sm"
          disabled={create.isPending}
          onClick={onCancel}
        >
          取消
        </Button>
        <Button
          type="submit"
          size="sm"
          disabled={
            create.isPending ||
            name === "" ||
            nameError !== null ||
            requiredMissing
          }
        >
          添加
        </Button>
      </DialogFooter>
    </form>
  );
}

interface NewMcpDialogProps {
  wsId: string;
  config: McpConfig;
  open: boolean;
  onOpenChange: (open: boolean) => void;
}

export function NewMcpDialog({
  wsId,
  config,
  open,
  onOpenChange,
}: NewMcpDialogProps) {
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-h-[85vh] overflow-y-auto">
        <DialogHeader>
          <DialogTitle>添加 MCP server</DialogTitle>
          <DialogDescription>
            配置 MCP 服务器连接参数，添加后会立即写入团队的 mcpconfig.json。
          </DialogDescription>
        </DialogHeader>
        {/* 表单状态放在 DialogContent 里：关掉即卸载，下次打开是干净的一份。 */}
        <NewMcpForm
          wsId={wsId}
          config={config}
          onCancel={() => onOpenChange(false)}
          onCreated={() => onOpenChange(false)}
        />
      </DialogContent>
    </Dialog>
  );
}
