export function formatDuration(ms: number | null | undefined): string {
  if (ms === null || ms === undefined || Number.isNaN(ms)) return '–'
  if (ms < 1000) return `${Math.round(ms)} ms`
  const s = ms / 1000
  if (s < 60) return `${s < 10 ? s.toFixed(1) : Math.round(s)} s`
  const m = Math.floor(s / 60)
  return `${m} min ${Math.round(s - m * 60)} s`
}

export function formatDateTime(ms: number | null | undefined, lang: string): string {
  if (!ms) return '–'
  return new Date(ms).toLocaleString(lang === 'en' ? 'en-US' : 'es-MX', { dateStyle: 'medium', timeStyle: 'short' })
}

export function formatClock(ms: number, lang: string): string {
  return new Date(ms).toLocaleTimeString(lang === 'en' ? 'en-US' : 'es-MX', { hour: '2-digit', minute: '2-digit', second: '2-digit' })
}

export function formatOffset(ms: number): string {
  const s = Math.floor(ms / 1000)
  const m = Math.floor(s / 60)
  return `${String(m).padStart(2, '0')}:${String(s % 60).padStart(2, '0')}`
}

export function truncate(text: string, max: number): string {
  const flat = text.replace(/\s+/g, ' ').trim()
  return flat.length <= max ? flat : `${flat.slice(0, max - 1)}…`
}

/** Only http(s) links are ever followed from rendered text; anything else is shown as plain text. */
export function safeHttpUrl(url: string): string | null {
  try {
    const u = new URL(url)
    return u.protocol === 'https:' || u.protocol === 'http:' ? u.toString() : null
  } catch {
    return null
  }
}

export function agentKey(agent: string): 'claude' | 'codex' | 'google' | 'other' {
  const a = agent.toLowerCase()
  if (a.startsWith('claude')) return 'claude'
  if (a.startsWith('codex')) return 'codex'
  if (a.startsWith('google') || a.startsWith('agy') || a.startsWith('antigravity') || a.startsWith('gemini')) return 'google'
  return 'other'
}

export function download(filename: string, mime: string, content: string): void {
  const url = URL.createObjectURL(new Blob([content], { type: mime }))
  const a = document.createElement('a')
  a.href = url
  a.download = filename
  document.body.appendChild(a)
  a.click()
  a.remove()
  setTimeout(() => URL.revokeObjectURL(url), 1000)
}

/** A one-line preview of Markdown text: the markers are dropped so it reads as plain words. */
export function plainPreview(text: string, max: number): string {
  const flat = text
    .replace(/```[\s\S]*?```/g, ' ')
    .replace(/^\s*#{1,6}\s*/gm, '')
    .replace(/\|[-:\s|]+\|/g, ' ')
    .replace(/[*_`>|]/g, '')
    .replace(/\s+/g, ' ')
    .trim()
  return flat.length <= max ? flat : `${flat.slice(0, max - 1)}…`
}
