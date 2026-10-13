// The animated stage: the agents as small pixel characters in a theatre. Where they stand and whether they walk
// is decoration derived from what really happened (which stage is running, which call is active); every
// speech bubble is a message that was really recorded, trimmed, from the agent that wrote it - never invented.
// The whole scene is hidden from assistive technology because the same messages are in the conversation below.
import { useEffect, useMemo, useRef, useState } from 'react'
import { useI18n } from '../i18n'
import { agentKey, plainPreview } from '../lib/format'
import { usePrefs } from '../state/prefs'
import { useCharacters } from '../state/characters'
import { isFinished, type JobView } from '../state/job'
import { useAgentName } from './agents'
import { PixelRobot, type RobotState } from './PixelRobot'

export type StageActivity = 'idle' | 'planning' | 'research' | 'independent_analysis' | 'review' | 'cross_review' | 'synthesis' | 'done'
export type StageCast = { id: string; state: RobotState; coordinator?: boolean }
export type StageSpeech = { key: string; agent: string; text: string; failed?: boolean }

const WANDER = new Set<StageActivity>(['idle', 'planning', 'research'])
/** The curtains are open while the agents are really working or talking, closed before and after. */
const OPEN = new Set<StageActivity>(['independent_analysis', 'review', 'cross_review', 'synthesis'])

/** Hash to [0, 1): a stable "random" so a given agent and moment always map to the same spot. */
function unit(seed: string, tick: number): number {
  let h = tick * 2654435761
  for (let i = 0; i < seed.length; i++) h = (h ^ seed.charCodeAt(i)) * 16777619
  return ((h >>> 0) % 10000) / 10000
}

export type Spot = { x: number; y: number }

export function spotFor(i: number, n: number, activity: StageActivity, isCoordinator: boolean, id: string, tick: number, wander: boolean): Spot {
  // Everybody stands on the platform: feet between 70 % and 92 % of the stage height.
  const desk: Spot = { x: 22 + ((i + 0.5) / n) * 56, y: 82 }
  switch (activity) {
    case 'independent_analysis':
      return desk
    case 'review':
    case 'cross_review':
      return { x: 50 + (i - (n - 1) / 2) * 16, y: 76 }
    case 'synthesis':
      return isCoordinator ? { x: 50, y: 90 } : desk
    case 'done':
      return { x: 22 + ((i + 0.5) / n) * 56, y: 88 }
    default:
      return wander ? { x: 22 + unit(id, tick) * 56, y: 72 + unit(`${id}y`, tick) * 18 } : desk
  }
}

/** What the stage is doing, from the recorded stages of a live consultation. */
export function activityOfView(v: JobView): StageActivity {
  if (isFinished(v.phase)) return v.phase === 'completed' || v.phase === 'partial' ? 'done' : 'idle'
  const order = ['synthesis', 'cross_review', 'review', 'independent_analysis', 'research', 'planning'] as const
  return order.find((s) => v.stages[s] === 'active') ?? 'idle'
}

/** The last two recorded things said (messages and failures), oldest first. */
export function recentSpeech(v: JobView, name: (id: string) => string): StageSpeech[] {
  const all: (StageSpeech & { seq: number })[] = [
    ...v.messages.filter((m) => !m.technical).map((m) => ({ key: `m${m.seq}`, seq: m.seq, agent: m.agent, text: m.text })),
    ...v.failures.map((f) => ({ key: `f${f.seq}`, seq: f.seq, agent: f.agent, text: name(f.agent), failed: true })),
  ]
  return all.sort((a, b) => a.seq - b.seq).slice(-2)
}

export function useMotion(): boolean {
  const { prefs } = usePrefs()
  if (prefs.reduced_motion === 'on') return false
  if (prefs.reduced_motion === 'off') return true
  try {
    return !window.matchMedia('(prefers-reduced-motion: reduce)').matches
  } catch {
    return true
  }
}

