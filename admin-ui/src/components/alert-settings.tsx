import { useState, useEffect } from 'react'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { Switch } from '@/components/ui/switch'
import { Badge } from '@/components/ui/badge'
import { toast } from 'sonner'
import { Plus, Pencil, Trash2 } from 'lucide-react'
import {
  useAlertConfig, useAlertStatus, useUpdateAlertConfig, useDeleteAlertChannel,
} from '@/hooks/use-alerts'
import { AlertChannelDialog } from '@/components/alert-channel-dialog'
import type { AlertChannelResponse } from '@/types/api'

export function AlertSettings() {
  const { data: config } = useAlertConfig()
  const { data: status } = useAlertStatus()
  const updateConfig = useUpdateAlertConfig()
  const deleteChannel = useDeleteAlertChannel()

  const [enabled, setEnabled] = useState(false)
  const [threshold, setThreshold] = useState('1000')
  const [pollSecs, setPollSecs] = useState('1800')
  const [prefix, setPrefix] = useState('')
  const [dialogOpen, setDialogOpen] = useState(false)
  const [editing, setEditing] = useState<AlertChannelResponse | null>(null)

  useEffect(() => {
    if (config) {
      setEnabled(config.enabled)
      setThreshold(String(config.thresholdRemaining))
      setPollSecs(String(config.pollIntervalSecs))
      setPrefix(config.subjectPrefix ?? '')
    }
  }, [config])

  const handleSave = async () => {
    try {
      await updateConfig.mutateAsync({
        enabled,
        thresholdRemaining: Number(threshold),
        pollIntervalSecs: Number(pollSecs),
        subjectPrefix: prefix,
      })
      toast.success('预警设置已保存')
    } catch {
      toast.error('保存失败')
    }
  }

  const handleDelete = async (id: string) => {
    try {
      await deleteChannel.mutateAsync(id)
      toast.success('渠道已删除')
    } catch {
      toast.error('删除失败')
    }
  }

  const fmtTime = (ts?: number) =>
    ts ? new Date(ts * 1000).toLocaleString('zh-CN') : '尚未检查'

  return (
    <Card className="mb-6">
      <CardHeader>
        <CardTitle className="flex items-center justify-between">
          <span>Credit 预警设置</span>
          {status && (
            <Badge variant={status.fired ? 'destructive' : 'secondary'}>
              {status.fired ? '已触发' : '已就绪'}
            </Badge>
          )}
        </CardTitle>
      </CardHeader>
      <CardContent className="space-y-4">
        <div className="flex items-center gap-2">
          <Switch checked={enabled} onCheckedChange={setEnabled} />
          <span className="text-sm">启用预警</span>
        </div>
        <div className="grid grid-cols-2 gap-3">
          <div>
            <label className="text-sm text-muted-foreground">阈值（总剩余低于）</label>
            <Input value={threshold} onChange={(e) => setThreshold(e.target.value)} />
          </div>
          <div>
            <label className="text-sm text-muted-foreground">轮询间隔（秒）</label>
            <Input value={pollSecs} onChange={(e) => setPollSecs(e.target.value)} />
          </div>
        </div>
        <div>
          <label className="text-sm text-muted-foreground">主题前缀（区分实例，可选）</label>
          <Input value={prefix} onChange={(e) => setPrefix(e.target.value)} />
        </div>
        <div className="text-sm text-muted-foreground">
          上次总剩余：{status?.lastTotalRemaining?.toFixed(2) ?? '—'} ·
          上次检查：{fmtTime(status?.lastEvaluatedAt)} ·
          SMTP：{config?.smtpConfigured ? '已配置' : '未配置（通过环境变量设置）'}
        </div>
        <Button onClick={handleSave} disabled={updateConfig.isPending}>保存设置</Button>

        <div className="border-t pt-4">
          <div className="flex items-center justify-between mb-2">
            <span className="font-medium text-sm">通知渠道</span>
            <Button size="sm" variant="outline" onClick={() => { setEditing(null); setDialogOpen(true) }}>
              <Plus className="h-4 w-4 mr-1" />添加渠道
            </Button>
          </div>
          <div className="space-y-2">
            {config?.channels.map((ch) => (
              <div key={ch.id} className="flex items-center justify-between rounded border px-3 py-2 text-sm">
                <div className="flex items-center gap-2">
                  <Badge variant="outline">{ch.kind}</Badge>
                  <span>{ch.name || (ch.kind === 'telegram' ? ch.maskedBotToken : ch.to)}</span>
                  {!ch.enabled && <Badge variant="secondary">已禁用</Badge>}
                </div>
                <div className="flex gap-1">
                  <Button size="icon" variant="ghost" onClick={() => { setEditing(ch); setDialogOpen(true) }}>
                    <Pencil className="h-4 w-4" />
                  </Button>
                  <Button size="icon" variant="ghost" onClick={() => handleDelete(ch.id)}>
                    <Trash2 className="h-4 w-4" />
                  </Button>
                </div>
              </div>
            ))}
            {(!config?.channels || config.channels.length === 0) && (
              <div className="text-sm text-muted-foreground">暂无渠道</div>
            )}
          </div>
        </div>
      </CardContent>
      <AlertChannelDialog open={dialogOpen} onOpenChange={setDialogOpen} channel={editing} />
    </Card>
  )
}
