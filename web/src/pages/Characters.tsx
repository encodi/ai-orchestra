// Characters: a name, a short profile and a look for each agent. The name and the profile are written into the
// prompts of the calls that write prose; the look is only for the interface. None of it changes what an agent
// is allowed to do, and the page says so.
import { useEffect, useState } from 'react'
import type { CharacterEntry, CharactersFile } from '../api/types'
import { ACCESSORIES, PixelRobot } from '../components/PixelRobot'
import { Field, Notice, Pill } from '../components/ui'
import { useI18n, type TKey } from '../i18n'
import { agentKey } from '../lib/format'
import { DEFAULT_FILE, useCharacters } from '../state/characters'
import { useStore } from '../state/store'

const PRESETS: { key: TKey; text: TKey }[] = [
  { key: 'char.preset.analytic', text: 'char.preset.analytic.text' },
  { key: 'char.preset.skeptic', text: 'char.preset.skeptic.text' },
  { key: 'char.preset.creative', text: 'char.preset.creative.text' },
  { key: 'char.preset.pragmatic', text: 'char.preset.pragmatic.text' },
  { key: 'char.preset.teacher', text: 'char.preset.teacher.text' },
]
const ACC_KEY: Record<string, TKey> = { none: 'char.acc.none', glasses: 'char.acc.glasses', cap: 'char.acc.cap', headphones: 'char.acc.headphones' }
const EMPTY: CharacterEntry = { name: '', personality: '', appearance: { palette: 0, accessory: 'none' } }

function CharacterCard({ id, label, entry, onChange, nameMax, textMax }: { id: string; label: string; entry: CharacterEntry; onChange: (e: CharacterEntry) => void; nameMax: number; textMax: number }) {
  const { t } = useI18n()
  const kind = agentKey(id)
  const { palette, accessory } = entry.appearance
  const set = (patch: Partial<CharacterEntry>) => onChange({ ...entry, ...patch })
  const setLook = (patch: Partial<CharacterEntry['appearance']>) => set({ appearance: { ...entry.appearance, ...patch } })
  return (
    <section className="card char-card" aria-label={label}>
      <div className="char-preview" aria-hidden="true">
        <PixelRobot kind={kind} state="idle" size={96} palette={palette} accessory={accessory} decorative />
        <div className="row" style={{ gap: 6 }}>
          <PixelRobot kind={kind} state="thinking" size={40} palette={palette} accessory={accessory} decorative />
          <PixelRobot kind={kind} state="speaking" size={40} palette={palette} accessory={accessory} decorative />
          <PixelRobot kind={kind} state="idle" size={40} palette={palette} accessory={accessory} walking decorative />
        </div>
      </div>
      <div className="stack" style={{ flex: 1, minWidth: 0 }}>
        <div className="row wrap" style={{ gap: 8 }}>
          <h3 style={{ margin: 0 }}>{entry.name.trim() || label}</h3>
          <Pill>{label}</Pill>
          <span className="spacer" />
          <button type="button" className="btn ghost sm" onClick={() => onChange(EMPTY)} disabled={JSON.stringify(entry) === JSON.stringify(EMPTY)}>{t('char.reset')}</button>
        </div>
        <Field label={t('char.name')}>
          {(fid) => (
            <>
              <input id={fid} className="input" value={entry.name} maxLength={nameMax} placeholder={label} onChange={(e) => set({ name: e.target.value })} />
            </>
          )}
        </Field>
        <Field label={t('char.personality')}>
          {(fid) => (
            <>
              <textarea id={fid} className="textarea" style={{ minHeight: 84 }} value={entry.personality} maxLength={textMax} placeholder={t('char.personality.ph')} onChange={(e) => set({ personality: e.target.value })} aria-describedby={`${fid}-count`} />
              <span id={`${fid}-count`} className="hint">{entry.personality.length}/{textMax}</span>
            </>
          )}
        </Field>
        <div className="chips" role="group" aria-label={t('char.presets')}>
          {PRESETS.map((p) => (
            <button key={p.key} type="button" className="chip" onClick={() => set({ personality: t(p.text) })}>{t(p.key)}</button>
          ))}
        </div>
        <fieldset className="fieldset">
          <legend>{t('char.look')}</legend>
          <div className="row wrap" style={{ gap: 16 }}>
            <div className="chips" role="radiogroup" aria-label={t('char.palette')}>
              {[0, 1, 2].map((p) => (
                <label key={p} className={`chip palette-chip${palette === p ? ' on' : ''}`}>
                  <input type="radio" name={`palette-${id}`} checked={palette === p} onChange={() => setLook({ palette: p })} />
                  <PixelRobot kind={kind} state="idle" size={28} palette={p} accessory="none" decorative />
                  <span>{t('char.palette.n', { n: p + 1 })}</span>
                </label>
              ))}
            </div>
            <div className="chips" role="radiogroup" aria-label={t('char.accessory')}>
              {ACCESSORIES.map((a) => (
                <label key={a} className={`chip${accessory === a ? ' on' : ''}`}>
                  <input type="radio" name={`acc-${id}`} checked={accessory === a} onChange={() => setLook({ accessory: a })} />
                  {t(ACC_KEY[a] as TKey)}
                </label>
              ))}
            </div>
          </div>
        </fieldset>
      </div>
    </section>
  )
}

