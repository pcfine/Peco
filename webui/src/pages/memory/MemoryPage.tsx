// 「记忆」页骨架 —— 标题 / 刷新 / Tabs（design §3.5.2）

import { useRef, useState } from "react";
import { RefreshCw } from "lucide-react";
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

export function MemoryPage() {
  // 状态提升：刷新按钮据此只触发「当前激活视图」重取
  const [activeTab, setActiveTab] = useState("graph");
  const graphRef = useRef<MemoryGraphViewHandle>(null);
  const documentsRef = useRef<MemoryDocumentListHandle>(null);

  const handleRefresh = () => {
    if (activeTab === "documents") documentsRef.current?.refresh();
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

      <Tabs defaultValue="graph" value={activeTab} onValueChange={setActiveTab}>
        <TabsList>
          <TabsTrigger value="graph">图谱</TabsTrigger>
          <TabsTrigger value="documents">列表</TabsTrigger>
        </TabsList>

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
