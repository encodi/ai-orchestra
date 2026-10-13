// One question and everything the server recorded about answering it: live progress, then the result.
import { useCallback, useEffect, useMemo, useState } from 'react'
import { ApiError, api } from '../api/client'
import type { Consultation, ResearchResponse, Timeline } from '../api/types'
import { useI18n, type TKey } from '../i18n'
import { Markdown } from '../lib/markdown'
import { download, formatDuration, formatDateTime } from '../lib/format'
import { isFinished, STAGES, useJob, type JobView, type StageId } from '../state/job'
import { useStore, type Turn } from '../state/store'
import { AgentTag, STAGE_KEY, useAgentName } from './agents'
import { Icon } from './Icons'
import { Conversation, fromTimeline, type TMsg } from './Bubble'
import { Stage, activityOfView, recentSpeech, useMotion, type StageCast } from './Stage'
import { SummaryBox } from './SummaryBox'
import { usePrefs } from '../state/prefs'
import { ResearchView } from './ResearchView'
import { Collapsible, Notice, Pill, Spinner, TabPanel, Tabs, statusTone } from './ui'

function useNow(active: boolean, ms = 1000): number {
  const [now, setNow] = useState(Date.now())
  useEffect(() => {
    if (!active) return
    const id = window.setInterval(() => setNow(Date.now()), ms)
    return () => window.clearInterval(id)
  }, [active, ms])
  return now
}

const ERROR_HINT: Record<string, TKey> = {
  provider_quota: 'err.quota', provider_auth: 'err.auth', provider_timeout: 'err.timeout', provider_missing: 'err.missing', validation: 'err.validation', cancelled: 'err.cancelled',
}

/** Theatre-flavoured wording for the stage that is really running. Decoration only: it says nothing about how far along the work is. */
const SHOW_COUNT: Record<string, number> = {"planning": 4, "research": 4, "independent_analysis": 5, "review": 4, "cross_review": 3, "synthesis": 4, "cancelling": 1}
const SHOW_STAGE: Record<string, string> = { idle: 'planning', planning: 'planning', research: 'research', independent_analysis: 'independent_analysis', review: 'review', cross_review: 'cross_review', synthesis: 'synthesis', done: 'synthesis' }

function useShowLine(stageKey: string, cancelling: boolean): string {
  const { t } = useI18n()
  const motion = useMotion()
  const [i, setI] = useState(0)
  useEffect(() => {
    if (!motion || cancelling) return
    const id = window.setInterval(() => setI((x) => x + 1), 3600)
    return () => window.clearInterval(id)
  }, [motion, cancelling])
  const key = cancelling ? 'cancelling' : stageKey
  const n = SHOW_COUNT[key] ?? 1
  return t(`show.${key}.${(i % n) + 1}` as TKey)
}

