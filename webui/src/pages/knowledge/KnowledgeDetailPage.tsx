import { useEffect, useState, useRef } from 'react'
import { useParams, Link } from 'react-router-dom'
import { Button } from '@/components/ui/button'
import { LoadingSpinner } from '@/components/common/LoadingSpinner'
import { getKnowledgeBase, listDocuments, uploadDocument, syncKnowledgeBase, deleteDocument } from '@/api/knowledge'
import type { KnowledgeBase, Document } from '@/types/knowledge'
import { ArrowLeft, Upload, RefreshCw, Trash2, FileText } from 'lucide-react'
import { toast } from 'sonner'

const STATUS_COLORS: Record<string, string> = {
  pending: 'bg-yellow-100 text-yellow-700',
  processing: 'bg-blue-100 text-blue-700',
  ready: 'bg-green-100 text-green-700',
  error: 'bg-red-100 text-red-700',
}

export function KnowledgeDetailPage() {
  const { kbId } = useParams<{ kbId: string }>()
  const [kb, setKb] = useState<KnowledgeBase | null>(null)
  const [docs, setDocs] = useState<Document[]>([])
  const [loading, setLoading] = useState(true)
  const [uploading, setUploading] = useState(false)
  const [progress, setProgress] = useState(0)
  const fileRef = useRef<HTMLInputElement>(null)
  const abortRef = useRef<AbortController | null>(null)

  const load = () => {
    if (!kbId) return
    Promise.all([getKnowledgeBase(kbId), listDocuments(kbId)])
      .then(([k, d]) => { setKb(k); setDocs(d) })
      .catch(() => toast.error('加载失败'))
      .finally(() => setLoading(false))
  }
  useEffect(() => { load() }, [kbId])

  // Poll document status
  useEffect(() => {
    const pending = docs.some((d) => d.status === 'pending' || d.status === 'processing')
    if (!pending) return
    const timer = setInterval(() => {
      if (!kbId) return
      listDocuments(kbId).then(setDocs)
    }, 3000)
    return () => clearInterval(timer)
  }, [docs, kbId])

  const handleUpload = async (files: FileList | null) => {
    if (!files?.length || !kbId) return
    setUploading(true)
    abortRef.current = new AbortController()
    try {
      await uploadDocument(kbId, files[0], setProgress, abortRef.current.signal)
      toast.success('上传成功')
      load()
    } catch {
      toast.error('上传失败')
    } finally {
      setUploading(false)
      setProgress(0)
    }
  }

  const handleSync = async () => {
    if (!kbId) return
    try {
      const result = await syncKnowledgeBase(kbId)
      toast.success(`同步完成: +${result.added} -${result.removed}`)
      load()
    } catch {
      toast.error('同步失败')
    }
  }

  const handleDeleteDoc = async (docId: string) => {
    if (!kbId) return
    try {
      await deleteDocument(kbId, docId)
      setDocs((p) => p.filter((d) => d.id !== docId))
      toast.success('已删除')
    } catch {
      toast.error('删除失败')
    }
  }

  if (loading) return <LoadingSpinner />
  if (!kb) return null

  return (
    <div className="max-w-4xl mx-auto space-y-6">
      <div className="flex items-center gap-3">
        <Link to="/knowledge" className="text-muted-foreground hover:text-foreground">
          <ArrowLeft className="h-5 w-5" />
        </Link>
        <h2 className="text-2xl font-bold">{kb.name}</h2>
        <Button variant="outline" size="sm" onClick={handleSync}>
          <RefreshCw className="mr-2 h-4 w-4" />同步
        </Button>
      </div>
      <p className="text-muted-foreground">{kb.description || '无描述'}</p>
      <p className="text-sm text-muted-foreground">
        {kb.document_count} 文档 · {kb.chunk_count} 分块 · {kb.embedding_model}
      </p>

      <div className="flex items-center gap-2">
        <input ref={fileRef} type="file" className="hidden"
          onChange={(e) => handleUpload(e.target.files)}
          accept=".pdf,.docx,.html,.md,.txt,.py,.rs,.go,.js,.ts" />
        <Button onClick={() => fileRef.current?.click()} disabled={uploading}>
          <Upload className="mr-2 h-4 w-4" />
          {uploading ? `上传中 ${progress}%` : '上传文档'}
        </Button>
        {uploading && (
          <Button variant="ghost" onClick={() => abortRef.current?.abort()}>取消</Button>
        )}
      </div>

      <div className="space-y-2">
        <h3 className="font-semibold">文档列表 ({docs.length})</h3>
        {docs.length === 0 ? (
          <p className="text-muted-foreground">暂无文档</p>
        ) : (
          <div className="space-y-2">
            {docs.map((doc) => (
              <div key={doc.id} className="flex items-center gap-3 rounded-md border p-3">
                <FileText className="h-5 w-5 text-muted-foreground shrink-0" />
                <div className="flex-1 min-w-0">
                  <p className="text-sm font-medium truncate">{doc.filename}</p>
                  <p className="text-xs text-muted-foreground">
                    {(doc.file_size / 1024).toFixed(1)} KB · {doc.mime_type}
                  </p>
                </div>
                <span className={`rounded-full px-2 py-0.5 text-xs font-medium ${STATUS_COLORS[doc.status] || ''}`}>
                  {doc.status}
                </span>
                {doc.error_msg && (
                  <span className="text-xs text-destructive" title={doc.error_msg}>⚠️</span>
                )}
                <Button variant="ghost" size="icon" onClick={() => handleDeleteDoc(doc.id)}>
                  <Trash2 className="h-4 w-4 text-destructive" />
                </Button>
              </div>
            ))}
          </div>
        )}
      </div>
    </div>
  )
}
