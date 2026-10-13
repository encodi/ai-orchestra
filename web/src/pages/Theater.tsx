// Agent Theater: the same recorded consultation, shown as a conversation between the agents.
// Everything on this screen comes from a recorded event or recorded step. The theater adds no dialogue,
// no reasoning and no word-by-word typing; replay only reveals real messages at their recorded times.
import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import { api } from '../api/client'
import type { Consultation, HistoryItem, OneResponse, Timeline } from '../api/types'
import { STAGE_KEY, STATE_KEY, useAgentName } from '../components/agents'
import { Stage, activityOfView, type StageActivity, type StageSpeech } from '../components/Stage'
import { Icon } from '../components/Icons'
import { Bubble, fromTimeline, type TMsg } from '../components/Bubble'
import { MessageCard } from '../components/MessageCard'
import type { RobotState } from '../components/PixelRobot'
import { Collapsible, Empty, Notice, Pill, Spinner, statusTone } from '../components/ui'
import { useI18n, type TKey } from '../i18n'
import { formatOffset, truncate } from '../lib/format'
import { Markdown } from '../lib/markdown'
import { isFinished, robotStateFor, useJob } from '../state/job'
import { useStore } from '../state/store'
import { navigate } from '../lib/router'

function stateAt(agent: string, msgs: TMsg[], cursor: number | null, fallbackIdle: boolean): RobotState {
  const mine = msgs.filter((m) => m.agent === agent)
  if (cursor === null) {
    const last = mine[mine.length - 1]
    if (!last) return fallbackIdle ? 'idle' : 'waiting'
    return last.ok ? 'finished' : 'error'
  }
  const active = mine.find((m) => m.startMs <= cursor && cursor < m.endMs)
  if (active) return robotStateFor(active.stage)
  const done = [...mine].reverse().find((m) => m.endMs <= cursor)
  if (done) return done.ok ? 'finished' : 'error'
  return cursor > 0 ? 'waiting' : 'idle'
}

type Replay = { cursor: number; playing: boolean; speed: number }