/** The theatre itself, drawn in pixels: hall, proscenium arch, painted backdrop, wooden platform and footlights. */
function Backdrop() {
  const planks = [46, 50, 54, 58, 62]
  const lights = Array.from({ length: 11 }, (_, i) => 20 + i * 12)
  const stars = [[24, 9], [37, 14], [52, 8], [66, 17], [83, 10], [97, 15], [111, 8], [124, 13], [136, 18], [45, 22], [104, 24]]
  const heads = Array.from({ length: 20 }, (_, i) => 4 + i * 8)
  return (
    <svg className="stage-backdrop" viewBox="0 0 160 70" preserveAspectRatio="none" shapeRendering="crispEdges" aria-hidden="true" focusable="false">
      <rect width="160" height="70" fill="var(--stage-hall)" />
      {/* painted backdrop: night sky, moon, a skyline */}
      <rect x="12" y="6" width="136" height="38" fill="var(--stage-sky-a)" />
      <rect x="12" y="20" width="136" height="12" fill="var(--stage-sky-b)" />
      <rect x="12" y="32" width="136" height="12" fill="var(--stage-sky-c)" />
      {stars.map(([x, y]) => <rect key={`${x}-${y}`} x={x} y={y} width="1" height="1" fill="var(--stage-star)" />)}
      <rect x="118" y="10" width="6" height="6" fill="var(--stage-moon)" />
      <rect x="121" y="9" width="5" height="6" fill="var(--stage-sky-a)" />
      <rect x="12" y="30" width="10" height="14" fill="var(--stage-skyline)" />
      <rect x="24" y="26" width="8" height="18" fill="var(--stage-skyline)" />
      <rect x="34" y="33" width="12" height="11" fill="var(--stage-skyline)" />
      <rect x="112" y="29" width="9" height="15" fill="var(--stage-skyline)" />
      <rect x="123" y="24" width="7" height="20" fill="var(--stage-skyline)" />
      <rect x="132" y="31" width="16" height="13" fill="var(--stage-skyline)" />
      <rect x="26" y="29" width="1" height="1" fill="var(--stage-star)" /><rect x="29" y="33" width="1" height="1" fill="var(--stage-star)" />
      <rect x="125" y="28" width="1" height="1" fill="var(--stage-star)" /><rect x="137" y="35" width="1" height="1" fill="var(--stage-star)" />
      {/* the platform: planks, seams and the front lip */}
      <rect x="8" y="44" width="144" height="20" fill="var(--stage-wood-a)" />
      {planks.map((y, i) => <rect key={y} x="8" y={y} width="144" height="1" fill="var(--stage-wood-dark)" opacity={i % 2 ? 0.5 : 0.8} />)}
      {planks.slice(0, 4).map((y, r) => Array.from({ length: 6 }, (_, c) => <rect key={`${y}-${c}`} x={14 + c * 24 + (r % 2) * 12} y={y + 1} width="1" height="3" fill="var(--stage-wood-dark)" opacity="0.55" />))}
      <rect x="8" y="64" width="144" height="3" fill="var(--stage-wood-dark)" />
      <rect x="8" y="64" width="144" height="1" fill="var(--stage-wood-b)" />
      {lights.map((x) => (
        <g key={x}>
          <rect x={x} y="65" width="2" height="1" fill="var(--stage-bulb)" />
          <rect x={x - 1} y="62" width="4" height="2" fill="var(--stage-bulb)" opacity="0.18" />
        </g>
      ))}
      {/* the audience's heads at the very bottom */}
      <rect y="67" width="160" height="3" fill="var(--stage-audience)" />
      {heads.map((x, i) => <rect key={x} x={x} y={i % 3 === 0 ? 66 : 67} width="4" height={i % 3 === 0 ? 4 : 3} fill="var(--stage-audience)" />)}
      {/* the proscenium arch and the valance above it */}
      <rect x="0" y="0" width="12" height="70" fill="var(--stage-hall)" />
      <rect x="148" y="0" width="12" height="70" fill="var(--stage-hall)" />
      <rect x="8" y="6" width="4" height="58" fill="var(--stage-gold-dark)" />
      <rect x="148" y="6" width="4" height="58" fill="var(--stage-gold-dark)" />
      <rect x="9" y="6" width="1" height="58" fill="var(--stage-gold)" />
      <rect x="149" y="6" width="1" height="58" fill="var(--stage-gold)" />
      <rect x="8" y="5" width="144" height="2" fill="var(--stage-gold-dark)" />
      <rect x="8" y="5" width="144" height="1" fill="var(--stage-gold)" />
      <rect x="0" y="0" width="160" height="6" fill="var(--stage-curtain)" />
      {Array.from({ length: 40 }, (_, i) => <rect key={i} x={i * 4} y="6" width="3" height={i % 2 ? 2 : 1} fill="var(--stage-curtain)" />)}
      {Array.from({ length: 20 }, (_, i) => <rect key={i} x={i * 8 + 3} y="0" width="1" height="6" fill="var(--stage-curtain-dark)" />)}
      <rect x="0" y="5" width="160" height="1" fill="var(--stage-gold)" />
    </svg>
  )
}

