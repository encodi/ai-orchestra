import { Fragment, useEffect, useState } from 'react'
import { api } from '../api/client'
import type { ProviderRow, ProvidersResponse, SettingsObj } from '../api/types'
import { Icon } from '../components/Icons'
import { SettingsFields, hasAny } from '../components/SettingsPanel'
import { Field, Notice, Pill, Spinner, TabPanel, Tabs } from '../components/ui'
import { useI18n } from '../i18n'
import { formatDateTime, formatDuration } from '../lib/format'
import { CharactersPanel } from './Characters'
import { usePrefs } from '../state/prefs'
import { useStore } from '../state/store'

type Tab = 'defaults' | 'characters' | 'interface' | 'providers' | 'safety'

function Defaults() {
  const { t } = useI18n()
  const { caps } = useStore()
  const [saved, setSaved] = useState<SettingsObj | null>(null)
  const [draft, setDraft] = useState<SettingsObj>({})
  const [note, setNote] = useState('')
  const [msg, setMsg] = useState<{ tone: 'ok' | 'danger'; text: string } | null>(null)
  useEffect(() => {
    api.getSettings().then((r) => { setSaved(r.defaults); setDraft(r.defaults); setNote(r.note) }).catch((e: unknown) => setMsg({ tone: 'danger', text: e instanceof Error ? e.message : String(e) }))
  }, [])
  const dirty = JSON.stringify(saved) !== JSON.stringify(draft)
  const save = async () => {
    try {
      const r = await api.putSettings(draft)
      setSaved(r.defaults)
      setDraft(r.defaults)
      setMsg({ tone: 'ok', text: t('settings.saved') })
    } catch (e) {
      setMsg({ tone: 'danger', text: e instanceof Error ? e.message : String(e) })
    }
  }
  if (!caps || !saved) return msg ? <Notice tone="danger">{msg.text}</Notice> : <Spinner label={t('loading')} />
  return (
    <div className="stack" style={{ gap: 16 }}>
      <Notice tone="info">{note || t('settings.defaults_help')}</Notice>
      <p className="hint">{t('settings.layers')}</p>
      {msg && <Notice tone={msg.tone} role="status">{msg.text}</Notice>}
      <SettingsFields caps={caps} value={draft} onChange={setDraft} originOf={() => 'preference'} idPrefix="saved" />
      <div className="row">
        <button type="button" className="btn primary" disabled={!dirty} onClick={save}>{t('settings.save')}</button>
        <button type="button" className="btn" disabled={!hasAny(draft)} onClick={() => setDraft({})}><Icon name="reset" size={14} /> {t('reset_all_auto')}</button>
        {dirty && <span className="hint">{t('settings.unsaved')}</span>}
      </div>
    </div>
  )
}

function Interface() {
  const { t } = useI18n()
  const { prefs, update } = usePrefs()
  const [msg, setMsg] = useState<string | null>(null)
  const set = async (patch: Partial<typeof prefs>) => setMsg(await update(patch))
  return (
    <div className="stack" style={{ gap: 16, maxWidth: 560 }}>
      {msg && <Notice tone="warn">{t('settings.not_persisted')}: {msg}</Notice>}
      <Field label={t('settings.theme')}>{(id) => (<>
        <select id={id} className="select" value={prefs.theme} onChange={(e) => set({ theme: e.target.value as typeof prefs.theme })}>
          <option value="dark">{t('theme.dark')}</option><option value="light">{t('theme.light')}</option><option value="system">{t('theme.system')}</option>
        </select>
      </>)}</Field>
      <Field label={t('settings.language')}>{(id) => (<>
        <select id={id} className="select" value={prefs.language} onChange={(e) => set({ language: e.target.value as typeof prefs.language })}>
          <option value="es">Español</option><option value="en">English</option>
        </select>
      </>)}</Field>
      <Field label={t('settings.motion')}>{(id) => (<>
        <select id={id} className="select" value={prefs.reduced_motion} onChange={(e) => set({ reduced_motion: e.target.value as typeof prefs.reduced_motion })}>
          <option value="system">{t('motion.system')}</option><option value="on">{t('motion.reduced')}</option><option value="off">{t('motion.full')}</option>
        </select>
        <span className="hint">{t('settings.motion_help')}</span>
      </>)}</Field>
      <label className="row" style={{ gap: 8 }}>
        <input type="checkbox" checked={prefs.theater_default} onChange={(e) => set({ theater_default: e.target.checked })} />
        <span>{t('settings.theater_default')}</span>
      </label>
      <label className="row" style={{ gap: 8 }}>
        <input type="checkbox" checked={prefs.show_stage} onChange={(e) => set({ show_stage: e.target.checked })} />
        <span>{t('settings.show_stage')}</span>
      </label>
    </div>
  )
}