function ProgressPanel({ turn, view }: { turn: Turn; view: JobView }) {
  const { t } = useI18n()
  const name = useAgentName()
  const now = useNow(!isFinished(view.phase))
  const [cancelState, setCancelState] = useState<'idle' | 'sending' | 'sent' | 'error'>('idle')
  const [cancelError, setCancelError] = useState('')
  const elapsed = now - turn.startedMs
  const cancel = async () => {
    setCancelState('sending')
    try {
      await api.cancel(turn.jobId)
      setCancelState('sent')
    } catch (e) {
      setCancelState('error')
      setCancelError(e instanceof Error ? e.message : String(e))
    }
  }
  const stageKey = SHOW_STAGE[activityOfView(view)] ?? 'planning'
  const showLine = useShowLine(stageKey, cancelState === 'sent')
  const shown: StageId[] = STAGES.filter((s) => view.stages[s] !== 'pending' || s === 'independent_analysis' || s === 'synthesis')
  return (
    <section className="card progress" aria-live="polite" aria-label={t('progress.title')}>
      <div className="row wrap" style={{ gap: 12 }}>
        <h3 className="sr-only">{t('progress.sr_title', { stage: t(STAGE_KEY[stageKey] as TKey) })}</h3>
        <span className="show-line" aria-hidden="true">{showLine}</span>
        <span className="faint mono" aria-label={t('progress.elapsed')}>{formatDuration(elapsed)}</span>
        {view.stream === 'reconnecting' && <Pill tone="warn">{t('progress.reconnecting')}</Pill>}
        <span className="spacer" />
        <button type="button" className="btn danger sm" onClick={cancel} disabled={cancelState === 'sending' || cancelState === 'sent'}>
          <Icon name="stop" size={14} /> {cancelState === 'sent' ? t('progress.cancelling') : t('progress.cancel')}
        </button>
      </div>
      {cancelState === 'error' && <Notice tone="danger">{cancelError}</Notice>}
      {view.mode && (
        <p className="muted" style={{ margin: '8px 0 0' }}>
          {t('progress.plan', { mode: view.mode, coordinator: name(view.coordinator ?? ''), n: Math.max(0, view.order.length) })}
        </p>
      )}
      <ol className="phases" aria-label={t('progress.stages')}>
        {shown.map((s) => (
          <li key={s} className={`phase phase-${view.stages[s]}`} aria-current={view.stages[s] === 'active' ? 'step' : undefined}>
            <span className="phase-dot" aria-hidden="true" />
            <span>{t(STAGE_KEY[s] as TKey)}</span>
            <span className="sr-only">{view.stages[s]}</span>
          </li>
        ))}
      </ol>
      <div className="agent-row">
        {view.order.map((id) => {
          const a = view.agents[id]
          if (!a) return null
          return <AgentTag key={id} agent={id} state={a.state} extra={a.stage ? ` · ${t(STAGE_KEY[a.stage] ?? 'stage.synthesis')}` : ''} />
        })}
        {view.order.length === 0 && <span className="muted">{t('progress.waiting_plan')}</span>}
      </div>
      {view.messages.length > 0 && (
        <div>
          <h4 style={{ margin: '4px 0 8px' }}>{t('progress.conversation')}</h4>
          <Conversation msgs={liveBubbles(view)} known={new Set()} />
        </div>
      )}
      {view.failures.length > 0 && (
        <Notice tone="warn">
          {view.failures.map((f) => (
            <div key={f.seq}>
              <strong>{name(f.agent)}</strong>: {f.error}
            </div>
          ))}
          <div className="hint">{t('progress.failure_note')}</div>
        </Notice>
      )}
      {view.notes.length > 0 && (
        <Collapsible title={t('progress.notes', { n: view.notes.length })} defaultOpen>
          <ul style={{ margin: 0, paddingLeft: 18 }}>{view.notes.slice(-8).map((n, i) => <li key={i}>{n}</li>)}</ul>
        </Collapsible>
      )}
      {view.planText && (
        <Collapsible title={t('progress.why')}>
          <pre className="plain-pre">{view.planText}</pre>
        </Collapsible>
      )}
      <p className="hint" style={{ margin: '8px 0 0' }}>{t('progress.no_percent')}</p>
    </section>
  )
}

type ResultTab = 'conversation' | 'answer' | 'research' | 'details'