type CharProps = {
  id: string
  index: number
  spot: Spot
  state: RobotState
  speech?: StageSpeech
  motion: boolean
  size: number
}

function StageChar({ id, spot, state, speech, motion, size }: CharProps) {
  const { t } = useI18n()
  const name = useAgentName()
  const { appearanceOf } = useCharacters()
  const look = appearanceOf(id)
  const prev = useRef<Spot | null>(null)
  const [moving, setMoving] = useState(false)
  const [flip, setFlip] = useState(false)
  const { x, y } = spot
  useEffect(() => {
    const p = prev.current
    prev.current = { x, y }
    if (!p || (p.x === x && p.y === y) || !motion) return
    setFlip(x < p.x)
    setMoving(true)
    const timer = window.setTimeout(() => setMoving(false), 1500)
    return () => window.clearTimeout(timer)
  }, [x, y, motion])
  const side = spot.x < 24 ? ' bubble-left' : spot.x > 76 ? ' bubble-right' : ''
  return (
    <div className={`stage-char st-${state}`} style={{ left: `${spot.x}%`, top: `${spot.y}%` }} data-agent={id} data-moving={moving || undefined}>
      {speech && (
        <div className={`stage-bubble${speech.failed ? ' failed' : ''}${side}`} data-speech={speech.key}>
          {speech.failed ? t('stage.failed') : plainPreview(speech.text, 80)}
        </div>
      )}
      <PixelRobot kind={agentKey(id)} state={state} size={size} palette={look.palette} accessory={look.accessory} walking={moving} flip={flip} decorative />
      <div className="stage-name">
        <strong>{name(id)}</strong>
      </div>
    </div>
  )
}

export function Stage({ cast, speech, activity, compact = false, curtains = 'auto' }: { cast: StageCast[]; speech: StageSpeech[]; activity: StageActivity; compact?: boolean; curtains?: 'auto' | 'open' }) {
  const { t } = useI18n()
  const motion = useMotion()
  const [tick, setTick] = useState(0)
  const open = curtains === 'open' || OPEN.has(activity)
  const wandering = motion && open && WANDER.has(activity)
  useEffect(() => {
    if (!wandering) return
    const id = window.setInterval(() => setTick((x) => x + 1), 4500)
    return () => window.clearInterval(id)
  }, [wandering])
  const bySpeaker = useMemo(() => {
    const m = new Map<string, StageSpeech>()
    for (const s of speech) m.set(s.agent, s)
    return m
  }, [speech])
  const n = Math.max(1, cast.length)
  // The light follows whoever spoke last.
  const last = speech[speech.length - 1]
  const lastIndex = last ? cast.findIndex((c) => c.id === last.agent) : -1
  const spot = lastIndex >= 0 ? spotFor(lastIndex, n, activity, Boolean(cast[lastIndex]?.coordinator), last?.agent ?? '', tick, wandering) : null
  const caption = activity === 'done' && open ? 'stage.act.done_open' : `stage.act.${activity}`
  return (
    <div className={`stage${compact ? ' compact' : ''}`} data-activity={activity} data-curtains={open ? 'open' : 'closed'} aria-hidden="true">
      <Backdrop />
      {open && spot && <div className="stage-spot" style={{ left: `${spot.x}%`, top: `${spot.y}%` }} />}
      {cast.map((c, i) => (
        <StageChar
          key={c.id}
          id={c.id}
          index={i}
          spot={spotFor(i, n, activity, Boolean(c.coordinator), c.id, tick, wandering)}
          state={c.state}
          speech={open ? bySpeaker.get(c.id) : undefined}
          motion={motion}
          size={compact ? 64 : 72}
        />
      ))}
      <div className="stage-curtain left" />
      <div className="stage-curtain right" />
      <span className="stage-caption">{t(caption as never)}</span>
    </div>
  )
}
