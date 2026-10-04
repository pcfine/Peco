// 「记忆」页骨架 —— 标题 / 刷新 / Tabs（design §3.5.2）
// 历史 tab 置顶 + 待处理取代徽章：监管纠错为主用例（design §11 / Q1）

import { useCallback, useEffect, useRef, useState } from "react";
import { RefreshCw } from "lucide-react";
import { getSupersedeHealth } from "@/api/memory";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import {
  MemoryGraphView,
  type MemoryGraphViewHandle,
} from "@/components/memory/MemoryGraphView";
import {
  MemoryDocumentList,
  type MemoryDocumentListHandle,
} from "@/components/memory/MemoryDocumentList";
import {
  MemoryAuditList,
  type MemoryAuditListHandle,
} from "@/components/memory/MemoryAuditList";

export function MemoryPage() {
  // 状态提升：刷新按钮据此只触发「当前激活视图」重取。
  // 默认落「列表」—— 取代门未开时历史为空，避免落在空页；
  // 历史以首位 + 徽章体现监管优先。
  const [activeTab, setActiveTab] = useState("documents");
  const graphRef = useRef<MemoryGraphViewHandle>(null);
  const documentsRef = useRef<MemoryDocumentListHandle>(null);
  const historyRef = useRef<MemoryAuditListHandle>(null);

  // 「待处理取代」徽章 = health.pending + failed（>0 才显示）。
  // health 拉取失败静默降级：保持上次值，不阻断页面。
  const [pendingCount, setPendingCount] = useState(0);
  const loadHealth = useCallback(async () => {
    try {
      const health = await getSupersedeHealth();
      setPendingCount(health.pending + health.failed);
    } catch {
      // 静默降级
    }
  }, []);

  // 挂载即拉一次（徽章常驻 tab 栏）；切到历史 tab 再拉，跟上后台对账进度。
  useEffect(() => {
    void loadHealth();
  }, [loadHealth]);
  useEffect(() => {
    if (activeTab === "history") void loadHealth();
  }, [activeTab, loadHealth]);

  const handleRefresh = () => {
    if (activeTab === "history") historyRef.current?.refresh();
    else if (activeTab === "documents") documentsRef.current?.refresh();
    else graphRef.current?.refresh();
  };

  return (
    <div className="space-y-4">
      <div className="flex items-center justify-between">
        <h2 className="text-lg font-semibold">记忆</h2>
        <Button variant="outline" size="sm" onClick={handleRefresh}>
          <RefreshCw className="h-4 w-4" />
          刷新
        </Button>
      </div>

      <Tabs
        defaultValue="documents"
        value={activeTab}
        onValueChange={setActiveTab}
      >
        <TabsList>
          <TabsTrigger value="history" className="gap-1.5">
            历史
            {pendingCount > 0 && (
              <Badge
                variant="destructive"
                className="px-1.5"
                title="待处理取代"
              >
                {pendingCount}
              </Badge>
            )}
          </TabsTrigger>
          <TabsTrigger value="graph">图谱</TabsTrigger>
          <TabsTrigger value="documents">列表</TabsTrigger>
        </TabsList>

        <TabsContent value="history" className="mt-4">
          <MemoryAuditList ref={historyRef} />
        </TabsContent>
        <TabsContent value="graph" className="mt-4">
          <MemoryGraphView ref={graphRef} />
        </TabsContent>
        <TabsContent value="documents" className="mt-4">
          <MemoryDocumentList ref={documentsRef} />
        </TabsContent>
      </Tabs>
    </div>
  );
}