function Shell({
  question, status, coordinator, participants, msgs, totalMs: recordedMs, live, consultation, eventsList, liveStates, liveActivity, skipped, notes,
}: {
  question?: string
  status: string
  coordinator?: string
  participants: string[]
  msgs: TMsg[]
  totalMs: number
  live: boolean
  consultation: Consultation | null
  eventsList: { key: string; at: number; label: string; target?: string; tone?: 'danger' | 'ok' }[]
  liveStates?: Record<string, RobotState>
  liveActivity?: StageActivity
  skipped: { agent: string; reason: string }[]
  notes: string[]
}) {
  const { t } = useI18n()
  const name = useAgentName()
  const [view, setView] = useState<'theater' | 'transcript'>('theater')
  // The record's own duration can end a few ms before its last call's recorded finish; replay must reach it.
  const totalMs = Math.max(recordedMs, ...msgs.map((m) => m.endMs))
  const [replay, setReplay] = useState<Replay | null>(null)
  const [skipWaits, setSkipWaits] = useState(true)
  const [follow, setFollow] = useState(live)
  const feedRef = useRef<HTMLDivElement>(null)

  const cast = useMemo(() => {
    const ids = [...participants]
    if (coordinator && !ids.includes(coordinator)) ids.push(coordinator)
    for (const m of msgs) if (!ids.includes(m.agent)) ids.push(m.agent)
    return ids
  }, [participants, coordinator, msgs])

  const visible = useMemo(() => (replay ? msgs.filter((m) => m.endMs <= replay.cursor) : msgs), [msgs, replay])

  // Playback: recorded time advances at `speed`; with "skip long waits" the idle stretch before the next
  // recorded message is crossed quickly. The messages and their timestamps are never changed.
  useEffect(() => {
    if (!replay?.playing) return
    let last = performance.now()
    const id = window.setInterval(() => {
      const now = performance.now()
      const dt = now - last
      last = now
      setReplay((r) => {
        if (!r || !r.playing) return r
        const next = msgs.find((m) => m.endMs > r.cursor)
        let step = dt * r.speed
        if (skipWaits && next && next.endMs - r.cursor > 3000) step = Math.max(step, Math.min(dt * 25, next.endMs - r.cursor - 1200))
        const cursor = Math.min(totalMs, r.cursor + step)
        return { ...r, cursor, playing: cursor < totalMs }
      })
    }, 100)
    return () => window.clearInterval(id)
  }, [replay?.playing, msgs, totalMs, skipWaits])

  // Follow only the feed's own scroll area (never the whole page), and only while live or replaying.
  useEffect(() => {
    const el = feedRef.current
    if (follow && el && (live || replay)) el.scrollTop = el.scrollHeight
  }, [visible.length, follow, live, replay])

  const onScroll = () => {
    const el = feedRef.current
    if (!el) return
    if (!live && !replay) return
    const near = el.scrollHeight - el.scrollTop - el.clientHeight < 80
    if (!near && follow) setFollow(false)
  }

  const known = useMemo(() => new Set<number>(), [])
  const stateOf = (id: string): RobotState => liveStates?.[id] ?? stateAt(id, msgs, replay ? replay.cursor : null, !live)
  // What the stage shows: the real recorded stage (live), the stage the replay cursor is in, or "done".
  const speech: StageSpeech[] = visible.filter((m) => !m.technical).slice(-2).map((m) => ({ key: m.key, agent: m.agent, text: m.text ?? '', failed: !m.ok }))
  const lastVisible = visible[visible.length - 1]
  const activeAtCursor = replay ? msgs.find((m) => m.startMs <= replay.cursor && replay.cursor < m.endMs) : undefined
  const activity: StageActivity = live
    ? (liveActivity ?? 'idle')
    : replay
      ? ((activeAtCursor?.stage ?? (replay.cursor >= totalMs ? 'done' : lastVisible?.stage ?? 'idle')) as StageActivity)
      : 'done'
  const groups: { stage: string; items: TMsg[] }[] = []
  for (const m of visible) {
    const g = groups[groups.length - 1]
    if (g && g.stage === m.stage) g.items.push(m)
    else groups.push({ stage: m.stage, items: [m] })
  }
  const meta = (consultation?.meta ?? {}) as { unresolved_disagreement?: boolean; disagreements?: string | null }
  const hasFailure = msgs.some((m) => !m.ok)

  const startReplay = () => { setReplay({ cursor: 0, playing: true, speed: 4 }); setFollow(true) }
  const eventsShown = replay ? eventsList.filter((e) => e.at <= replay.cursor) : eventsList

  return (
    <div className="theater">
      <div className="row wrap" style={{ gap: 8 }}>
        <Pill tone={statusTone(status)}>{t(`status.${status}` as TKey)}</Pill>
        {live && <Pill tone="info">{t('theater.live')}</Pill>}
        {replay && <Pill tone="accent">{t('theater.replaying')}</Pill>}
        <span className="muted" style={{ flex: 1, minWidth: 0, overflowWrap: 'anywhere' }}>{question ? truncate(question, 160) : ''}</span>
        <div className="seg" role="group" aria-label={t('theater.view')}>
          <button type="button" aria-pressed={view === 'theater'} onClick={() => setView('theater')}>{t('theater.view_theater')}</button>
          <button type="button" aria-pressed={view === 'transcript'} onClick={() => setView('transcript')}>{t('theater.view_transcript')}</button>
        </div>
      </div>

      <Stage cast={cast.map((id) => ({ id, state: stateOf(id), coordinator: id === coordinator }))} speech={speech} activity={activity} curtains={!live && !replay ? 'open' : 'auto'} />
      {skipped.length > 0 && <p className="hint" style={{ margin: 0 }}>{t('result.skipped')}: {skipped.map((s) => `${name(s.agent)} (${s.reason})`).join('; ')}</p>}

      {!live && msgs.length > 0 && (
        <div className="card flat replay-bar" role="group" aria-label={t('replay.title')}>
          {!replay ? (
            <>
              <button type="button" className="btn primary" onClick={startReplay}><Icon name="play" size={16} /> {t('replay.start')}</button>
              <span className="hint">{t('replay.explain')}</span>
            </>
          ) : (
            <>
              <button type="button" className="btn" onClick={() => setReplay({ ...replay, playing: replay.cursor >= totalMs ? false : !replay.playing })} aria-label={replay.playing ? t('replay.pause') : t('replay.play')}>
                <Icon name={replay.playing ? 'pause' : 'play'} size={16} />
              </button>
              <button type="button" className="btn" onClick={() => {
                const next = msgs.find((m) => m.endMs > replay.cursor)
                if (next) setReplay({ ...replay, playing: false, cursor: next.endMs })
              }} aria-label={t('replay.step')}><Icon name="step" size={16} /></button>
              <button type="button" className="btn" onClick={() => setReplay({ cursor: 0, playing: false, speed: replay.speed })} aria-label={t('replay.restart')}><Icon name="reset" size={16} /></button>
              <label className="row" style={{ gap: 6 }}>
                <span className="faint">{t('replay.speed')}</span>
                <select className="select" style={{ width: 'auto' }} value={replay.speed} onChange={(e) => setReplay({ ...replay, speed: Number(e.target.value) })}>
                  {[1, 2, 4, 8, 16].map((s) => <option key={s} value={s}>{s}×</option>)}
                </select>
              </label>
              <label className="row" style={{ gap: 6 }}>
                <input type="checkbox" checked={skipWaits} onChange={(e) => setSkipWaits(e.target.checked)} />
                <span>{t('replay.skip')}</span>
              </label>
              <input
                type="range" min={0} max={Math.max(100, Math.ceil(totalMs / 100) * 100)} step={100} value={replay.cursor}
                onChange={(e) => setReplay({ ...replay, playing: false, cursor: Number(e.target.value) })}
                aria-label={t('replay.position')} aria-valuetext={`${formatOffset(replay.cursor)} / ${formatOffset(totalMs)}`} style={{ flex: 1, minWidth: 120 }}
              />
              <span className="mono faint">{formatOffset(replay.cursor)} / {formatOffset(totalMs)}</span>
              <button type="button" className="btn ghost sm" onClick={() => setReplay(null)}>{t('replay.exit')}</button>
            </>
          )}
        </div>
      )}
      {replay && <p className="hint" style={{ margin: 0 }}>{t('replay.note')}</p>}

      <div className="theater-grid">
        <div className="feed-wrap">
          <div className="feed" ref={feedRef} onScroll={onScroll} role="log" aria-live={live ? 'polite' : 'off'} aria-label={t('theater.feed')} tabIndex={0}>
            {msgs.length === 0 && live && <p className="muted">{t('theater.waiting_first')}</p>}
            {msgs.length === 0 && !live && <Empty title={t('theater.empty')} />}
            {view === 'theater' ? (
              groups.map((g, gi) => (
                <section key={`${g.stage}-${gi}`} className="stage-group" aria-label={t(STAGE_KEY[g.stage] ?? 'stage.synthesis')}>
                  <h3 className="stage-title">{t(STAGE_KEY[g.stage] ?? 'stage.synthesis')}</h3>
                  {g.items.map((m, i) => <Bubble key={m.key} m={m} avatarState={m.ok ? 'finished' : 'error'} known={known} n={gi + i} />)}
                </section>
              ))
            ) : (
              visible.map((m) => <MessageCard key={m.key} m={m} />)
            )}
            {(meta.unresolved_disagreement || hasFailure) && visible.length === msgs.length && (
              <div className="indicators">
                {meta.unresolved_disagreement && (
                  <Notice tone="warn">
                    <strong>{t('theater.disagreement')}</strong>
                    {meta.disagreements ? <Markdown text={meta.disagreements} /> : null}
                    <div className="hint">{t('theater.disagreement_note')}</div>
                  </Notice>
                )}
                {hasFailure && <Notice tone="danger">{t('theater.failure_note')}</Notice>}
              </div>
            )}
          </div>
          {!follow && (live || replay) && (
            <button type="button" className="btn follow-btn" onClick={() => { setFollow(true) }}>
              {t('theater.resume_follow')}
            </button>
          )}
          <div className="row" style={{ marginTop: 6 }}>
            <label className="row" style={{ gap: 6 }}>
              <input type="checkbox" checked={follow} onChange={(e) => setFollow(e.target.checked)} />
              <span className="hint">{t('theater.autoscroll')}</span>
            </label>
          </div>
        </div>

        <aside className="timeline card flat" aria-label={t('theater.timeline')}>
          <h3>{t('theater.timeline')}</h3>
          <ol className="tl">
            {eventsShown.map((e) => (
              <li key={e.key} className={e.tone ? `tl-${e.tone}` : ''}>
                <span className="mono faint">+{formatOffset(Math.max(0, e.at))}</span>{' '}
                {e.target ? <a href={`#${e.target}`} onClick={(ev) => { ev.preventDefault(); document.getElementById(e.target ?? '')?.scrollIntoView({ block: 'center' }) }}>{e.label}</a> : e.label}
              </li>
            ))}
            {eventsShown.length === 0 && <li className="faint">{t('theater.no_events')}</li>}
          </ol>
          {notes.length > 0 && (
            <Collapsible title={t('progress.notes', { n: notes.length })}>
              <ul style={{ margin: 0, paddingLeft: 16 }}>{notes.slice(-12).map((n, i) => <li key={i}>{n}</li>)}</ul>
            </Collapsible>
          )}
          <p className="hint" style={{ marginBottom: 0 }}>{t('theater.provenance')}</p>
        </aside>
      </div>
    </div>
  )
}

