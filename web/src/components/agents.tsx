import type { ReactNode } from 'react'
import { agentKey } from '../lib/format'
import { PixelRobot, ROBOT_NAMES, type RobotState } from './PixelRobot'
import { useI18n, type TKey } from '../i18n'
import { useCharacters } from '../state/characters'
import { useStore } from '../state/store'

/** The name shown for an agent: the character name the user chose, else the provider's own name. */
export function useAgentName(): (id: string) => string {
  const { caps } = useStore()
  const { nameOf } = useCharacters()
  return (id: string) => nameOf(id, caps?.providers.find((p) => p.id === id)?.display_name ?? id.charAt(0).toUpperCase() + id.slice(1))
}

/** The provider's own name (the CLI), whatever character it plays. */
export function useProviderName(): (id: string) => string {
  const { caps } = useStore()
  return (id: string) => caps?.providers.find((p) => p.id === id)?.display_name ?? id
}

export const STAGE_KEY: Record<string, TKey> = {
  planning: 'stage.planning', research: 'stage.research', independent_analysis: 'stage.independent_analysis', review: 'stage.review',
  cross_review: 'stage.cross_review', synthesis: 'stage.synthesis',
}
export const ROLE_KEY: Record<string, TKey> = {
  planning: 'role.planning', verification: 'role.verification', perspective: 'role.perspective', proposal: 'role.proposal',
  review: 'role.review', cross_review: 'role.cross_review', synthesis: 'role.synthesis',
}
export const STATE_KEY: Record<RobotState, TKey> = {
  idle: 'state.idle', thinking: 'state.thinking', speaking: 'state.speaking', reviewing: 'state.reviewing', waiting: 'state.waiting',
  finished: 'state.finished', error: 'state.error',
}

export function Avatar({ agent, state = 'idle', size = 48 }: { agent: string; state?: RobotState; size?: number }) {
  const kind = agentKey(agent)
  const { appearanceOf } = useCharacters()
  const look = appearanceOf(agent)
  return (
    <div className={`avatar avatar-${kind}`} style={{ width: size + 12, height: size + 12 }}>
      <PixelRobot kind={kind} state={state} size={size} palette={look.palette} accessory={look.accessory} decorative />
    </div>
  )
}

/** The robot with the agent's real name written beside it (the robot itself is decorative). */
export function AgentTag({ agent, state, size = 40, extra }: { agent: string; state?: RobotState; size?: number; extra?: ReactNode }) {
  const name = useAgentName()
  const { t } = useI18n()
  return (
    <div className="agent-tag">
      <Avatar agent={agent} state={state} size={size} />
      <div style={{ minWidth: 0 }}>
        <div className="agent-name">{name(agent)}</div>
        <div className="faint" style={{ fontSize: 'var(--text-xs)' }}>
          {state ? t(STATE_KEY[state]) : ROBOT_NAMES[agentKey(agent)]}
          {extra}
        </div>
      </div>
    </div>
  )
}
