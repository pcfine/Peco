import { useEffect, useRef, useState } from "react";
import { Button } from "@/components/ui/button";
import { Card, CardContent } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Textarea } from "@/components/ui/textarea";
import { Badge } from "@/components/ui/badge";
import { Switch } from "@/components/ui/switch";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { LoadingSpinner } from "@/components/common/LoadingSpinner";
import { EmptyState } from "@/components/common/EmptyState";
import { getMcpConfig, saveMcpConfig, testMcpConnection } from "@/api/mcp";
import type {
  McpServerConfig,
  McpTestErrorType,
  McpTestResult,
  TransportType,
} from "@/types/mcp";
import {
  Plus,
  Pencil,
  Trash2,
  RefreshCw,
  Plug,
  Terminal,
  Globe,
} from "lucide-react";
import { toast } from "sonner";
import axios from "axios";

interface ServerFormData extends McpServerConfig {
  name: string;
  transport: TransportType;
}

const emptyForm = (): ServerFormData => ({
  name: "",
  transport: "stdio",
  enabled: true,
  command: "",
  args: [],
  env: {},
  url: "",
  headers: {},
  timeoutSecs: 30,
  maxRetries: 3,
});

const TRANSPORT_LABELS: Record<TransportType, string> = {
  stdio: "stdio (本地进程)",
  sse: "sse (远程 SSE)",
  streamable_http: "streamable_http (远程 HTTP)",
};

const BADGE_VARIANT: Record<
  TransportType,
  "secondary" | "default" | "outline"
> = {
  stdio: "secondary",
  sse: "default",
  streamable_http: "outline",
};

/** Strip surrounding single or double quotes from a value. */
function unquote(s: string): string {
  if (
    (s.startsWith('"') && s.endsWith('"')) ||
    (s.startsWith("'") && s.endsWith("'"))
  ) {
    const inner = s.slice(1, -1);
    // Only unquote when the quotes are matched and the inner content doesn't break
    if (!inner.includes(s[0])) return inner;
  }
  return s;
}

function getApiErrorMessage(err: unknown): string | undefined {
  if (axios.isAxiosError(err)) {
    // 后端 ApiError 序列化为 { "error": "...", "details": "..." }
    if (err.response?.data?.details) return String(err.response.data.details);
    if (err.response?.data?.message) return String(err.response.data.message);
    if (err.message) return err.message;
  }
  if (err instanceof Error) return err.message;
  return undefined;
}

function parseLines(text: string): string[] {
  return text
    .split("\n")
    .map((l) => l.trim())
    .filter(Boolean);
}

function parseEnv(text: string): Record<string, string> {
  const env: Record<string, string> = {};
  for (const line of parseLines(text)) {
    const eq = line.indexOf("=");
    if (eq > 0) {
      env[line.slice(0, eq).trim()] = unquote(line.slice(eq + 1).trim());
    }
  }
  return env;
}

function parseHeaders(text: string): Record<string, string> {
  const headers: Record<string, string> = {};
  for (const line of parseLines(text)) {
    const col = line.indexOf(":");
    if (col > 0) {
      headers[line.slice(0, col).trim()] = unquote(line.slice(col + 1).trim());
    }
  }
  return headers;
}

function envToString(env: Record<string, string>): string {
  return Object.entries(env)
    .map(([k, v]) => `${k}=${v}`)
    .join("\n");
}

function headersToString(headers: Record<string, string>): string {
  return Object.entries(headers)
    .map(([k, v]) => `${k}: ${v}`)
    .join("\n");
}

// Form state uses string representations for args/env/headers (textarea-friendly)
interface DialogFormState {
  name: string;
  transport: TransportType;
  enabled: boolean;
  command: string;
  argsText: string;
  envText: string;
  url: string;
  headersText: string;
  timeoutSecs: number;
  maxRetries: number;
}

const dialogFormDefault = (): DialogFormState => ({
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
});