const EVENT_LABEL: Record<string, TKey> = {
  planning_started: 'ev.planning_started', strategy_selected: 'ev.strategy_selected', consultation_created: 'ev.created',
  consultation_completed: 'ev.completed', consultation_failed: 'ev.failed', consultation_cancelled: 'ev.cancelled',
  research_note: 'ev.research', memory_note: 'ev.memory',
}

function LiveTheater({ id, one }: { id: string; one: OneResponse }) {
  const { t } = useI18n()
  const name = useAgentName()
  const view = useJob(id, one.job?.consultation_id ?? undefined)
  // A job that had already finished when the page opened is a record to read from the top, not a live feed.
  const wasRunning = useRef(one.job?.status === 'running').current
  const first = view.firstTs ?? 0
  const msgs: TMsg[] = useMemo(() => {
    const ok: TMsg[] = view.messages.map((m) => {
      const end = m.ts_ms - first
      return { key: `l${m.seq}`, seq: m.seq, agent: m.agent, role: m.role, stage: m.stage, round: m.round, ok: true, text: m.text, error: null, technical: m.technical, durationMs: m.duration_ms, offsetMs: end, startMs: end - m.duration_ms, endMs: end }
    })
    const bad: TMsg[] = view.failures.map((f) => {
      const end = f.ts_ms - first
      return { key: `f${f.seq}`, seq: f.seq, agent: f.agent, role: f.role, stage: f.stage, round: 0, ok: false, text: null, error: f.error, technical: false, durationMs: f.duration_ms, offsetMs: end, startMs: end - f.duration_ms, endMs: end }
    })
    return [...ok, ...bad].sort((a, b) => (a as unknown as { seq: number }).seq - (b as unknown as { seq: number }).seq)
  }, [view.messages, view.failures, first])
  const events = useMemo(
    () =>
      view.events.map((e) => {
        const at = e.ts_ms - first
        const agent = typeof e.data.agent === 'string' ? name(e.data.agent) : ''
        const stage = typeof e.data.stage === 'string' ? t(STAGE_KEY[e.data.stage] ?? 'stage.synthesis') : ''
        let label = ''
        let target: string | undefined
        let tone: 'danger' | 'ok' | undefined
        if (e.kind === 'provider_started') label = t('ev.started', { a: agent, s: stage })
        else if (e.kind === 'provider_completed') {
          label = e.data.ok === true ? t('ev.finished_ok', { a: agent, s: stage }) : t('ev.finished_fail', { a: agent, s: stage })
          tone = e.data.ok === true ? undefined : 'danger'
        } else if (e.kind === 'agent_message_recorded') {
          label = t('ev.message', { a: agent })
          target = `msg-${`l${e.seq}`}`
        } else label = EVENT_LABEL[e.kind] ? t(EVENT_LABEL[e.kind] as TKey) : e.kind
        if (e.kind === 'consultation_completed') tone = 'ok'
        return { key: `e${e.seq}`, at, label, target, tone }
      }),
    [view.events, first, name, t],
  )
  const liveStates = useMemo(() => Object.fromEntries(Object.entries(view.agents).map(([k, a]) => [k, a.state])) as Record<string, RobotState>, [view.agents])
  const total = view.lastTs && view.firstTs ? view.lastTs - view.firstTs : 0
  return (
    <Shell
      question={one.job?.question_preview}
      status={view.consultation?.status ?? (view.phase === 'loading' ? 'running' : view.phase)}
      coordinator={view.coordinator}
      participants={view.order.filter((a) => a !== view.coordinator)}
      msgs={msgs}
      totalMs={total}
      live={wasRunning && !isFinished(view.phase) && view.phase !== 'gone'}
      consultation={view.consultation ?? null}
      eventsList={events}
      liveStates={isFinished(view.phase) ? undefined : liveStates}
      liveActivity={activityOfView(view)}
      skipped={view.skipped}
      notes={view.notes}
    />
  )
}

