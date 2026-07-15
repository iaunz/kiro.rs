import { useState, useEffect, useRef, useCallback } from 'react'
import { toast } from 'sonner'
import { CheckCircle2, XCircle, Loader2, ExternalLink, Copy } from 'lucide-react'
import { useQueryClient } from '@tanstack/react-query'
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogFooter,
} from '@/components/ui/dialog'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { startSsoSession, getSsoSession, cancelSsoSession } from '@/api/credentials'
import { extractErrorMessage } from '@/lib/utils'
import type { SsoApiRegion, SsoSessionResponse } from '@/types/api'

interface SsoImportDialogProps {
  open: boolean
  onOpenChange: (open: boolean) => void
}

// 轮询间隔（毫秒）
const POLL_INTERVAL_MS = 3000

const STATUS_TEXT: Record<SsoSessionResponse['status'], string> = {
  pending: '等待授权',
  completed: '导入成功',
  failed: '失败',
  expired: '已超时',
  denied: '已拒绝',
  cancelled: '已取消',
}

export function SsoImportDialog({ open, onOpenChange }: SsoImportDialogProps) {
  const [startUrl, setStartUrl] = useState('')
  const [authRegion, setAuthRegion] = useState('')
  const [apiRegion, setApiRegion] = useState<SsoApiRegion>('us-east-1')
  const [priority, setPriority] = useState('0')
  const [endpoint, setEndpoint] = useState('')

  const [starting, setStarting] = useState(false)
  const [session, setSession] = useState<SsoSessionResponse | null>(null)

  const pollTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null)
  const queryClient = useQueryClient()

  const isPolling = session?.status === 'pending'

  const stopPolling = useCallback(() => {
    if (pollTimerRef.current) {
      clearTimeout(pollTimerRef.current)
      pollTimerRef.current = null
    }
  }, [])

  const resetForm = useCallback(() => {
    stopPolling()
    setStartUrl('')
    setAuthRegion('')
    setApiRegion('us-east-1')
    setPriority('0')
    setEndpoint('')
    setStarting(false)
    setSession(null)
  }, [stopPolling])

  // 轮询会话状态
  useEffect(() => {
    if (!session || session.status !== 'pending') {
      stopPolling()
      return
    }

    const poll = async () => {
      try {
        const updated = await getSsoSession(session.sessionId)
        setSession(updated)
        if (updated.status === 'completed') {
          toast.success(`SSO 凭据导入成功（#${updated.credentialId}）`)
          queryClient.invalidateQueries({ queryKey: ['credentials'] })
        } else if (updated.status === 'pending') {
          pollTimerRef.current = setTimeout(poll, POLL_INTERVAL_MS)
        } else {
          // failed / expired / denied / cancelled
          if (updated.error) toast.error(`SSO 导入${STATUS_TEXT[updated.status]}: ${updated.error}`)
        }
      } catch (error) {
        // 查询失败时继续重试（网络抖动），除非会话已不存在
        pollTimerRef.current = setTimeout(poll, POLL_INTERVAL_MS)
        console.warn('轮询 SSO 会话失败:', extractErrorMessage(error))
      }
    }

    pollTimerRef.current = setTimeout(poll, POLL_INTERVAL_MS)
    return stopPolling
  }, [session, stopPolling, queryClient])

  // 卸载时清理定时器
  useEffect(() => () => stopPolling(), [stopPolling])

  const handleStart = async (e: React.FormEvent) => {
    e.preventDefault()

    if (!startUrl.trim()) {
      toast.error('请输入 Start URL')
      return
    }
    if (!authRegion.trim()) {
      toast.error('请输入 Auth Region')
      return
    }

    setStarting(true)
    try {
      const created = await startSsoSession({
        startUrl: startUrl.trim(),
        authRegion: authRegion.trim(),
        apiRegion,
        priority: parseInt(priority) || 0,
        endpoint: endpoint.trim() || undefined,
      })
      setSession(created)
      // 自动尝试打开验证 URL
      const url = created.verificationUriComplete || created.verificationUri
      if (url) {
        window.open(url, '_blank', 'noopener,noreferrer')
      }
      toast.success('已发起 SSO 登录，请在浏览器中完成授权')
    } catch (error) {
      toast.error(`发起失败: ${extractErrorMessage(error)}`)
    } finally {
      setStarting(false)
    }
  }

  const handleCancel = async () => {
    if (!session) return
    stopPolling()
    try {
      const updated = await cancelSsoSession(session.sessionId)
      setSession(updated)
      toast.info('已取消 SSO 会话')
    } catch (error) {
      toast.error(`取消失败: ${extractErrorMessage(error)}`)
    }
  }

  const handleClose = (newOpen: boolean) => {
    if (!newOpen) {
      // 关闭时若仍在等待，取消后台会话
      if (session && session.status === 'pending') {
        cancelSsoSession(session.sessionId).catch(() => {})
      }
      resetForm()
    }
    onOpenChange(newOpen)
  }

  const copyUserCode = () => {
    if (session?.userCode) {
      navigator.clipboard?.writeText(session.userCode).then(
        () => toast.success('已复制 user code'),
        () => toast.error('复制失败')
      )
    }
  }

  const verificationUrl = session?.verificationUriComplete || session?.verificationUri

  return (
    <Dialog open={open} onOpenChange={handleClose}>
      <DialogContent className="sm:max-w-lg max-h-[85vh] flex flex-col">
        <DialogHeader>
          <DialogTitle>AWS SSO 自动导入</DialogTitle>
        </DialogHeader>

        <div className="flex flex-col min-h-0 flex-1 overflow-y-auto pr-1">
          {/* 输入表单：会话开始前显示 */}
          {!session && (
            <form onSubmit={handleStart} className="space-y-4 py-4">
              <div className="space-y-2">
                <label htmlFor="ssoStartUrl" className="text-sm font-medium">
                  Start URL <span className="text-red-500">*</span>
                </label>
                <Input
                  id="ssoStartUrl"
                  placeholder="https://<alias>.awsapps.com/start"
                  value={startUrl}
                  onChange={(e) => setStartUrl(e.target.value)}
                  disabled={starting}
                />
              </div>

              <div className="space-y-2">
                <label htmlFor="ssoAuthRegion" className="text-sm font-medium">
                  Auth Region <span className="text-red-500">*</span>
                </label>
                <Input
                  id="ssoAuthRegion"
                  placeholder="门户所在区域，如 ap-southeast-1"
                  value={authRegion}
                  onChange={(e) => setAuthRegion(e.target.value)}
                  disabled={starting}
                />
                <p className="text-xs text-muted-foreground">
                  门户 / SSO 所在区域，用于设备授权与 Token 刷新（非账号区域）
                </p>
              </div>

              <div className="space-y-2">
                <label htmlFor="ssoApiRegion" className="text-sm font-medium">
                  API Region <span className="text-red-500">*</span>
                </label>
                <select
                  id="ssoApiRegion"
                  value={apiRegion}
                  onChange={(e) => setApiRegion(e.target.value as SsoApiRegion)}
                  disabled={starting}
                  className="flex h-10 w-full rounded-md border border-input bg-background px-3 py-2 text-sm ring-offset-background focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 disabled:cursor-not-allowed disabled:opacity-50"
                >
                  <option value="us-east-1">us-east-1</option>
                  <option value="eu-central-1">eu-central-1</option>
                </select>
                <p className="text-xs text-muted-foreground">
                  用于 API 请求的区域
                </p>
              </div>

              <div className="grid grid-cols-2 gap-2">
                <div className="space-y-2">
                  <label htmlFor="ssoPriority" className="text-sm font-medium">
                    优先级
                  </label>
                  <Input
                    id="ssoPriority"
                    type="number"
                    min="0"
                    value={priority}
                    onChange={(e) => setPriority(e.target.value)}
                    disabled={starting}
                  />
                </div>
                <div className="space-y-2">
                  <label htmlFor="ssoEndpoint" className="text-sm font-medium">
                    端点
                  </label>
                  <Input
                    id="ssoEndpoint"
                    placeholder="留空使用默认"
                    value={endpoint}
                    onChange={(e) => setEndpoint(e.target.value)}
                    disabled={starting}
                  />
                </div>
              </div>
            </form>
          )}

          {/* 授权引导：会话创建后显示 */}
          {session && (
            <div className="space-y-4 py-4">
              {/* 锁定参数展示（只读） */}
              <div className="rounded-md border p-3 text-sm space-y-1 bg-muted/30">
                <div className="flex justify-between gap-2">
                  <span className="text-muted-foreground">Start URL</span>
                  <span className="font-mono truncate max-w-[60%]" title={session.startUrl}>
                    {session.startUrl}
                  </span>
                </div>
                <div className="flex justify-between gap-2">
                  <span className="text-muted-foreground">Auth Region</span>
                  <span className="font-mono">{session.authRegion}</span>
                </div>
                <div className="flex justify-between gap-2">
                  <span className="text-muted-foreground">API Region</span>
                  <span className="font-mono">{session.apiRegion}</span>
                </div>
                <p className="text-xs text-muted-foreground pt-1">
                  以上参数已在后端锁定，无法修改
                </p>
              </div>

              {/* 等待授权 */}
              {session.status === 'pending' && (
                <div className="space-y-3">
                  <div className="space-y-2">
                    <label className="text-sm font-medium">User Code</label>
                    <div className="flex items-center gap-2">
                      <code className="flex-1 rounded-md border bg-background px-3 py-2 text-lg font-mono tracking-widest text-center">
                        {session.userCode}
                      </code>
                      <Button type="button" variant="outline" size="icon" onClick={copyUserCode}>
                        <Copy className="h-4 w-4" />
                      </Button>
                    </div>
                    <p className="text-xs text-muted-foreground">
                      请在浏览器打开的页面中核对此 code 一致，然后登录并点击 Allow access
                    </p>
                  </div>

                  {verificationUrl && (
                    <Button
                      type="button"
                      variant="outline"
                      className="w-full"
                      onClick={() =>
                        window.open(verificationUrl, '_blank', 'noopener,noreferrer')
                      }
                    >
                      <ExternalLink className="h-4 w-4 mr-2" />
                      重新打开登录页面
                    </Button>
                  )}

                  <div className="flex items-center gap-2 text-sm text-muted-foreground">
                    <Loader2 className="h-4 w-4 animate-spin" />
                    等待你在浏览器完成登录并批准…
                  </div>
                </div>
              )}

              {/* 成功 */}
              {session.status === 'completed' && (
                <div className="flex items-start gap-3 rounded-md border border-green-500/40 bg-green-500/10 p-3">
                  <CheckCircle2 className="h-5 w-5 text-green-500 shrink-0 mt-0.5" />
                  <div className="text-sm">
                    <div className="font-medium">导入成功</div>
                    <div className="text-muted-foreground mt-1">
                      已添加凭据 #{session.credentialId}
                      {session.email ? `（${session.email}）` : ''}
                    </div>
                  </div>
                </div>
              )}

              {/* 失败 / 超时 / 拒绝 / 取消 */}
              {(session.status === 'failed' ||
                session.status === 'expired' ||
                session.status === 'denied' ||
                session.status === 'cancelled') && (
                <div className="flex items-start gap-3 rounded-md border border-red-500/40 bg-red-500/10 p-3">
                  <XCircle className="h-5 w-5 text-red-500 shrink-0 mt-0.5" />
                  <div className="text-sm">
                    <div className="font-medium">{STATUS_TEXT[session.status]}</div>
                    {session.error && (
                      <div className="text-muted-foreground mt-1 break-all">{session.error}</div>
                    )}
                  </div>
                </div>
              )}
            </div>
          )}
        </div>

        <DialogFooter>
          {!session && (
            <>
              <Button
                type="button"
                variant="outline"
                onClick={() => handleClose(false)}
                disabled={starting}
              >
                取消
              </Button>
              <Button type="button" onClick={handleStart} disabled={starting}>
                {starting ? '发起中...' : '发起登录'}
              </Button>
            </>
          )}

          {session && isPolling && (
            <>
              <Button type="button" variant="outline" onClick={handleCancel}>
                取消会话
              </Button>
            </>
          )}

          {session && !isPolling && (
            <>
              {session.status !== 'completed' && (
                <Button type="button" variant="outline" onClick={resetForm}>
                  重新发起
                </Button>
              )}
              <Button type="button" onClick={() => handleClose(false)}>
                完成
              </Button>
            </>
          )}
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}
