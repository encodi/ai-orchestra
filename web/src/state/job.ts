// Turns the recorded event stream of one consultation into what the interface shows. Every field is
// derived from an event the server recorded: there is no estimate of progress and no invented message.
import { useEffect, useReducer, useRef } from 'react'
import { ApiError, api, followEvents } from '../api/client'
import type { Consultation, EventRecord, InForce } from '../api/types'
import type { RobotState } from '../components/PixelRobot'

export type StageId = 'planning' | 'research' | 'independent_analysis' | 'review' | 'cross_review' | 'synthesis'
export const STAGES: StageId[] = ['planning', 'research', 'independent_analysis', 'review', 'cross_review', 'synthesis']
export type StageState = 'pending' | 'active' | 'done'

export type AgentLive = {
  id: string
  state: RobotState
  role?: string
  stage?: string
  calls: number
  failed: number
  error?: string
  lastDurationMs?: number
}
export type LiveMessage = {
  seq: number
  ts_ms: number
  agent: string
  role: string
  stage: string
  round: number
  technical: boolean
  text: string
  duration_ms: number
}
export type LiveFailure = { seq: number; ts_ms: number; agent: string; role: string; stage: string; error: string; duration_ms: number }

export type JobPhase = 'loading' | 'running' | 'completed' | 'partial' | 'failed' | 'cancelled' | 'gone'
export type JobView = {
  phase: JobPhase
  stream: 'connecting' | 'open' | 'reconnecting' | 'closed'
  events: EventRecord[]
  seen: number
  agents: Record<string, AgentLive>
  order: string[]
  coordinator?: string
  mode?: string
  rounds?: number
  skipped: { agent: string; reason: string }[]
  stages: Record<StageId, StageState>
  messages: LiveMessage[]
  failures: LiveFailure[]
  notes: string[]
  planText?: string
  inForce?: InForce
  consultationId?: string
  calls?: number
  durationMs?: number
  warnings: string[]
  error?: { kind: string; message: string }
  consultation?: Consultation
  firstTs?: number
  lastTs?: number
}

export function emptyView(): JobView {
  return {
    phase: 'loading', stream: 'connecting', events: [], seen: -1, agents: {}, order: [], skipped: [],
    stages: { planning: 'pending', research: 'pending', independent_analysis: 'pending', review: 'pending', cross_review: 'pending', synthesis: 'pending' },
    messages: [], failures: [], notes: [], warnings: [],
  }
}

const str = (v: unknown): string => (typeof v === 'string' ? v : '')
const num = (v: unknown): number => (typeof v === 'number' ? v : 0)

export function robotStateFor(stage: string): RobotState {
  if (stage === 'synthesis') return 'speaking'
  if (stage === 'review' || stage === 'cross_review') return 'reviewing'
  return 'thinking'
}

function ensureAgent(v: JobView, id: string): AgentLive {
  let a = v.agents[id]
  if (!a) {
    a = { id, state: 'waiting', calls: 0, failed: 0 }
    v.agents[id] = a
    if (!v.order.includes(id)) v.order.push(id)
  }
  return a
}

/** Applies one recorded event. Pure and idempotent per `seq`: an event already applied is ignored. */
export function applyEvent(prev: JobView, ev: EventRecord): JobView {
  if (ev.seq <= prev.seen) return prev
  const v: JobView = {
    ...prev, events: [...prev.events, ev], seen: ev.seq, agents: { ...prev.agents }, order: [...prev.order],
    stages: { ...prev.stages }, messages: [...prev.messages], failures: [...prev.failures], notes: [...prev.notes],
    firstTs: prev.firstTs ?? ev.ts_ms, lastTs: ev.ts_ms,
  }
  for (const k of Object.keys(v.agents)) v.agents[k] = { ...(v.agents[k] as AgentLive) }
  if (v.phase === 'loading') v.phase = 'running'
  const d = ev.data
  switch (ev.kind) {
    case 'planning_started':
      v.stages.planning = 'active'
      v.inForce = d.settings as InForce | undefined
      break
    case 'strategy_selected':
      v.stages.planning = 'done'
      v.planText = str(d.plan_text) || v.planText
      break
    case 'consultation_created': {
      v.consultationId = str(d.consultation_id)
      v.mode = str(d.mode)
      v.coordinator = str(d.coordinator)
      v.rounds = num(d.rounds)
      const parts = Array.isArray(d.participants) ? (d.participants as string[]) : []
      for (const p of [...parts, v.coordinator]) if (p) ensureAgent(v, p)
      v.skipped = Array.isArray(d.skipped) ? (d.skipped as { agent: string; reason: string }[]) : []
      break
    }
    case 'provider_started': {
      const id = str(d.agent)
      const a = ensureAgent(v, id)
      const stage = str(d.stage)
      a.state = robotStateFor(stage)
      a.role = str(d.role)
      a.stage = stage
      if (stage in v.stages) v.stages[stage as StageId] = 'active'
      break
    }
    case 'provider_completed': {
      const id = str(d.agent)
      const a = ensureAgent(v, id)
      a.calls += 1
      a.lastDurationMs = num(d.duration_ms)
      const stage = str(d.stage)
      if (d.ok === true) a.state = 'finished'
      else {
        a.state = 'error'
        a.failed += 1
        a.error = str(d.error) || 'failed'
        v.failures.push({ seq: ev.seq, ts_ms: ev.ts_ms, agent: id, role: str(d.role), stage, error: a.error, duration_ms: num(d.duration_ms) })
      }
      // A stage is done when none of its calls is still running.
      const stillRunning = Object.values(v.agents).some((x) => x.id !== id && x.stage === stage && (x.state === 'thinking' || x.state === 'reviewing' || x.state === 'speaking'))
      if (stage in v.stages && !stillRunning) v.stages[stage as StageId] = 'done'
      break
    }
    case 'agent_message_recorded':
      v.messages.push({
        seq: ev.seq, ts_ms: ev.ts_ms, agent: str(d.agent), role: str(d.role), stage: str(d.stage), round: num(d.round),
        technical: d.technical === true, text: str(d.text), duration_ms: num(d.duration_ms),
      })
      break
    case 'research_note':
      v.stages.research = v.stages.research === 'pending' ? 'active' : v.stages.research
      v.notes.push(str(d.message))
      break
    case 'memory_note':
      v.notes.push(str(d.message))
      break
    case 'consultation_completed':
    case 'consultation_failed':
    case 'consultation_cancelled': {
      const status = str(d.status)
      v.phase = status === 'completed' ? 'completed' : status === 'partial' ? 'partial' : status === 'cancelled' ? 'cancelled' : 'failed'
      v.consultationId = str(d.consultation_id) || v.consultationId
      v.calls = typeof d.calls === 'number' ? d.calls : v.calls
      v.durationMs = typeof d.duration_ms === 'number' ? d.duration_ms : v.durationMs
      v.warnings = Array.isArray(d.warnings) ? (d.warnings as string[]) : []
      if (d.error && typeof d.error === 'object') v.error = d.error as { kind: string; message: string }
      for (const a of Object.values(v.agents)) {
        if (a.state === 'thinking' || a.state === 'reviewing' || a.state === 'speaking' || a.state === 'waiting') a.state = v.phase === 'cancelled' ? 'idle' : a.failed > 0 ? 'error' : 'finished'
      }
      for (const k of STAGES) if (v.stages[k] === 'active') v.stages[k] = v.phase === 'cancelled' || v.phase === 'failed' ? 'pending' : 'done'
      break
    }
    default:
      break
  }
  return v
}

