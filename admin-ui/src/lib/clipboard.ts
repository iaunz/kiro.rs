/** 同时支持 HTTPS Clipboard API 和 HTTP 页面中的传统复制。 */
export async function copyToClipboard(text: string): Promise<void> {
  if (navigator.clipboard?.writeText) {
    try {
      await navigator.clipboard.writeText(text)
      return
    } catch {
      // 权限或浏览器限制时继续尝试传统复制。
    }
  }

  const previouslyFocused = document.activeElement
  const textarea = document.createElement('textarea')
  textarea.value = text
  textarea.readOnly = true
  textarea.style.position = 'fixed'
  textarea.style.opacity = '0'
  document.body.appendChild(textarea)
  try {
    textarea.focus()
    textarea.select()
    if (!document.execCommand('copy')) {
      throw new Error('浏览器未允许自动复制，请手动复制')
    }
  } finally {
    textarea.remove()
    if (previouslyFocused instanceof HTMLElement) {
      previouslyFocused.focus({ preventScroll: true })
    }
  }
}
