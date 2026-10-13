// One real, recorded message (or recorded failure) of an agent. The text is exactly what the CLI returned.
import { useState } from 'react'
import type { RobotState } from './PixelRobot'
import { Avatar, ROLE_KEY, STAGE_KEY, useAgentName } from './agents'
import { Icon } from './Icons'
import { Markdown } from '../lib/markdown'
import { formatDuration, formatOffset, plainPreview } from '../lib/format'
import { useI18n } from '../i18n'
import { Pill } from './ui'

export type MessageLike = {
  key: string
  seq?: number
  agent: string
  role: string
  stage: string
  round: number
  ok: boolean
  text: string | null
  error: string | null
  technical: boolean
  durationMs: number
  usage?: { input_tokens?: number | null; output_tokens?: number | null; cached_input_tokens?: number | null } | null
  /** Offset from the start of the consultation, when the record has one. */
  offsetMs?: number
  /** Absolute time (live events). */
  clock?: string
}

export function MessageCard({
  m, defaultOpen = false, avatarState, knownSources, onCite, id,
}: {
  m: MessageLike
  defaultOpen?: boolean
  avatarState?: RobotState
  knownSources?: Set<number>
  onCite?: (n: number) => void
  id?: string
}) {
  const { t } = useI18n()
  const name = useAgentName()
  const [open, setOpen] = useState(defaultOpen)
  const state: RobotState = avatarState ?? (m.ok ? 'finished' : 'error')
  return (
    <article className={`msg msg-${m.ok ? 'ok' : 'fail'}`} id={id} aria-label={`${name(m.agent)}, ${t(ROLE_KEY[m.role] ?? 'role.perspective')}`}>
      <Avatar agent={m.agent} state={state} size={36} />
      <div className="msg-main">
        <div className="row wrap" style={{ gap: 8 }}>
          <strong>{name(m.agent)}</strong>
          <Pill tone={m.ok ? 'neutral' : 'danger'}>{t(ROLE_KEY[m.role] ?? 'role.perspective')}</Pill>
          {m.round > 0 && <span className="faint" style={{ fontSize: 'var(--text-xs)' }}>{t('round_n', { n: m.round })}</span>}
          <span className="spacer" />
          <span className="faint mono" style={{ fontSize: 'var(--text-xs)' }}>
            {m.offsetMs !== undefined ? `+${formatOffset(m.offsetMs)}` : m.clock ?? ''} · {formatDuration(m.durationMs)}
          </span>
        </div>
        {m.ok && m.text !== null ? (
          <>
            {!open && <p className="msg-preview muted">{plainPreview(m.text, m.technical ? 120 : 240)}</p>}
            {open && <Markdown text={m.text} knownSources={knownSources} onCite={onCite} />}
            <button type="button" className="btn ghost sm" aria-expanded={open} onClick={() => setOpen(!open)}>
              <Icon name="chevron" size={14} /> {open ? t('collapse') : t('expand')}
              {m.technical ? ` (${t('technical')})` : ''}
              {' · '}{t(STAGE_KEY[m.stage] ?? 'stage.synthesis')}
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
