import { useState } from 'react'
import { toast } from 'sonner'
import { Button } from '@/components/ui/button'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import { useSetRemark } from '@/hooks/use-credentials'
import { extractErrorMessage } from '@/lib/utils'
import type { CredentialStatusItem } from '@/types/api'

interface CredentialRemarkDialogProps {
  credential: CredentialStatusItem
  onOpenChange: (open: boolean) => void
}

export function CredentialRemarkDialog({ credential, onOpenChange }: CredentialRemarkDialogProps) {
  const [remark, setRemark] = useState(credential.remark ?? '')
  const setRemarkMutation = useSetRemark()

  const handleSubmit = (event: React.FormEvent) => {
    event.preventDefault()
    setRemarkMutation.mutate(
      { id: credential.id, remark },
      {
        onSuccess: (response) => {
          toast.success(response.message)
          onOpenChange(false)
        },
        onError: (error) => toast.error(`保存失败: ${extractErrorMessage(error)}`),
      }
    )
  }

  return (
    <Dialog open onOpenChange={(open) => {
      if (!setRemarkMutation.isPending) onOpenChange(open)
    }}>
      <DialogContent className="w-[calc(100%-2rem)] sm:max-w-lg">
        <DialogHeader>
          <DialogTitle>编辑凭据备注</DialogTitle>
          <DialogDescription>
            {credential.email || `凭据 #${credential.id}`}，留空保存可清除备注。
          </DialogDescription>
        </DialogHeader>
        <form onSubmit={handleSubmit} className="space-y-4">
          <div className="space-y-2">
            <label htmlFor="credential-remark" className="text-sm font-medium">备注</label>
            <textarea
              id="credential-remark"
              className="flex min-h-28 w-full rounded-md border border-input bg-background px-3 py-2 text-sm ring-offset-background placeholder:text-muted-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 disabled:opacity-50"
              rows={4}
              placeholder="例如：账号用途、所属项目或续费信息"
              value={remark}
              onChange={(event) => setRemark(event.target.value)}
              disabled={setRemarkMutation.isPending}
            />
          </div>
          <DialogFooter>
            <Button type="button" variant="outline" onClick={() => onOpenChange(false)} disabled={setRemarkMutation.isPending}>
              取消
            </Button>
            <Button type="submit" disabled={setRemarkMutation.isPending}>
              {setRemarkMutation.isPending ? '保存中...' : '保存'}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  )
}
