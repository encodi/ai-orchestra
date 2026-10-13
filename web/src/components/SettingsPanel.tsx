// Every consultation setting, drawn from the schema the server publishes at /capabilities (the same list
// the terminal chat uses). A setting left empty is AUTO. Nothing here changes a value on its own.
import { useI18n, type TKey } from '../i18n'
import type { Capabilities, FieldSchema, InForce, Origin, SettingsField, SettingsObj } from '../api/types'
import { Pill } from './ui'

export const fieldOf = (key: string): SettingsField => key.replace(/-/g, '_') as SettingsField

const LABEL: Record<string, TKey> = {
  mode: 'set.mode', coordinator: 'set.coordinator', agents: 'set.agents', rounds: 'set.rounds', depth: 'set.depth', web: 'set.web',
  strategy: 'set.strategy', length: 'set.length', timeout: 'set.timeout', 'max-calls': 'set.max_calls', memory: 'set.memory',
  'context-budget': 'set.context_budget', view: 'set.view', 'max-sources': 'set.max_sources', 'freshness-days': 'set.freshness_days',
  'max-duration': 'set.max_duration', 'max-parallel': 'set.max_parallel',
}
const HELP: Record<string, TKey> = {
  mode: 'help.mode', coordinator: 'help.coordinator', agents: 'help.agents', rounds: 'help.rounds', depth: 'help.depth', web: 'help.web',
  strategy: 'help.strategy', length: 'help.length', timeout: 'help.timeout', 'max-calls': 'help.max_calls', memory: 'help.memory',
  'context-budget': 'help.context_budget', view: 'help.view', 'max-sources': 'help.max_sources', 'freshness-days': 'help.freshness_days',
  'max-duration': 'help.max_duration', 'max-parallel': 'help.max_parallel',
}
export const GROUPS: { id: TKey; keys: string[] }[] = [
  { id: 'group.consultation', keys: ['mode', 'strategy', 'coordinator', 'agents', 'rounds', 'length'] },
  { id: 'group.research', keys: ['web', 'depth', 'max-sources', 'freshness-days'] },
  { id: 'group.limits', keys: ['timeout', 'max-calls', 'max-duration', 'max-parallel'] },
  { id: 'group.memory', keys: ['memory', 'context-budget'] },
  { id: 'group.output', keys: ['view'] },
]

export const ORIGIN_KEY: Record<Origin, TKey> = { auto: 'origin.auto', preference: 'origin.preference', session: 'origin.session', request: 'origin.request' }

function FieldControl({ f, value, onChange, idBase }: { f: FieldSchema; value: unknown; onChange: (v: unknown) => void; idBase: string }) {
  const { t } = useI18n()
  const disabled = Boolean(f.disabled_reason)
  if (f.kind === 'integer') {
    return (
      <input
        id={idBase}
        className="input"
        type="number"
        inputMode="numeric"
        min={f.min ?? undefined}
        max={f.max ?? undefined}
        placeholder={t('auto')}
        disabled={disabled}
        value={typeof value === 'number' ? value : ''}
        onChange={(e) => {
          const raw = e.target.value
          if (raw === '') return onChange(null)
          const n = Number(raw)
          if (Number.isFinite(n)) onChange(Math.trunc(n))
        }}
        aria-describedby={`${idBase}-help`}
      />
    )
  }
  if (f.kind === 'multi') {
    const selected = Array.isArray(value) ? (value as string[]) : []
    return (
      <div className="chips" role="group" aria-labelledby={`${idBase}-label`}>
        {f.choices.map((c) => {
          const on = selected.includes(c.value)
          return (
            <label key={c.value} className={`chip${on ? ' on' : ''}${!c.available ? ' off' : ''}`} title={c.reason ?? undefined}>
              <input
                type="checkbox"
                checked={on}
                disabled={disabled || !c.available}
                onChange={() => {
                  const next = on ? selected.filter((x) => x !== c.value) : [...selected, c.value]
                  onChange(next.length ? next : null)
                }}
              />
              {c.label}
            </label>
          )
        })}
        {selected.length === 0 && <span className="faint" style={{ alignSelf: 'center' }}>{t('auto')}</span>}
      </div>
    )
  }
  return (
    <select
      id={idBase}
      className="select"
      disabled={disabled}
      value={typeof value === 'string' ? value : ''}
      onChange={(e) => onChange(e.target.value === '' ? null : e.target.value)}
      aria-describedby={`${idBase}-help`}
    >
      <option value="">{t('auto')}</option>
      {f.choices.map((c) => (
        <option key={c.value} value={c.value} disabled={!c.available}>
          {c.label}
          {!c.available ? ` — ${t('unavailable')}` : ''}
        </option>
      ))}
    </select>
  )
}