function useLazy<T>(load: (() => Promise<T>) | null, key: string): { data: T | null; error: string | null; loading: boolean } {
  const [state, setState] = useState<{ key: string; data: T | null; error: string | null }>({ key: '', data: null, error: null })
  useEffect(() => {
    if (!load) return
    let alive = true
    load()
      .then((data) => alive && setState({ key, data, error: null }))
      .catch((e: unknown) => alive && setState({ key, data: null, error: e instanceof Error ? e.message : String(e) }))
    return () => {
      alive = false
    }
    // `load` is rebuilt every render; the key says when a reload is wanted.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [key, load === null])
  const current = state.key === key
  return { data: current ? state.data : null, error: current ? state.error : null, loading: load !== null && !(current && (state.data || state.error)) }
}

function FeedbackRow({ id }: { id: string }) {
  const { t } = useI18n()
  const [rating, setRating] = useState('')
  const [note, setNote] = useState('')
  const [msg, setMsg] = useState<{ tone: 'ok' | 'danger'; text: string } | null>(null)
  const send = async () => {
    try {
      const r = await api.feedback(id, rating, note)
      void r
      setMsg({ tone: 'ok', text: t('feedback.saved') })
      setRating('')
      setNote('')
    } catch (e) {
      setMsg({ tone: 'danger', text: e instanceof Error ? e.message : String(e) })
    }
  }
  const ratings = ['helpful', 'not-helpful', 'inaccurate', 'incomplete', 'bad-citation', 'slow', 'verbose', 'unsatisfactory']
  return (
    <div className="stack" style={{ gap: 8 }}>
      <div className="row wrap" style={{ gap: 8 }}>
        <label className="sr-only" htmlFor={`fb-${id}`}>{t('feedback.rating')}</label>
        <select id={`fb-${id}`} className="select" style={{ width: 'auto' }} value={rating} onChange={(e) => setRating(e.target.value)}>
          <option value="">{t('feedback.choose')}</option>
          {ratings.map((r) => <option key={r} value={r}>{t(`fb.${r}` as TKey)}</option>)}
        </select>
        <input className="input" style={{ flex: 1, minWidth: 160 }} placeholder={t('feedback.note')} aria-label={t('feedback.note')} value={note} maxLength={300} onChange={(e) => setNote(e.target.value)} />
        <button type="button" className="btn sm" disabled={!rating} onClick={send}>{t('feedback.send')}</button>
      </div>
      <div className="hint">{t('feedback.note_local')}</div>
      {msg && <Notice tone={msg.tone}>{msg.text}</Notice>}
    </div>
  )
}

function Details({ c, view, turn }: { c: Consultation; view: JobView; turn?: Turn }) {
  const { t, lang } = useI18n()
  const meta = (c.meta ?? {}) as Record<string, unknown>
  const tokens = meta.token_usage as { input_tokens: number; output_tokens: number; calls_reporting: number; calls_total: number } | null | undefined
  const ctx = meta.context as { session?: string | null; retrieved: number; selected: number; omitted: number; estimated_tokens: number; reduction_percent?: number | null; token_basis: string; mode: string } | null | undefined
  const stageMs = (meta.stage_ms ?? {}) as Record<string, number>
  const inv = (meta.invocations ?? {}) as Record<string, number>
  const orchestration = meta.orchestration as Record<string, unknown> | null | undefined
  return (
    <div className="stack" style={{ gap: 16 }}>
      <div className="grid2">
        <dl className="kv">
          <dt>{t('d.id')}</dt><dd className="mono">{c.id}</dd>
          <dt>{t('d.status')}</dt><dd>{c.status}</dd>
          <dt>{t('d.mode')}</dt><dd>{c.mode ?? '–'}</dd>
          <dt>{t('d.strategy')}</dt><dd>{c.strategy ?? '–'}</dd>
          <dt>{t('d.coordinator')}</dt><dd>{c.coordinator}</dd>
          <dt>{t('d.participants')}</dt><dd>{c.participants.join(', ') || '–'}</dd>
          <dt>{t('d.calls')}</dt><dd>{c.calls}{Object.keys(inv).length ? ` (${Object.entries(inv).map(([k, v]) => `${k}: ${v}`).join(', ')})` : ''}</dd>
          <dt>{t('d.duration')}</dt><dd>{formatDuration(c.duration_ms)}</dd>
          <dt>{t('d.created')}</dt><dd>{formatDateTime(c.created_at_ms, lang)}</dd>
        </dl>
        <dl className="kv">
          <dt>{t('d.tokens')}</dt>
          <dd>
            {tokens && tokens.calls_reporting > 0
              ? t('d.tokens_value', { i: tokens.input_tokens, o: tokens.output_tokens, n: tokens.calls_reporting, m: tokens.calls_total })
              : t('d.tokens_none')}
          </dd>
          {ctx && (
            <>
              <dt>{t('d.memory')}</dt>
              <dd>
                {t('d.memory_value', { mode: ctx.mode, sel: ctx.selected, ret: ctx.retrieved, tok: ctx.estimated_tokens })}
                {typeof ctx.reduction_percent === 'number' ? ` · ${t('d.reduction', { p: Math.round(ctx.reduction_percent) })}` : ''}
                <div className="hint">{t('d.estimate_note')}</div>
              </dd>
            </>
          )}
          {Object.keys(stageMs).length > 0 && (
            <>
              <dt>{t('d.stage_times')}</dt>
              <dd>{Object.entries(stageMs).map(([k, v]) => `${k}: ${formatDuration(v)}`).join(' · ')}</dd>
            </>
          )}
        </dl>
      </div>
      {view.inForce && (
        <div>
          <h4>{t('d.settings')}</h4>
          <div className="table-wrap" tabIndex={0} role="region" aria-label={t('table.scroll')}>
            <table className="table">
              <thead><tr><th>{t('d.setting')}</th><th>{t('d.value')}</th><th>{t('d.origin')}</th></tr></thead>
              <tbody>
                {Object.entries(view.inForce).map(([k, v]) => (
                  <tr key={k}>
                    <td className="mono">{k}</td>
                    <td>{v.value === null || v.value === undefined ? <span className="faint">AUTO</span> : Array.isArray(v.value) ? v.value.join(', ') : String(v.value)}</td>
                    <td>{v.origin_label}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        </div>
      )}
      {(view.planText || c.routing_text) && (
        <Collapsible title={t('d.routing')}>
          <pre className="plain-pre">{view.planText ?? c.routing_text}</pre>
        </Collapsible>
      )}
      {orchestration && (
        <Collapsible title={t('d.orchestration')}>
          <pre className="plain-pre">{JSON.stringify(orchestration, null, 2)}</pre>
        </Collapsible>
      )}
      {c.warnings.length > 0 && (
        <Notice tone="warn">
          <strong>{t('d.warnings')}</strong>
          <ul style={{ margin: '4px 0 0', paddingLeft: 18 }}>{c.warnings.map((w, i) => <li key={i}>{w}</li>)}</ul>
        </Notice>
      )}
      {turn && <p className="hint">{t('d.question_sent', { n: Object.keys(turn.requestSettings).length })}</p>}
    </div>
  )
}

/** What the live messages of a run look like as bubbles (times are offsets from the first recorded event). */
export function liveBubbles(view: JobView): TMsg[] {
  const first = view.firstTs ?? 0
  const ok: (TMsg & { seq: number })[] = view.messages
    .filter((m) => !m.technical)
    .map((m) => ({ key: `l${m.seq}`, seq: m.seq, agent: m.agent, role: m.role, stage: m.stage, round: m.round, ok: true, text: m.text, error: null, technical: false, durationMs: m.duration_ms, offsetMs: m.ts_ms - first, startMs: m.ts_ms - first - m.duration_ms, endMs: m.ts_ms - first }))
  const bad = view.failures.map((f) => ({ key: `f${f.seq}`, seq: f.seq, agent: f.agent, role: f.role, stage: f.stage, round: 0, ok: false, text: null, error: f.error, technical: false, durationMs: f.duration_ms, offsetMs: f.ts_ms - first, startMs: f.ts_ms - first - f.duration_ms, endMs: f.ts_ms - first }))
  return [...ok, ...bad].sort((x, y) => x.seq - y.seq)
}

function castOf(view: JobView, c: Consultation | undefined): StageCast[] {
  if (view.order.length > 0) return view.order.map((id) => ({ id, state: view.agents[id]?.state ?? 'idle', coordinator: id === view.coordinator }))
  if (c) return [...c.participants, c.coordinator].filter((v, i, a) => a.indexOf(v) === i).map((id) => ({ id, state: 'finished' as const, coordinator: id === c.coordinator }))
  return []
}

/** The animated stage above a consultation: decoration driven by the real stage; its speech is recorded messages. */
function StageBlock({ view, c }: { view: JobView; c: Consultation | undefined }) {
  const { t } = useI18n()
  const name = useAgentName()
  const cast = castOf(view, c)
  const [open, setOpen] = useState(true)
  if (cast.length === 0) return null
  return (
    <div className="stage-block">
      <div className="stage-toggle">
        <strong>{t('stage.title')}</strong>
        <span className="spacer" />
        <button type="button" className="btn ghost sm" aria-expanded={open} onClick={() => setOpen(!open)}>
          {open ? t('stage.hide') : t('stage.show')}
        </button>
      </div>
      {open && <Stage cast={cast} speech={recentSpeech(view, name)} activity={activityOfView(view)} compact />}
    </div>
  )
}

export function TurnView({ turn, onFollowUp, isLatest = false }: { turn: Turn; onFollowUp: (text: string) => void; isLatest?: boolean }) {
  const { t } = useI18n()
  const { prefs } = usePrefs()
  const { patchTurn, setSessionId } = useStore()
  const view = useJob(turn.jobId, turn.consultationId)
  const c = view.consultation
  const cid = c?.id ?? view.consultationId

  useEffect(() => {
    if (cid && cid !== turn.consultationId) patchTurn(turn.id, { consultationId: cid })
  }, [cid, turn.consultationId, turn.id, patchTurn])
  useEffect(() => {
    const ctx = (c?.meta as { context?: { session?: string | null } } | null | undefined)?.context
    if (ctx?.session) setSessionId(ctx.session)
  }, [c, setSessionId])

  const stage = isLatest && prefs.show_stage ? <StageBlock view={view} c={c} /> : null
  if (!isFinished(view.phase) && !c) return <>{stage}<ProgressPanel turn={turn} view={view} /></>
  if (view.phase === 'loading') return <Spinner label={t('loading')} />
  return <>{stage}<ResultPanel c={c ?? null} view={view} turn={turn} onFollowUp={onFollowUp} defaultTab="conversation" /></>
}

/** The result of a consultation: the answer, each agent's contribution, the research and the details. */
export function ResultPanel({ c, view, turn, onFollowUp, defaultTab = 'answer' }: { c: Consultation | null; view: JobView; turn?: Turn; onFollowUp: (text: string) => void; defaultTab?: ResultTab }) {
  const { t } = useI18n()
  const [tab, setTab] = useState<ResultTab>(defaultTab)
  const [focus, setFocus] = useState<number | null>(null)
  const hasResearch = Boolean(c?.research && c.research.sources > 0)
  const research = useLazy<ResearchResponse>(c && (hasResearch || tab === 'research') ? () => api.research(c.id) : null, `r-${c?.id ?? ''}-${tab === 'research'}`)
  const timeline = useLazy<Timeline>(c && tab === 'conversation' ? () => api.theater(c.id) : null, `t-${c?.id ?? ''}`)
  const known = useMemo(() => new Set((research.data?.sources ?? []).filter((s) => s.retrieved).map((s) => s.id)), [research.data])
  const onCite = useCallback((n: number) => {
    setFocus(n)
    setTab('research')
  }, [])

  const bubbles: TMsg[] = timeline.data ? fromTimeline(timeline.data) : liveBubbles(view)

  const exportJson = async () => {
    if (!c) return
    const bundle: Record<string, unknown> = { exported_by: 'AI Orchestra web', consultation: c }
    try { bundle.research = await api.research(c.id) } catch { /* none */ }
    try { bundle.recorded_steps = await api.theater(c.id) } catch { /* none */ }
    download(`consultation-${c.id}.json`, 'application/json', JSON.stringify(bundle, null, 2))
  }

  const errKey = view.error ? ERROR_HINT[view.error.kind] : undefined
  const tabs = [
    { id: 'conversation' as const, label: t('tab.conversation'), badge: c?.steps ?? view.messages.length },
    { id: 'answer' as const, label: t('tab.answer') },
    { id: 'research' as const, label: t('tab.research'), badge: c?.research ? c.research.retrieved : undefined },
    { id: 'details' as const, label: t('tab.details') },
  ]
  const status = c?.status ?? view.phase

  return (
    <section className="result card" aria-label={t('result.title')}>
      <div className="row wrap" style={{ gap: 8 }}>
        <Pill tone={statusTone(status)}>{t(`status.${status}` as TKey)}</Pill>
        {c?.mode && <Pill>{c.mode}</Pill>}
        {c?.strategy && <Pill>{c.strategy}</Pill>}
        {c && <Pill>{t('result.calls', { n: c.calls })}</Pill>}
        {c && <Pill>{formatDuration(c.duration_ms)}</Pill>}
        {c && <span className="faint">{c.participants.concat(c.coordinator).filter((v, i, a) => a.indexOf(v) === i).join(' · ')}</span>}
        <span className="spacer" />
        {c && (
          <>
            <a className="btn sm" href={`#/theater/${encodeURIComponent(c.id)}`}><Icon name="theater" size={14} /> {t('result.open_theater')}</a>
            <button type="button" className="btn sm" onClick={exportJson}><Icon name="download" size={14} /> {t('result.export')}</button>
          </>
        )}
      </div>

      {view.phase === 'gone' && !c && <Notice tone="info">{t('result.gone')}</Notice>}
      {status === 'partial' && (
        <Notice tone="warn">
          <strong>{t('result.partial_title')}</strong> {t('result.partial_body')}
          {view.failures.map((f) => <div key={f.seq} className="hint">{f.agent}: {f.error}</div>)}
        </Notice>
      )}
      {(status === 'failed' || (view.error && status !== 'cancelled')) && (
        <Notice tone="danger">
          <strong>{t('result.failed_title')}</strong> {view.error?.message}
          {errKey && <div className="hint">{t(errKey)}</div>}
          {view.failures.map((f) => <div key={f.seq} className="hint">{f.agent}: {f.error}</div>)}
        </Notice>
      )}
      {status === 'cancelled' && <Notice tone="info">{t('result.cancelled')}</Notice>}

      {c && <SummaryBox c={c} />}

      {c && (
        <>
          <Tabs tabs={tabs} value={tab} onChange={setTab} label={t('result.tabs')} />
          {tab === 'answer' && (
            <TabPanel id="answer">
              {c.final_answer.trim() ? <Markdown text={c.final_answer} knownSources={known} onCite={onCite} /> : <p className="muted">{t('result.no_answer')}</p>}
              {c.skipped.length > 0 && <p className="hint">{t('result.skipped')}: {c.skipped.map((s) => `${s.agent} (${s.reason})`).join('; ')}</p>}
              {hasResearch && !research.data && !research.error && <p className="hint">{t('result.loading_sources')}</p>}
              <div className="followups" aria-label={t('followups.title')}>
                <span className="faint">{t('followups.title')}</span>
                {(['followups.1', 'followups.2', 'followups.3'] as const).map((k) => (
                  <button key={k} type="button" className="btn sm" onClick={() => onFollowUp(t(k))}>{t(k)}</button>
                ))}
              </div>
              <Collapsible title={t('feedback.title')}>
                <FeedbackRow id={c.id} />
              </Collapsible>
            </TabPanel>
          )}
          {tab === 'conversation' && (
            <TabPanel id="conversation">
              <p className="hint">{timeline.data ? timeline.data.note : t('agents.live_note')}</p>
              {timeline.error && <Notice tone="danger">{timeline.error}</Notice>}
              {bubbles.length === 0 && !timeline.loading && <p className="muted">{t('agents.none')}</p>}
              {timeline.loading && <Spinner label={t('loading')} />}
              <Conversation msgs={bubbles} known={known} />
            </TabPanel>
          )}
          {tab === 'research' && (
            <TabPanel id="research">
              {research.loading && <Spinner label={t('loading')} />}
              {research.error && <Notice tone="danger">{research.error}</Notice>}
              {research.data && <ResearchView data={research.data} focus={focus} onFocus={setFocus} />}
            </TabPanel>
          )}
          {tab === 'details' && (
            <TabPanel id="details">
              <Details c={c} view={view} turn={turn} />
            </TabPanel>
          )}
        </>
      )}
    </section>
  )
}

export { ApiError }
