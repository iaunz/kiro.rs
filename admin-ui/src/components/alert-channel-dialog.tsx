import { useState, useEffect } from 'react'
import {
  Dialog, DialogContent, DialogHeader, DialogTitle, DialogFooter,
} from '@/components/ui/dialog'
import { Input } from '@/components/ui/input'
import { Button } from '@/components/ui/button'
import { Switch } from '@/components/ui/switch'
import { toast } from 'sonner'
import { useCreateAlertChannel, useUpdateAlertChannel } from '@/hooks/use-alerts'
import type { AlertChannelResponse, AlertChannelKind } from '@/types/api'

const MASK_PLACEHOLDER = '__unchanged__'

interface Props {
  open: boolean
  onOpenChange: (o: boolean) => void
  channel: AlertChannelResponse | null
}

export function AlertChannelDialog({ open, onOpenChange, channel }: Props) {
  const isEdit = channel !== null
  const [kind, setKind] = useState<AlertChannelKind>('telegram')
  const [name, setName] = useState('')
  const [enabled, setEnabled] = useState(true)
  const [botToken, setBotToken] = useState('')
  const [chatId, setChatId] = useState('')
  const [to, setTo] = useState('')

  const create = useCreateAlertChannel()
  const update = useUpdateAlertChannel()

  useEffect(() => {
    if (open) {
      setKind(channel?.kind ?? 'telegram')
      setName(channel?.name ?? '')
      setEnabled(channel?.enabled ?? true)
      setBotToken('') // 编辑时留空 = 不改
      setChatId(channel?.chatId ?? '')
      setTo(channel?.to ?? '')
    }
  }, [open, channel])

  const handleSubmit = async () => {
    const req = {
      kind,
      enabled,
      name: name || undefined,
      chatId: kind === 'telegram' ? chatId || undefined : undefined,
      to: kind === 'email' ? to || undefined : undefined,
      botToken:
        kind === 'telegram'
          ? (botToken || (isEdit ? MASK_PLACEHOLDER : undefined))
          : undefined,
    }
    try {
      if (isEdit && channel) {
        await update.mutateAsync({ id: channel.id, req })
      } else {
        await create.mutateAsync(req)
      }
      toast.success(isEdit ? '渠道已更新' : '渠道已添加')
      onOpenChange(false)
    } catch (e) {
      toast.error('保存失败')
    }
  }

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="sm:max-w-md">
        <DialogHeader>
          <DialogTitle>{isEdit ? '编辑渠道' : '添加渠道'}</DialogTitle>
        </DialogHeader>
        <div className="space-y-3">
          <div className="flex gap-2">
            <Button
              variant={kind === 'telegram' ? 'default' : 'outline'}
              size="sm"
              onClick={() => setKind('telegram')}
              disabled={isEdit}
            >
              Telegram
            </Button>
            <Button
              variant={kind === 'email' ? 'default' : 'outline'}
              size="sm"
              onClick={() => setKind('email')}
              disabled={isEdit}
            >
              Email
            </Button>
          </div>
          <Input placeholder="名称（可选）" value={name} onChange={(e) => setName(e.target.value)} />
          {kind === 'telegram' ? (
            <>
              <Input
                placeholder={channel?.maskedBotToken ? `当前: ${channel.maskedBotToken}（留空不改）` : 'Bot Token'}
                value={botToken}
                onChange={(e) => setBotToken(e.target.value)}
              />
              <Input placeholder="Chat ID" value={chatId} onChange={(e) => setChatId(e.target.value)} />
            </>
          ) : (
            <Input placeholder="收件邮箱" value={to} onChange={(e) => setTo(e.target.value)} />
          )}
          <div className="flex items-center gap-2">
            <Switch checked={enabled} onCheckedChange={setEnabled} />
            <span className="text-sm">启用</span>
          </div>
        </div>
        <DialogFooter>
          <Button variant="outline" onClick={() => onOpenChange(false)}>取消</Button>
          <Button onClick={handleSubmit} disabled={create.isPending || update.isPending}>
            保存
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}