function configToDialogForm(name: string, s: McpServerConfig): DialogFormState {
  return {
    name,
    transport: s.transport,
    enabled: s.enabled !== false,
    command: s.command || "",
    argsText: (s.args || []).join("\n"),
    envText: s.env ? envToString(s.env) : "",
    url: s.url || "",
    headersText: s.headers ? headersToString(s.headers) : "",
    timeoutSecs: s.timeoutSecs ?? 30,
    maxRetries: s.maxRetries ?? 3,
  };
}

function dialogFormToConfig(f: DialogFormState): McpServerConfig {
  const isStdio = f.transport === "stdio";
  return {
    transport: f.transport,
    enabled: f.enabled,
    ...(isStdio
      ? {
          command: f.command || undefined,
          args: parseLines(f.argsText),
          env: parseEnv(f.envText),
        }
      : {
          url: f.url || undefined,
          headers: parseHeaders(f.headersText),
        }),
    timeoutSecs: f.timeoutSecs,
    maxRetries: f.maxRetries,
  };
}

export function McpConfigPage() {
  const [config, setConfig] = useState<Record<string, McpServerConfig>>({});
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);

  // Dialog state
  const [dialogOpen, setDialogOpen] = useState(false);
  const [editingName, setEditingName] = useState<string | null>(null);
  const [dialogForm, setDialogForm] =
    useState<DialogFormState>(dialogFormDefault());
  const [nameError, setNameError] = useState("");

  // Test connection state
  const [testing, setTesting] = useState<string | null>(null);

  // Delete confirmation
  const [deleteTarget, setDeleteTarget] = useState<string | null>(null);

  const unmountedRef = useRef(false);
  useEffect(() => {
    unmountedRef.current = false;
    getMcpConfig()
      .then((res) => {
        if (unmountedRef.current) return;
        const servers = res.mcpServers || {};
        setConfig(servers);
      })
      .catch(() => {
        if (!unmountedRef.current) toast.error("加载 MCP 配置失败");
      })
      .finally(() => {
        if (!unmountedRef.current) setLoading(false);
      });
    return () => {
      unmountedRef.current = true;
    };
  }, []);

  // ── Dialog handlers ──────────────────────────────────────────────────────────

  const openAddDialog = () => {
    setEditingName(null);
    setDialogForm(dialogFormDefault());
    setNameError("");
    setDialogOpen(true);
  };

  const openEditDialog = (name: string) => {
    setEditingName(name);
    const server = config[name];
    setDialogForm(configToDialogForm(name, server));
    setNameError("");
    setDialogOpen(true);
  };

  const closeDialog = () => {
    setDialogOpen(false);
    setEditingName(null);
    setNameError("");
  };

  const isDialogFormValid = () => {
    if (!dialogForm.name.trim()) return false;
    if (dialogForm.transport === "stdio") {
      if (!dialogForm.command.trim()) return false;
    } else {
      if (!dialogForm.url.trim()) return false;
    }
    return true;
  };

  const handleDialogSubmit = async () => {
    // Validate name
    const name = dialogForm.name.trim();
    if (!name) {
      setNameError("请输入服务器名称");
      return;
    }

    // Check for duplicate name on add
    if (!editingName && config[name]) {
      setNameError("该名称已存在");
      return;
    }

    setNameError("");
    const serverConfig = dialogFormToConfig(dialogForm);
    const nextConfig = { ...config, [name]: serverConfig };

    // 立即持久化到后端，避免"保存修改"仅更新前端 state 导致文件未更新
    setSaving(true);
    try {
      await saveMcpConfig({ mcpServers: nextConfig });
      setConfig(nextConfig);
      closeDialog();
      toast.success("配置已保存");
    } catch (err) {
      toast.error(getApiErrorMessage(err) || "保存失败");
    } finally {
      setSaving(false);
    }
  };

  const handleTransportChange = (transport: TransportType) => {
    // Clear transport-specific fields when switching
    setDialogForm((prev) => ({
      ...prev,
      transport,
      ...(transport === "stdio"
        ? { url: "", headersText: "" }
        : { command: "", argsText: "", envText: "" }),
    }));
  };

  // ── Config actions ───────────────────────────────────────────────────────────

  const confirmDelete = (name: string) => {
    setDeleteTarget(name);
  };

  const handleDelete = async () => {
    if (!deleteTarget) return;
    const name = deleteTarget;
    const nextConfig = { ...config };
    delete nextConfig[name];

    // 立即持久化到后端
    setSaving(true);
    try {
      await saveMcpConfig({ mcpServers: nextConfig });
      setConfig(nextConfig);
      setDeleteTarget(null);
      toast.success("配置已删除");
    } catch (err) {
      toast.error(getApiErrorMessage(err) || "删除失败");
    } finally {
      setSaving(false);
    }
  };

  const handleTest = async (name: string) => {
    setTesting(name);
    try {
      const result: McpTestResult = await testMcpConnection(name);
      if (result.success) {
        toast.success(
          `${name} 连接成功 — 发现 ${result.tool_count} 个工具：${result.tools.join(", ")}（耗时 ${result.duration_ms}ms）`,
        );
      } else {
        // 根据 error_type 给出差异化提示
        const hints: Record<McpTestErrorType, string> = {
          config_not_found: "请检查 MCP 配置是否已保存",
          invalid_config: "请检查必填字段（command 或 url）是否填写正确",
          connection_refused: "请确认 MCP Server 已启动且端口可访问",
          connection_timeout: "请检查网络连接或增加超时时间",
          handshake_failed:
            "MCP 协议握手失败，请确认 Server 实现了正确的 MCP 协议",
          transport_error: "传输层错误，请检查连接参数或进程路径",
          tool_list_failed:
            "连接成功但获取工具列表失败，请检查 Server 端工具注册逻辑",
        };
        const hint = result.error_type ? hints[result.error_type] : "";
        toast.error(
          `测试 ${name} 失败：${result.message}（耗时 ${result.duration_ms}ms）`,
          {
            description: hint || result.error_type,
          },
        );
      }
    } catch (err) {
      toast.error(getApiErrorMessage(err) || `连接 ${name} 测试失败`);
    } finally {
      setTesting(null);
    }
  };

  // ── Render ───────────────────────────────────────────────────────────────────

  const servers = Object.entries(config);

  if (loading) return <LoadingSpinner />;

  return (
    <div className="max-w-4xl mx-auto space-y-4">
      {/* Header */}
      <div className="flex items-center justify-between">
        <h2 className="text-2xl font-bold">MCP 配置</h2>
        <div className="flex gap-2">
          <Button onClick={openAddDialog}>
            <Plus className="mr-2 h-4 w-4" />
            添加服务器
          </Button>
        </div>
      </div>

      {/* Empty state */}
      {servers.length === 0 ? (
        <EmptyState
          icon={Plug}
          title="暂无 MCP 服务器"
          description="添加一个 MCP 服务器开始使用"
        />
      ) : (
        <div className="grid gap-3 md:grid-cols-2">
          {servers.map(([name, server]) => {
            const isStdio = server.transport === "stdio";
            const isTesting = testing === name;
            return (
              <Card
                key={name}
                className="group hover:border-primary/50 transition-colors"
              >
                <CardContent className="p-4 space-y-3">
                  {/* Row 1: status + name + transport badge + actions */}
                  <div className="flex items-center gap-2.5">
                    {/* Status dot */}
                    <span
                      className={`h-2.5 w-2.5 shrink-0 rounded-full ${
                        server.enabled !== false
                          ? "bg-green-500"
                          : "bg-gray-400"
                      }`}
                    />
                    <span className="font-medium truncate flex-1">{name}</span>
                    <Badge
                      variant={BADGE_VARIANT[server.transport]}
                      className="shrink-0"
                    >
                      {server.transport}
                    </Badge>

                    {/* Actions — visible on hover */}
                    <div className="flex gap-0.5 opacity-0 group-hover:opacity-100 transition-opacity shrink-0">
                      <Button
                        variant="ghost"
                        size="icon"
                        onClick={() => openEditDialog(name)}
                        title="编辑"
                      >
                        <Pencil className="h-4 w-4" />
                      </Button>
                      <Button
                        variant="ghost"
                        size="icon"
                        onClick={() => handleTest(name)}
                        disabled={testing !== null}
                        title={testing !== null ? "正在测试中..." : "测试连接"}
                      >
                        <RefreshCw
                          className={`h-4 w-4 ${isTesting ? "animate-spin" : ""}`}
                        />
                      </Button>
                      <Button
                        variant="ghost"
                        size="icon"
                        onClick={() => confirmDelete(name)}
                        title="删除"
                      >
                        <Trash2 className="h-4 w-4 text-destructive" />
                      </Button>
                    </div>
                  </div>

                  {/* Row 2: metadata */}
                  <div className="flex items-center gap-2 text-xs text-muted-foreground">
                    {isStdio ? (
                      <>
                        <Terminal className="h-3.5 w-3.5 shrink-0" />
                        <span className="truncate font-mono">
                          {server.command || "?"}
                        </span>
                        {(server.args || []).length > 0 && (
                          <span className="shrink-0">
                            · {(server.args || []).length} 个参数
                          </span>
                        )}
                      </>
                    ) : (
                      <>
                        <Globe className="h-3.5 w-3.5 shrink-0" />
                        <span className="truncate font-mono">
                          {server.url || "?"}
                        </span>
                      </>
                    )}
                    <span className="shrink-0">
                      · {server.timeoutSecs ?? 30}s · 重试{" "}
                      {server.maxRetries ?? 3} 次
                    </span>
                  </div>
                </CardContent>
              </Card>
            );
          })}
        </div>
      )}

      {/* Delete Confirmation Dialog */}
      <Dialog open={!!deleteTarget} onOpenChange={() => setDeleteTarget(null)}>
        <DialogContent className="sm:max-w-md">
          <DialogHeader>
            <DialogTitle>确认删除</DialogTitle>
            <DialogDescription>
              确定要删除 MCP 服务器{" "}
              <span className="font-semibold">{deleteTarget}</span>{" "}
              吗？此操作不可撤销。
            </DialogDescription>
          </DialogHeader>
          <DialogFooter>
            <Button variant="outline" onClick={() => setDeleteTarget(null)}>
              取消
            </Button>
            <Button
              variant="destructive"
              onClick={handleDelete}
              disabled={saving}
            >
              删除
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      {/* Add / Edit Dialog */}
      <Dialog open={dialogOpen} onOpenChange={(open) => !open && closeDialog()}>
        <DialogContent className="sm:max-w-lg max-h-[85vh] overflow-y-auto">
          <DialogHeader>
            <DialogTitle>
              {editingName ? `编辑 ${editingName}` : "添加 MCP 服务器"}
            </DialogTitle>
            <DialogDescription>配置 MCP 服务器连接参数</DialogDescription>
          </DialogHeader>

          <div className="space-y-4">
            {/* Name */}
            <div className="space-y-1.5">
              <Label htmlFor="mcp-name">名称</Label>
              <Input
                id="mcp-name"
                placeholder="my-mcp-server"
                value={dialogForm.name}
                onChange={(e) => {
                  setDialogForm((p) => ({ ...p, name: e.target.value }));
                  if (nameError) setNameError("");
                }}
                disabled={!!editingName}
              />
              {nameError && (
                <p className="text-sm text-destructive mt-1">{nameError}</p>
              )}
            </div>

            {/* Transport */}
            <div className="space-y-1.5">
              <Label>传输类型</Label>
              <Select
                value={dialogForm.transport}
                onValueChange={(v) => handleTransportChange(v as TransportType)}
              >
                <SelectTrigger className="w-full">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="stdio">
                    {TRANSPORT_LABELS.stdio}
                  </SelectItem>
                  <SelectItem value="sse">{TRANSPORT_LABELS.sse}</SelectItem>
                  <SelectItem value="streamable_http">
                    {TRANSPORT_LABELS.streamable_http}
                  </SelectItem>
                </SelectContent>
              </Select>
            </div>

            {/* Enabled */}
            <div className="flex items-center gap-2">
              <Switch
                id="mcp-enabled"
                checked={dialogForm.enabled}
                onCheckedChange={(v) =>
                  setDialogForm((p) => ({ ...p, enabled: v }))
                }
              />
              <Label htmlFor="mcp-enabled">启用</Label>
            </div>

            {/* ── stdio fields ─────────────────────────────────────────── */}
            {dialogForm.transport === "stdio" && (
              <>
                <div className="space-y-1.5">
                  <Label htmlFor="mcp-command">命令 *</Label>
                  <Input
                    id="mcp-command"
                    placeholder="npx 或可执行文件路径"
                    value={dialogForm.command}
                    onChange={(e) =>
                      setDialogForm((p) => ({ ...p, command: e.target.value }))
                    }
                  />
                </div>
                <div className="space-y-1.5">
                  <Label htmlFor="mcp-args">参数（每行一个）</Label>
                  <Textarea
                    id="mcp-args"
                    rows={3}
                    placeholder="-y\n@scope/server\n--port=8080"
                    value={dialogForm.argsText}
                    onChange={(e) =>
                      setDialogForm((p) => ({ ...p, argsText: e.target.value }))
                    }
                  />
                </div>
                <div className="space-y-1.5">
                  <Label htmlFor="mcp-env">
                    环境变量（KEY=VALUE，每行一个）
                  </Label>
                  <Textarea
                    id="mcp-env"
                    rows={3}
                    placeholder="NODE_ENV=production\nDEBUG=true"
                    value={dialogForm.envText}
                    onChange={(e) =>
                      setDialogForm((p) => ({ ...p, envText: e.target.value }))
                    }
                  />
                </div>
              </>
            )}

            {/* ── sse / streamable_http fields ─────────────────────────── */}
            {(dialogForm.transport === "sse" ||
              dialogForm.transport === "streamable_http") && (
              <>
                <div className="space-y-1.5">
                  <Label htmlFor="mcp-url">URL *</Label>
                  <Input
                    id="mcp-url"
                    placeholder="http://localhost:8000/mcp"
                    value={dialogForm.url}
                    onChange={(e) =>
                      setDialogForm((p) => ({ ...p, url: e.target.value }))
                    }
                  />
                </div>
                <div className="space-y-1.5">
                  <Label htmlFor="mcp-headers">
                    Headers（KEY: VALUE，每行一个）
                  </Label>
                  <Textarea
                    id="mcp-headers"
                    rows={3}
                    placeholder="Authorization: Bearer token\nX-API-Key: abc123"
                    value={dialogForm.headersText}
                    onChange={(e) =>
                      setDialogForm((p) => ({
                        ...p,
                        headersText: e.target.value,
                      }))
                    }
                  />
                </div>
              </>
            )}

            {/* ── Common fields ────────────────────────────────────────── */}
            <div className="grid grid-cols-2 gap-4">
              <div className="space-y-1.5">
                <Label htmlFor="mcp-timeout">超时（秒）</Label>
                <Input
                  id="mcp-timeout"
                  type="number"
                  min={1}
                  max={300}
                  value={dialogForm.timeoutSecs}
                  onChange={(e) =>
                    setDialogForm((p) => ({
                      ...p,
                      timeoutSecs: Number(e.target.value) || 30,
                    }))
                  }
                />
              </div>
              <div className="space-y-1.5">
                <Label htmlFor="mcp-retries">最大重试次数</Label>
                <Input
                  id="mcp-retries"
                  type="number"
                  min={0}
                  max={10}
                  value={dialogForm.maxRetries}
                  onChange={(e) =>
                    setDialogForm((p) => ({
                      ...p,
                      maxRetries: Number(e.target.value) || 3,
                    }))
                  }
                />
              </div>
            </div>
          </div>

          <DialogFooter>
            <Button variant="outline" onClick={closeDialog}>
              取消
            </Button>
            <Button
              onClick={handleDialogSubmit}
              disabled={!isDialogFormValid() || saving}
            >
              {editingName ? "保存修改" : "添加"}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </div>
  );
}
