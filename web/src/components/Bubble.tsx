// One recorded message as a chat bubble: the agent's character, its name, what kind of call it was, when it
// finished and the text it returned. A failed call is a failure with its recorded error, never a reply.
import { useState } from 'react'
import type { Timeline } from '../api/types'
import { formatDuration, formatOffset, plainPreview } from '../lib/format'
import { Markdown } from '../lib/markdown'
import { useI18n, type TKey } from '../i18n'
import { Avatar, ROLE_KEY, useAgentName, useProviderName } from './agents'
import { Icon } from './Icons'
import type { MessageLike } from './MessageCard'
import type { RobotState } from './PixelRobot'
import { Pill } from './ui'

export type TMsg = MessageLike & { startMs: number; endMs: number }

export function fromTimeline(tl: Timeline): TMsg[] {
  return tl.messages.map((m) => ({
    key: `t${m.index}`, agent: m.agent, role: m.role, stage: m.stage, round: m.round, ok: m.ok, text: m.text, error: m.error, technical: m.technical,
    durationMs: m.duration_ms, offsetMs: m.finished_offset_ms, startMs: m.started_offset_ms, endMs: m.finished_offset_ms,
    usage: m.usage ?? null,
  }))
}

const fmt = (n: number) => (n >= 10_000 ? `${(n / 1000).toFixed(1)}k` : String(n))

export function Bubble({ m, avatarState, known, n }: { m: TMsg; avatarState: RobotState; known: Set<number>; n: number }) {
  const { t } = useI18n()
  const name = useAgentName()
  const provider = useProviderName()
  const [open, setOpen] = useState(m.stage === 'synthesis' && !m.technical)
  void n
  return (
    <article className={`bubble bubble-${m.ok ? 'ok' : 'fail'} agent-${m.agent}`} id={`msg-${m.key}`} aria-label={`${name(m.agent)}, ${t(ROLE_KEY[m.role] ?? 'role.perspective')}`}>
      <Avatar agent={m.agent} state={avatarState} size={44} />
      <div className="bubble-body">
        <div className="row wrap" style={{ gap: 8 }}>
          <strong>{name(m.agent)}</strong>
          {name(m.agent) !== provider(m.agent) && <span className="faint" style={{ fontSize: 'var(--text-xs)' }}>{provider(m.agent)}</span>}
          <Pill tone={m.ok ? 'neutral' : 'danger'}>{t(ROLE_KEY[m.role] ?? 'role.perspective')}</Pill>
          {m.round > 0 && <span className="faint" style={{ fontSize: 'var(--text-xs)' }}>{t('round_n', { n: m.round })}</span>}
          <span className="spacer" />
          <time className="faint mono" style={{ fontSize: 'var(--text-xs)' }} title={t('theater.recorded_at')}>
            +{formatOffset(m.endMs)} · {formatDuration(m.durationMs)}
            {m.usage && (m.usage.input_tokens != null || m.usage.output_tokens != null) ? ` · ${fmt(m.usage.input_tokens ?? 0)} in / ${fmt(m.usage.output_tokens ?? 0)} out` : ''}
          </time>
        </div>
        {m.ok && m.text !== null ? (
          <>
            {open ? <Markdown text={m.text} knownSources={known} /> : <p className="muted" style={{ margin: 0, overflowWrap: 'anywhere' }}>{plainPreview(m.text, m.technical ? 140 : 320)}</p>}
            <button type="button" className="btn ghost sm" aria-expanded={open} onClick={() => setOpen(!open)}>
              <Icon name="chevron" size={14} /> {open ? t('collapse') : t('expand')}{m.technical ? ` (${t('technical')})` : ''}
            </button>
          </>
        ) : (
          <div role="alert" className="msg-fail-text">
            <strong>{t('call_failed')}</strong> {m.error ? `— ${m.error}` : ''}
            <div className="hint">{t('no_reply_invented')}</div>
          </div>
        )}
      </div>
    </article>
  )
}


const HUMAN_STAGE: Record<string, TKey> = {
  research: 'stage.human.research', independent_analysis: 'stage.human.independent_analysis', review: 'stage.human.review',
  cross_review: 'stage.human.cross_review', synthesis: 'stage.human.synthesis',
}

/** The recorded messages as a conversation, grouped by what the group was doing at the time. */
export function Conversation({ msgs, known }: { msgs: TMsg[]; known: Set<number> }) {
  const { t } = useI18n()
  const groups: { stage: string; items: TMsg[] }[] = []
  for (const m of msgs) {
    const g = groups[groups.length - 1]
    if (g && g.stage === m.stage) g.items.push(m)
    else groups.push({ stage: m.stage, items: [m] })
  }
  return (
    <div className="conversation">
      {groups.map((g, gi) => (
        <section key={`${g.stage}-${gi}`} className="stage-group" aria-label={t(HUMAN_STAGE[g.stage] ?? 'stage.human.synthesis')}>
          <h3 className="stage-title">{t(HUMAN_STAGE[g.stage] ?? 'stage.human.synthesis')}</h3>
          {g.items.map((m, i) => (
            <Bubble key={m.key} m={m} avatarState={m.ok ? 'finished' : 'error'} known={known} n={gi + i} />
          ))}
        </section>
      ))}
    </div>
  )
}
