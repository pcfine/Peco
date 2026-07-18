import { useEffect, useState } from 'react'
import { Link } from 'react-router-dom'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { Textarea } from '@/components/ui/textarea'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { Dialog, DialogContent, DialogHeader, DialogTitle, DialogTrigger } from '@/components/ui/dialog'
import { LoadingSpinner } from '@/components/common/LoadingSpinner'
import { EmptyState } from '@/components/common/EmptyState'
import { listKnowledgeBases, createKnowledgeBase, deleteKnowledgeBase } from '@/api/knowledge'
import type { KnowledgeBase, CreateKbRequest } from '@/types/knowledge'
import { Plus, Trash2, BookOpen } from 'lucide-react'
import { toast } from 'sonner'

export function KnowledgeListPage() {
  const [kbs, setKbs] = useState<KnowledgeBase[]>([])
  const [loading, setLoading] = useState(true)
  const [open, setOpen] = useState(false)
  const [form, setForm] = useState({ name: '', description: '' })

  const load = () => {
    listKnowledgeBases().then(setKbs).catch(() => toast.error('加载失败')).finally(() => setLoading(false))
  }
  useEffect(() => { load() }, [])

  const handleCreate = async () => {
    try {
      const data: CreateKbRequest = { name: form.name, description: form.description }
      await createKnowledgeBase(data)
      toast.success('知识库创建成功')
      setOpen(false)
      setForm({ name: '', description: '' })
      load()
    } catch {
      toast.error('创建失败')
    }
  }

  const handleDelete = async (id: string) => {
    try {
      await deleteKnowledgeBase(id)
      setKbs((p) => p.filter((k) => k.id !== id))
      toast.success('已删除')
    } catch {
      toast.error('删除失败')
    }
  }

  if (loading) return <LoadingSpinner />

  return (
    <div className="max-w-4xl mx-auto space-y-4">
      <div className="flex items-center justify-between">
        <h2 className="text-2xl font-bold">知识库</h2>
        <Dialog open={open} onOpenChange={setOpen}>
          <DialogTrigger asChild>
            <Button><Plus className="mr-2 h-4 w-4" />创建知识库</Button>
          </DialogTrigger>
          <DialogContent>
            <DialogHeader><DialogTitle>新建知识库</DialogTitle></DialogHeader>
            <div className="space-y-4">
              <div><Label>名称</Label><Input value={form.name} onChange={(e) => setForm({ ...form, name: e.target.value })} /></div>
              <div><Label>描述</Label><Textarea value={form.description} onChange={(e) => setForm({ ...form, description: e.target.value })} /></div>
              <Button onClick={handleCreate} disabled={!form.name.trim()} className="w-full">创建</Button>
            </div>
          </DialogContent>
        </Dialog>
      </div>

      {kbs.length === 0 ? (
        <EmptyState icon={BookOpen} title="暂无知识库" />
      ) : (
        <div className="grid gap-3 md:grid-cols-2">
          {kbs.map((kb) => (
            <Link key={kb.id} to={`/knowledge/${kb.id}`}>
              <Card className="group hover:bg-accent/50 transition-colors">
                <CardHeader className="flex flex-row items-center justify-between p-4">
                  <div className="min-w-0 flex-1">
                    <CardTitle className="text-base truncate">{kb.name}</CardTitle>
                    <p className="text-xs text-muted-foreground mt-1">{kb.description || '无描述'}</p>
                    <p className="text-xs text-muted-foreground mt-1">
                      {kb.document_count} 文档 · {kb.chunk_count} 分块 · {kb.embedding_model}
                    </p>
                  </div>
                  <Button variant="ghost" size="icon" className="shrink-0 opacity-0 group-hover:opacity-100"
                    onClick={(e) => { e.preventDefault(); handleDelete(kb.id) }}>
                    <Trash2 className="h-4 w-4 text-destructive" />
                  </Button>
                </CardHeader>
              </Card>
            </Link>
          ))}
        </div>
      )}
    </div>
  )
}