export function SettingsFields({
  caps, value, onChange, inForce, originOf, idPrefix = 'set', only,
}: {
  caps: Capabilities
  value: SettingsObj
  onChange: (next: SettingsObj) => void
  inForce?: InForce
  /** The layer this panel edits, shown as the origin of any value it holds. */
  originOf?: (key: string, v: unknown) => Origin
  idPrefix?: string
  only?: string[]
}) {
  const { t } = useI18n()
  const byKey = new Map(caps.fields.map((f) => [f.key, f]))
  const set = (key: string, v: unknown) => {
    const next = { ...value } as Record<string, unknown>
    if (v === null || v === undefined) delete next[fieldOf(key)]
    else next[fieldOf(key)] = v
    onChange(next as SettingsObj)
  }
  return (
    <div className="stack" style={{ gap: 20 }}>
      {GROUPS.map((g) => {
        const keys = g.keys.filter((k) => byKey.has(k) && (!only || only.includes(k)))
        if (keys.length === 0) return null
        return (
          <fieldset key={g.id} className="fieldset">
            <legend>{t(g.id)}</legend>
            <div className="settings-grid">
              {keys.map((key) => {
                const f = byKey.get(key) as FieldSchema
                const v = (value as Record<string, unknown>)[fieldOf(key)]
                const idBase = `${idPrefix}-${key}`
                const live = inForce?.[key]
                const manual = v !== undefined && v !== null
                return (
                  <div key={key} className={`setting${manual ? ' manual' : ''}`}>
                    <div className="row" style={{ justifyContent: 'space-between', gap: 8 }}>
                      <label id={`${idBase}-label`} htmlFor={idBase} className="setting-label">
                        {t(LABEL[key] ?? 'set.mode')}
                      </label>
                      {manual ? (
                        <button type="button" className="btn ghost sm" onClick={() => set(key, null)} aria-label={`${t('reset_auto')}: ${t(LABEL[key] ?? 'set.mode')}`}>
                          {t('reset_auto')}
                        </button>
                      ) : (
                        <Pill tone="neutral">{t('auto')}</Pill>
                      )}
                    </div>
                    <FieldControl f={f} value={v} onChange={(nv) => set(key, nv)} idBase={idBase} />
                    <div id={`${idBase}-help`} className="hint">
                      {f.disabled_reason ? <span style={{ color: 'var(--warn)' }}>{f.disabled_reason}</span> : t(HELP[key] ?? 'help.mode')}
                      {f.kind === 'integer' && f.min !== null && f.max !== null ? ` (${f.min}–${f.max})` : ''}
                    </div>
                    {manual && originOf && <div className="hint">{t(ORIGIN_KEY[originOf(key, v)])}</div>}
                    {live && !manual && (
                      <div className="hint">
                        {t('in_force')}: <b>{formatLive(live.value)}</b> · {live.origin_label}
                      </div>
                    )}
                  </div>
                )
              })}
            </div>
          </fieldset>
        )
      })}
    </div>
  )
}

function formatLive(v: unknown): string {
  if (v === null || v === undefined) return 'AUTO'
  if (Array.isArray(v)) return v.join(', ')
  return String(v)
}

export function hasAny(s: SettingsObj): boolean {
  return Object.values(s).some((v) => v !== undefined && v !== null)
}