function ProviderCard({ p }: { p: ProviderRow }) {
  const { t, lang } = useI18n()
  const h = p.history
  return (
    <div className="card provider">
      <div className="row wrap" style={{ gap: 8 }}>
        <h3 style={{ margin: 0 }}>{p.display_name}</h3>
        <span className="faint mono">{p.id}</span>
        {p.usable ? <Pill tone="ok">{t('prov.usable')}</Pill> : <Pill tone="danger">{t('prov.unusable')}</Pill>}
        {!p.enabled && <Pill tone="warn">{t('prov.disabled')}</Pill>}
        <Pill tone={p.installed ? 'ok' : 'danger'}>{p.installed ? t('prov.installed') : t('prov.not_installed')}</Pill>
      </div>
      {p.reason && <div className="hint" style={{ color: 'var(--warn)' }}>{p.reason}</div>}
      <dl className="kv">
        <dt>{t('prov.version')}</dt><dd>{p.version ?? t('prov.unknown')}</dd>
        <dt>{t('prov.executable')}</dt><dd className="mono">{p.executable ?? '–'}</dd>
        <dt>{t('prov.auth')}</dt><dd>{t('prov.auth_unknown')}<div className="hint">{t('prov.auth_note')}</div></dd>
        {p.configured_model && (<><dt>{t('prov.model')}</dt><dd>{p.configured_model}</dd></>)}
        <dt>{t('prov.web')}</dt><dd>{String((p.capabilities as { web_search?: string }).web_search ?? '–')}</dd>
      </dl>
      <h4 style={{ marginTop: 14 }}>{t('prov.history')}</h4>
      {!h || h.invocations === 0 ? <p className="muted" style={{ margin: 0 }}>{t('prov.no_history')}</p> : (
        <dl className="kv">
          <dt>{t('prov.invocations')}</dt><dd>{h.invocations} ({h.successes} ok · {h.failures} {t('reports.failed').toLowerCase()} · {h.timeouts} timeouts · {h.quota_errors} quota · {h.auth_errors} auth)</dd>
          {Object.entries(h.latency_ms).map(([k, v]) => (<Fragment key={k}><dt>{t('prov.latency')} ({k})</dt><dd>{formatDuration(v.mean)} · {t('prov.samples', { n: v.samples })}</dd></Fragment>))}
          {h.last_success_ms ? (<><dt>{t('prov.last_ok')}</dt><dd>{formatDateTime(h.last_success_ms, lang)}</dd></>) : null}
          {h.last_failure_ms ? (<><dt>{t('prov.last_fail')}</dt><dd>{formatDateTime(h.last_failure_ms, lang)} {h.last_failure_kind ? `(${h.last_failure_kind})` : ''}</dd></>) : null}
          {h.tokens_reported.calls > 0 && (<><dt>{t('reports.tokens')}</dt><dd>{t('reports.tokens_value', { i: h.tokens_reported.input, o: h.tokens_reported.output, n: h.tokens_reported.calls })}</dd></>)}
        </dl>
      )}
    </div>
  )
}

function Providers() {
  const { t } = useI18n()
  const [data, setData] = useState<ProvidersResponse | null>(null)
  const [err, setErr] = useState<string | null>(null)
  useEffect(() => { api.providers().then(setData).catch((e: unknown) => setErr(e instanceof Error ? e.message : String(e))) }, [])
  if (err) return <Notice tone="danger">{err}</Notice>
  if (!data) return <Spinner label={t('loading')} />
  return (
    <div className="stack" style={{ gap: 16 }}>
      <Notice tone="info">{t('prov.note')}</Notice>
      <div className="grid2">{data.providers.map((p) => <ProviderCard key={p.id} p={p} />)}</div>
      <p className="hint">{data.history_note}</p>
    </div>
  )
}

function Safety() {
  const { t } = useI18n()
  const { caps } = useStore()
  return (
    <div className="stack">
      <Notice tone="ok">{t('safety.intro')}</Notice>
      <ul>{caps?.safety.map((s, i) => <li key={i}>{s}</li>)}</ul>
      <h4>{t('safety.precedence')}</h4>
      <ol>{caps?.precedence.map((s, i) => <li key={i}>{s}</li>)}</ol>
      <h4>{t('safety.web')}</h4>
      <ul>{(['safety.w1', 'safety.w2', 'safety.w3', 'safety.w4'] as const).map((k) => <li key={k}>{t(k)}</li>)}</ul>
    </div>
  )
}

export function SettingsPage() {
  const { t } = useI18n()
  const [tab, setTab] = useState<Tab>('defaults')
  return (
    <div className="page">
      <div className="page-head">
        <div>
          <h1>{t('settings.title')}</h1>
          <p>{t('settings.subtitle')}</p>
        </div>
      </div>
      <Tabs label={t('settings.title')} value={tab} onChange={setTab} tabs={[
        { id: 'defaults', label: t('settings.tab_defaults') }, { id: 'characters', label: t('settings.tab_characters') }, { id: 'interface', label: t('settings.tab_interface') },
        { id: 'providers', label: t('settings.tab_providers') }, { id: 'safety', label: t('settings.tab_safety') },
      ]} />
      <TabPanel id={tab}>{tab === 'defaults' ? <Defaults /> : tab === 'characters' ? <CharactersPanel /> : tab === 'interface' ? <Interface /> : tab === 'providers' ? <Providers /> : <Safety />}</TabPanel>
    </div>
  )
}