export function CharactersPanel() {
  const { t } = useI18n()
  const { caps } = useStore()
  const { file, limits, note, save, loaded } = useCharacters()
  const [draft, setDraft] = useState<CharactersFile>(DEFAULT_FILE)
  const [msg, setMsg] = useState<{ tone: 'ok' | 'danger'; text: string } | null>(null)
  useEffect(() => setDraft(file), [file])
  if (!caps || !loaded) return null
  const dirty = JSON.stringify(draft) !== JSON.stringify(file)
  const entryOf = (id: string): CharacterEntry => draft.characters[id] ?? EMPTY
  const setEntry = (id: string, e: CharacterEntry) => {
    const next = { ...draft.characters }
    if (JSON.stringify(e) === JSON.stringify(EMPTY)) delete next[id]
    else next[id] = e
    setDraft({ ...draft, characters: next })
  }
  const doSave = async () => {
    const err = await save(draft)
    setMsg(err ? { tone: 'danger', text: err } : { tone: 'ok', text: t('char.saved') })
  }
  return (
    <div className="stack" style={{ gap: 16 }}>
      <Notice tone="info">
        {t('char.honest')}
        {note ? <div className="hint" style={{ marginTop: 4 }}>{t('char.honest2')}</div> : null}
      </Notice>
      {msg && <Notice tone={msg.tone} role="status">{msg.text}</Notice>}
      <section className="card flat stack">
        <h3 style={{ margin: 0 }}>{t('char.global')}</h3>
        <div className="row wrap" style={{ gap: 24, alignItems: 'flex-start' }}>
          <Field label={t('char.language')} style={{ minWidth: 240 }}>
            {(fid) => (
              <>
                <select id={fid} className="select" value={draft.reply_language} onChange={(e) => setDraft({ ...draft, reply_language: e.target.value as CharactersFile['reply_language'] })} aria-describedby={`${fid}-h`}>
                  <option value="auto">{t('char.language.auto')}</option>
                  <option value="es">Español</option>
                  <option value="en">English</option>
                </select>
                <span id={`${fid}-h`} className="hint">{t('char.language.help')}</span>
              </>
            )}
          </Field>
          <label className="row" style={{ gap: 8, alignItems: 'flex-start', maxWidth: 460 }}>
            <input type="checkbox" checked={draft.conversational} onChange={(e) => setDraft({ ...draft, conversational: e.target.checked })} style={{ marginTop: 4 }} />
            <span>
              <strong>{t('char.conversational')}</strong>
              <span className="hint" style={{ display: 'block' }}>{t('char.conversational.help')}</span>
            </span>
          </label>
        </div>
      </section>
      {caps.providers.map((p) => (
        <CharacterCard key={p.id} id={p.id} label={p.display_name} entry={entryOf(p.id)} onChange={(e) => setEntry(p.id, e)} nameMax={limits?.name_chars ?? 24} textMax={limits?.personality_chars ?? 600} />
      ))}
      <div className="row">
        <button type="button" className="btn primary" disabled={!dirty} onClick={doSave}>{t('char.save')}</button>
        {dirty && <span className="hint">{t('settings.unsaved')}</span>}
      </div>
    </div>
  )
}