function SavedTheater({ id, one }: { id: string; one: OneResponse }) {
  const { t } = useI18n()
  const [tl, setTl] = useState<Timeline | null>(null)
  const [err, setErr] = useState<string | null>(null)
  useEffect(() => {
    let alive = true
    api.theater(id).then((x) => alive && setTl(x)).catch((e: unknown) => alive && setErr(e instanceof Error ? e.message : String(e)))
    return () => { alive = false }
  }, [id])
  const msgs = useMemo(() => (tl ? fromTimeline(tl) : []), [tl])
  const events = useMemo(() => {
    const out: { key: string; at: number; label: string; target?: string; tone?: 'danger' | 'ok' }[] = []
    msgs.forEach((m) => {
      out.push({ key: `s${m.key}`, at: m.startMs, label: `${m.agent}: ${t(STAGE_KEY[m.stage] ?? 'stage.synthesis')} …` })
      out.push({ key: `e${m.key}`, at: m.endMs, label: m.ok ? t('ev.finished_ok', { a: m.agent, s: t(STAGE_KEY[m.stage] ?? 'stage.synthesis') }) : t('ev.finished_fail', { a: m.agent, s: t(STAGE_KEY[m.stage] ?? 'stage.synthesis') }), target: `msg-${m.key}`, tone: m.ok ? undefined : 'danger' })
    })
    return out.sort((a, b) => a.at - b.at)
  }, [msgs, t])
  if (err) return <Notice tone="danger">{err}</Notice>
  if (!tl) return <Spinner label={t('loading')} />
  const c = one.consultation ?? null
  return (
    <Shell
      question={c?.question}
      status={tl.status}
      coordinator={tl.coordinator}
      participants={tl.participants}
      msgs={msgs}
      totalMs={tl.duration_ms}
      live={false}
      consultation={c}
      eventsList={events}
      skipped={tl.skipped.map(([agent, reason]) => ({ agent, reason }))}
      notes={[]}
    />
  )
}

