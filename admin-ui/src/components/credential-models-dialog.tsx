import { Copy, Loader2, RefreshCw } from 'lucide-react'
import { toast } from 'sonner'
import { Button } from '@/components/ui/button'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import { useCredentialModels } from '@/hooks/use-credentials'
import { parseError } from '@/lib/utils'

interface CredentialModelsDialogProps {
  credentialId: number
  onOpenChange: (open: boolean) => void
}

async function copyModelIds(value: string) {
  try {
    await navigator.clipboard.writeText(value)
    toast.success('已复制模型 ID')
  } catch {
    toast.error('复制失败，请选中模型 ID 手动复制')
  }
}

function ModelId({ value }: { value: string | null }) {
  if (value === null) {
    return <span className="text-muted-foreground">—</span>
  }

  return (
    <div className="flex min-w-0 items-start gap-1">
      <code className="min-w-0 flex-1 break-all py-1 text-xs select-text">{value}</code>
      <Button
        size="icon"
        variant="ghost"
        className="h-7 w-7 shrink-0"
        aria-label={`复制 ${value}`}
        title={`复制 ${value}`}
        onClick={() => void copyModelIds(value)}
      >
        <Copy className="h-3.5 w-3.5" />
      </Button>
    </div>
  )
}

export function CredentialModelsDialog({ credentialId, onOpenChange }: CredentialModelsDialogProps) {
  const { data, isFetching, error, refetch } = useCredentialModels(credentialId)
  // 刷新失败时不展示上一次结果，避免旧列表被误认为本次查询成功。
  const models = !isFetching && !error && data?.id === credentialId ? data.models : null
  const parsedError = error ? parseError(error) : null

  const copyAllModels = () => {
    if (!models) return
    const ids = models.flatMap(({ modelId, thinkingModelId }) =>
      thinkingModelId === null ? [modelId] : [modelId, thinkingModelId]
    )
    void copyModelIds([...new Set(ids)].join('\n'))
  }

  return (
    <Dialog open onOpenChange={onOpenChange}>
      <DialogContent className="flex max-h-[85vh] w-[calc(100%-2rem)] flex-col sm:max-w-2xl">
        <DialogHeader className="shrink-0 pr-5">
          <DialogTitle>凭据 #{credentialId} 模型列表</DialogTitle>
          <DialogDescription>
            此账号实际返回的模型 ID，以及对应的 thinking 模型 ID。
          </DialogDescription>
        </DialogHeader>

        <div className="min-h-0 overflow-y-auto" aria-live="polite" aria-busy={isFetching}>
          {isFetching && (
            <div className="flex items-center justify-center gap-2 py-10 text-sm text-muted-foreground" role="status">
              <Loader2 className="h-5 w-5 animate-spin" />
              正在获取此账号的模型…
            </div>
          )}

          {!isFetching && parsedError && (
            <div className="space-y-2 py-8 text-center" role="alert">
              <p className="font-medium text-destructive">获取模型失败</p>
              <p className="break-words text-sm">{parsedError.title}</p>
              {parsedError.detail && (
                <p className="break-words text-sm text-muted-foreground">{parsedError.detail}</p>
              )}
            </div>
          )}

          {models?.length === 0 && (
            <p className="py-10 text-center text-sm text-muted-foreground">
              此账号未返回可用模型。
            </p>
          )}

          {models && models.length > 0 && (
            <div className="rounded-md border">
              <div className="hidden grid-cols-2 gap-4 border-b bg-muted/50 px-3 py-2 text-sm font-medium sm:grid">
                <span>模型 ID</span>
                <span>Thinking 模型 ID</span>
              </div>
              <ul className="divide-y">
                {models.map(({ modelId, thinkingModelId }) => (
                  <li key={modelId} className="grid gap-3 px-3 py-3 sm:grid-cols-2 sm:gap-4">
                    <div className="min-w-0">
                      <p className="mb-1 text-xs text-muted-foreground sm:hidden">模型 ID</p>
                      <ModelId value={modelId} />
                    </div>
                    <div className="min-w-0">
                      <p className="mb-1 text-xs text-muted-foreground sm:hidden">Thinking 模型 ID</p>
                      <ModelId value={thinkingModelId} />
                    </div>
                  </li>
                ))}
              </ul>
            </div>
          )}
        </div>

        <div className="flex shrink-0 flex-wrap items-center justify-between gap-3 border-t pt-4">
          <span className="text-sm text-muted-foreground">
            {models ? `共 ${models.length} 个账号模型` : ''}
          </span>
          <div className="flex flex-wrap gap-2">
            <Button size="sm" variant="outline" onClick={copyAllModels} disabled={!models?.length}>
              <Copy className="mr-1 h-4 w-4" />
              复制全部 ID
            </Button>
            <Button size="sm" onClick={() => void refetch()} disabled={isFetching}>
              <RefreshCw className={`mr-1 h-4 w-4 ${isFetching ? 'animate-spin' : ''}`} />
              {isFetching ? '获取中…' : error ? '重试' : '刷新'}
            </Button>
          </div>
        </div>
      </DialogContent>
    </Dialog>
  )
}
