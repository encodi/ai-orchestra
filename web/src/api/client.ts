import type {
  AiSummary, Analytics, Capabilities, CharactersFile, CharactersResponse, CreateRequest, CreateResponse, DryRunResponse, EventRecord, Health, HistoryResponse, MemoryList,
  OneResponse, ProvidersResponse, ReportsResponse, ResearchResponse, SettingsObj, Timeline, UiPrefs,
} from './types'

export class ApiError extends Error {
  kind: string
  status: number
  field?: string
  constructor(status: number, kind: string, message: string, field?: string) {
    super(message)
    this.name = 'ApiError'
    this.status = status
    this.kind = kind
    this.field = field
  }
}

function token(): string {
  return document.querySelector('meta[name="aio-token"]')?.getAttribute('content') ?? ''
}

type Opts = { method?: string; body?: unknown; signal?: AbortSignal }

export async function request<T>(path: string, opts: Opts = {}): Promise<T> {
  const method = opts.method ?? 'GET'
  const headers: Record<string, string> = { Accept: 'application/json' }
  let body: string | undefined
  if (method !== 'GET') {
    headers['X-AIO-Token'] = token()
    if (opts.body !== undefined) {
      headers['Content-Type'] = 'application/json'
      body = JSON.stringify(opts.body)
    }
  }
  let res: Response
  try {
    res = await fetch(`/api/v1${path}`, { method, headers, body, signal: opts.signal })
  } catch (e) {
    if (e instanceof DOMException && e.name === 'AbortError') throw e
    throw new ApiError(0, 'network', 'The local server did not answer. Is `ai-orchestra web` still running?')
  }
  const text = await res.text()
  let json: unknown = null
  try {
    json = text ? JSON.parse(text) : null
  } catch {
    /* non-JSON error body */
  }
  if (!res.ok) {
    const err = (json as { error?: { kind?: string; message?: string; field?: string } } | null)?.error
    throw new ApiError(res.status, err?.kind ?? 'internal', err?.message ?? `HTTP ${res.status}`, err?.field)
  }
  return json as T
}

const q = (params: Record<string, string | number | boolean | undefined | null>): string => {
  const p = new URLSearchParams()
  for (const [k, v] of Object.entries(params)) if (v !== undefined && v !== null && v !== '') p.set(k, String(v))
  const s = p.toString()
  return s ? `?${s}` : ''
}