function Picker() {
  const { t } = useI18n()
  const { turns } = useStore()
  const [items, setItems] = useState<HistoryItem[] | null>(null)
  const [err, setErr] = useState<string | null>(null)
  useEffect(() => {
    api.history({ limit: 12 }).then((r) => setItems(r.items)).catch((e: unknown) => setErr(e instanceof Error ? e.message : String(e)))
  }, [])
  const last = turns[turns.length - 1]
  return (
    <div className="stack">
      {last && (
        <div className="card">
          <h3>{t('theater.current')}</h3>
          <p className="muted">{truncate(last.question, 140)}</p>
          <button type="button" className="btn primary" onClick={() => navigate(`/theater/${last.consultationId ?? last.jobId}`)}>{t('theater.open')}</button>
        </div>
      )}
      <div className="card">
        <h3>{t('theater.recent')}</h3>
        {err && <Notice tone="danger">{err}</Notice>}
        {!items && !err && <Spinner label={t('loading')} />}
        {items && items.length === 0 && <p className="muted">{t('theater.none_saved')}</p>}
        <ul className="plain-list">
          {items?.map((it) => (
            <li key={it.id}>
              <a href={`#/theater/${encodeURIComponent(it.id)}`}>{truncate(it.question, 110)}</a> <Pill tone={statusTone(it.status)}>{it.status}</Pill>
            </li>
          ))}
        </ul>
      </div>
    </div>
  )
}

export function TheaterPage({ id }: { id?: string }) {
  const { t } = useI18n()
  const [one, setOne] = useState<OneResponse | null>(null)
  const [err, setErr] = useState<string | null>(null)
  const load = useCallback(() => {
    if (!id) return
    setOne(null)
    setErr(null)
    api.one(id).then(setOne).catch((e: unknown) => setErr(e instanceof Error ? e.message : String(e)))
  }, [id])
  useEffect(load, [load])
  return (
    <div className="page">
      <div className="page-head">
        <div>
          <h1>{t('theater.title')}</h1>
          <p>{t('theater.subtitle')}</p>
        </div>
        {id && <><span className="spacer" /><a className="btn" href="#/theater">{t('theater.back')}</a></>}
      </div>
      {!id && <Picker />}
      {id && err && <Notice tone="danger">{err}</Notice>}
      {id && !err && !one && <Spinner label={t('loading')} />}
      {id && one && (one.kind === 'job' && one.job && (one.job.status === 'running' || !one.consultation) ? (
        // A running job is watched through its events; a finished one is read from its saved record, so a
        // replay always uses the recorded step times and never the moments events happened to be received.
        <LiveTheater id={one.job.job_id} one={one} />
      ) : (
        <SavedTheater id={one.consultation?.id ?? id} one={one} />
      ))}
    </div>
  )
}

export { STATE_KEY }