export function reduceEvents(events: EventRecord[]): JobView {
  return events.reduce(applyEvent, emptyView())
}

type Action =
  | { t: 'event'; ev: EventRecord }
  | { t: 'stream'; s: JobView['stream'] }
  | { t: 'consultation'; c: Consultation }
  | { t: 'gone'; error?: string }
  | { t: 'reset' }

function reducer(state: JobView, a: Action): JobView {
  switch (a.t) {
    case 'event':
      return applyEvent(state, a.ev)
    case 'stream':
      return { ...state, stream: a.s }
    case 'consultation': {
      const phase: JobPhase = state.phase === 'loading' || state.phase === 'running' || state.phase === 'gone' ? mapStatus(a.c.status) : state.phase
      return { ...state, consultation: a.c, consultationId: a.c.id, phase }
    }
    case 'gone':
      return { ...state, phase: state.phase === 'running' || state.phase === 'loading' ? 'gone' : state.phase, error: a.error ? { kind: 'gone', message: a.error } : state.error }
    case 'reset':
      return emptyView()
  }
}

function mapStatus(s: string): JobPhase {
  return s === 'complete' ? 'completed' : s === 'partial' ? 'partial' : s === 'cancelled' ? 'cancelled' : s === 'failed' ? 'failed' : 'completed'
}

const TERMINAL = new Set(['completed', 'partial', 'failed', 'cancelled'])

/**
 * Follows a consultation. Mounting (including a page refresh) only reads recorded events, so it can never
 * start a call. If the server no longer remembers the job (it was restarted), the saved record is shown.
 */
export function useJob(jobId: string, knownConsultationId?: string): JobView {
  const [view, dispatch] = useReducer(reducer, undefined, emptyView)
  // Only a fallback for when the server no longer knows the job; changing it must not reopen the stream.
  const known = useRef(knownConsultationId)
  known.current = knownConsultationId

  useEffect(() => {
    dispatch({ t: 'reset' })
    let alive = true
    let terminalSeen = false
    const loadSaved = async (id: string) => {
      try {
        const r = await api.one(id)
        if (alive && r.consultation) dispatch({ t: 'consultation', c: r.consultation })
        return Boolean(r.consultation)
      } catch {
        return false
      }
    }
    const stop = followEvents(
      jobId,
      (ev) => {
        if (!alive) return
        dispatch({ t: 'event', ev })
        if (ev.kind.startsWith('consultation_') && ev.kind !== 'consultation_created') {
          terminalSeen = true
          const cid = typeof ev.data.consultation_id === 'string' ? ev.data.consultation_id : undefined
          // The full record (answer, sources count, settings) is served by the job itself or the saved file.
          void api
            .one(jobId)
            .then((r) => alive && r.consultation && dispatch({ t: 'consultation', c: r.consultation }))
            .catch(async (e: unknown) => {
              if (e instanceof ApiError && cid) await loadSaved(cid)
            })
        }
      },
      (s) => {
        if (!alive) return
        dispatch({ t: 'stream', s })
        if (s === 'closed' && !terminalSeen) {
          // The stream ended without a final event: the server no longer knows this job.
          void (async () => {
            const ok = known.current ? await loadSaved(known.current) : false
            if (!ok && alive) dispatch({ t: 'gone' })
          })()
        }
      },
    )
    return () => {
      alive = false
      stop()
    }
  }, [jobId])

  return view
}

export const isFinished = (p: JobPhase): boolean => TERMINAL.has(p)