export const api = {
  health: () => request<Health>('/health'),
  capabilities: () => request<Capabilities>('/capabilities'),
  getSettings: () => request<{ defaults: SettingsObj; path_known: boolean; note: string }>('/settings'),
  putSettings: (defaults: SettingsObj) => request<{ defaults: SettingsObj }>('/settings', { method: 'PUT', body: { defaults } }),
  getUi: () => request<{ preferences: UiPrefs }>('/ui-preferences'),
  putUi: (p: UiPrefs) => request<{ preferences: UiPrefs }>('/ui-preferences', { method: 'PUT', body: p }),
  providers: () => request<ProvidersResponse>('/providers'),
  create: (r: CreateRequest) => request<CreateResponse>('/consultations', { method: 'POST', body: r }),
  plan: (r: CreateRequest) => request<DryRunResponse>('/consultations', { method: 'POST', body: { ...r, dry_run: true } }),
  one: (id: string) => request<OneResponse>(`/consultations/${encodeURIComponent(id)}`),
  cancel: (id: string) => request<{ job_id: string; cancelling: boolean; note: string }>(`/consultations/${encodeURIComponent(id)}/cancel`, { method: 'POST' }),
  remove: (id: string) => request<{ deleted: string; note: string }>(`/consultations/${encodeURIComponent(id)}?confirm=true`, { method: 'DELETE' }),
  theater: (id: string) => request<Timeline>(`/consultations/${encodeURIComponent(id)}/theater`),
  research: (id: string) => request<ResearchResponse>(`/consultations/${encodeURIComponent(id)}/research`),
  feedback: (id: string, rating: string, note: string) =>
    request<{ feedback_id: string; note: string }>(`/consultations/${encodeURIComponent(id)}/feedback`, { method: 'POST', body: { rating, note } }),
  history: (p: { q?: string; mode?: string; provider?: string; strategy?: string; status?: string; from?: number; to?: number; limit?: number; offset?: number }) =>
    request<HistoryResponse>(`/consultations${q(p)}`),
  sessions: () => request<{ items: Record<string, unknown>[]; note: string }>('/sessions'),
  session: (id: string) => request<Record<string, unknown>>(`/sessions/${encodeURIComponent(id)}`),
  clearSession: (id: string) => request<{ cleared: string; memories_deleted: number }>(`/sessions/${encodeURIComponent(id)}?confirm=true`, { method: 'DELETE' }),
  memory: (p: { q?: string; scope?: string; kind?: string; all_versions?: boolean; reveal?: boolean }) => request<MemoryList>(`/memory${q(p)}`),
  memoryShow: (id: string, reveal = false) => request<Record<string, unknown>>(`/memory/${encodeURIComponent(id)}${q({ reveal })}`),
  memoryAdd: (b: { text: string; scope?: string; kind?: string; topic?: string; tags?: string[]; allow_sensitive?: boolean; confirm: boolean }) =>
    request<{ id: string; duplicate: boolean }>('/memory', { method: 'POST', body: b }),
  memoryCorrect: (id: string, text: string) => request<{ id: string }>(`/memory/${encodeURIComponent(id)}`, { method: 'PUT', body: { text, confirm: true } }),
  memoryForget: (id: string) => request<{ deleted: string }>(`/memory/${encodeURIComponent(id)}?confirm=true`, { method: 'DELETE' }),
  memoryExport: () => request<Record<string, unknown>>('/memory/export'),
  contextPreview: (b: { question: string; session_settings?: SettingsObj; settings?: SettingsObj; session?: string | null; new_session?: boolean }) =>
    request<{ report: unknown; block: string; note: string }>('/context/preview', { method: 'POST', body: b }),
  reports: () => request<ReportsResponse>('/reports'),
  report: (id: string) => request<Record<string, unknown>>(`/reports/${encodeURIComponent(id)}`),
  analytics: () => request<Analytics>('/analytics'),
  characters: () => request<CharactersResponse>('/characters'),
  putCharacters: (c: CharactersFile) => request<{ settings: CharactersFile }>('/characters', { method: 'PUT', body: c }),
  summarize: (id: string, agent?: string, signal?: AbortSignal) =>
    request<{ summary: AiSummary; note: string }>(`/consultations/${encodeURIComponent(id)}/summary`, { method: 'POST', body: agent ? { agent } : {}, signal }),
}

export const TERMINAL_EVENTS = ['consultation_completed', 'consultation_failed', 'consultation_cancelled']

/**
 * Follows a job's recorded events. EventSource reconnects by itself and resends `Last-Event-ID`; we also
 * drop anything already seen, so a reconnect can never show an event twice. Reading events never starts
 * a call on the server. The stream is closed after the terminal event (otherwise EventSource would
 * reconnect to a finished stream forever).
 */
export function followEvents(
  jobId: string,
  onEvent: (e: EventRecord) => void,
  onState: (s: 'open' | 'reconnecting' | 'closed') => void,
  after?: number,
): () => void {
  let last = after ?? -1
  const url = `/api/v1/consultations/${encodeURIComponent(jobId)}/events${after !== undefined ? `?after=${after}` : ''}`
  const es = new EventSource(url)
  const kinds = [
    'consultation_created', 'planning_started', 'strategy_selected', 'provider_started', 'provider_completed', 'agent_message_recorded',
    'research_note', 'memory_note', 'consultation_completed', 'consultation_failed', 'consultation_cancelled',
  ]
  const handle = (ev: MessageEvent) => {
    let rec: EventRecord
    try {
      rec = JSON.parse(ev.data) as EventRecord
    } catch {
      return
    }
    if (rec.seq <= last) return
    last = rec.seq
    onEvent(rec)
    if (TERMINAL_EVENTS.includes(rec.kind)) {
      es.close()
      onState('closed')
    }
  }
  for (const k of kinds) es.addEventListener(k, handle as EventListener)
  es.onopen = () => onState('open')
  es.onerror = () => {
    if (es.readyState === EventSource.CLOSED) onState('closed')
    else onState('reconnecting')
  }
  return () => {
    es.close()
    onState('closed')
  }
}
