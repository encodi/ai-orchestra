// Memory: what AI Orchestra remembers, where it came from and how to change or delete it. Nothing is ever
// remembered permanently here without an explicit, confirmed action.
import { useCallback, useEffect, useState } from 'react'
import { api } from '../api/client'
import type { MemoryRecord } from '../api/types'
import { Icon } from '../components/Icons'
import { Collapsible, Dialog, Empty, Field, Notice, Pill, Spinner, TabPanel, Tabs } from '../components/ui'
import { useI18n } from '../i18n'
import { download, formatDateTime, truncate } from '../lib/format'
import { navigate } from '../lib/router'
import { useStore } from '../state/store'

const KINDS = ['question', 'conclusion', 'finding', 'unresolved', 'evidence', 'preference', 'decision', 'requirement', 'interest', 'note']
type Tab = 'memories' | 'sessions' | 'context'

function MemoriesTab() {
  const { t, lang } = useI18n()
  const { caps } = useStore()
  const [q, setQ] = useState('')
  const [scope, setScope] = useState('')
  const [kind, setKind] = useState('')
  const [reveal, setReveal] = useState(false)
  const [items, setItems] = useState<MemoryRecord[] | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [msg, setMsg] = useState<string | null>(null)
  const [toDelete, setToDelete] = useState<MemoryRecord | null>(null)
  const [toEdit, setToEdit] = useState<MemoryRecord | null>(null)
  const [editText, setEditText] = useState('')
  const [details, setDetails] = useState<Record<string, unknown>>({})
  const [busy, setBusy] = useState(false)
  const [add, setAdd] = useState({ text: '', scope: 'project', kind: 'preference', topic: '', tags: '', allow_sensitive: false, confirm: false })

  const load = useCallback(() => {
    setError(null)
    api.memory({ q, scope, kind, reveal }).then((r) => setItems(r.items)).catch((e: unknown) => { setItems([]); setError(e instanceof Error ? e.message : String(e)) })
  }, [q, scope, kind, reveal])
  useEffect(load, [load])

  if (caps && !caps.features.memory) return <Notice tone="warn">{t('memory.unavailable')}</Notice>

  const doAdd = async () => {
    setBusy(true)
    setError(null)
    try {
      const r = await api.memoryAdd({
        text: add.text, scope: add.scope, kind: add.kind, topic: add.topic || undefined, tags: add.tags.split(',').map((x) => x.trim()).filter(Boolean),
        allow_sensitive: add.allow_sensitive, confirm: add.confirm,
      })
      setMsg(r.duplicate ? t('memory.duplicate') : t('memory.added'))
      setAdd({ ...add, text: '', topic: '', tags: '', confirm: false })
      load()
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e))
    } finally {
      setBusy(false)
    }
  }
  const doDelete = async () => {
    if (!toDelete) return
    setBusy(true)
    try {
      await api.memoryForget(toDelete.id)
      setMsg(t('memory.deleted'))
      setToDelete(null)
      load()
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e))
      setToDelete(null)
    } finally {
      setBusy(false)
    }
  }
  const doEdit = async () => {
    if (!toEdit) return
    setBusy(true)
    try {
      await api.memoryCorrect(toEdit.id, editText)
      setMsg(t('memory.corrected'))
      setToEdit(null)
      load()
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e))
      setToEdit(null)
    } finally {
      setBusy(false)
    }
  }
  const exportAll = async () => {
    try {
      download('ai-orchestra-memory.json', 'application/json', JSON.stringify(await api.memoryExport(), null, 2))
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e))
    }
  }
  const show = async (id: string) => {
    if (details[id]) return
    try {
      const d = await api.memoryShow(id, reveal)
      setDetails((x) => ({ ...x, [id]: d }))
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e))
    }
  }

  return (
    <div className="stack" style={{ gap: 16 }}>
      <Notice tone="info">{t('memory.rules')}</Notice>
      {error && <Notice tone="danger">{error}</Notice>}
      {msg && <Notice tone="ok" role="status">{msg}</Notice>}
      <Collapsible title={t('memory.add_title')}>
        <div className="stack">
          <p className="hint">{t('memory.add_help')}</p>
          <Field label={t('memory.text')}>{(id) => (<>
            <textarea id={id} className="textarea" value={add.text} maxLength={4000} onChange={(e) => setAdd({ ...add, text: e.target.value })} />
          </>)}</Field>
          <div className="grid3">
            <Field label={t('memory.scope')}>{(id) => (<>
              <select id={id} className="select" value={add.scope} onChange={(e) => setAdd({ ...add, scope: e.target.value })}>
                {['project', 'global', 'session'].map((s) => <option key={s} value={s}>{s}</option>)}
              </select>
            </>)}</Field>
            <Field label={t('memory.kind')}>{(id) => (<>
              <select id={id} className="select" value={add.kind} onChange={(e) => setAdd({ ...add, kind: e.target.value })}>
                {['preference', 'decision', 'requirement', 'interest', 'note'].map((s) => <option key={s} value={s}>{s}</option>)}
              </select>
            </>)}</Field>
            <Field label={t('memory.topic')}>{(id) => (<><input id={id} className="input" value={add.topic} onChange={(e) => setAdd({ ...add, topic: e.target.value })} /></>)}</Field>
          </div>
          <Field label={t('memory.tags')}>{(id) => (<><input id={id} className="input" value={add.tags} onChange={(e) => setAdd({ ...add, tags: e.target.value })} placeholder="a, b, c" /></>)}</Field>
          <label className="row" style={{ gap: 8 }}><input type="checkbox" checked={add.allow_sensitive} onChange={(e) => setAdd({ ...add, allow_sensitive: e.target.checked })} /><span>{t('memory.allow_sensitive')}</span></label>
          <label className="row" style={{ gap: 8 }}><input type="checkbox" checked={add.confirm} onChange={(e) => setAdd({ ...add, confirm: e.target.checked })} /><span><strong>{t('memory.confirm')}</strong></span></label>
          <div><button type="button" className="btn primary" disabled={!add.confirm || !add.text.trim() || busy} onClick={doAdd}>{t('memory.save')}</button></div>
        </div>
      </Collapsible>

      <form className="card flat filters" onSubmit={(e) => { e.preventDefault(); load() }} role="search">
        <Field label={t('history.search')} style={{ flex: 2, minWidth: 200 }}>{(id) => (<><input id={id} className="input" type="search" value={q} onChange={(e) => setQ(e.target.value)} /></>)}</Field>
        <Field label={t('memory.scope')}>{(id) => (<>
          <select id={id} className="select" value={scope} onChange={(e) => setScope(e.target.value)}><option value="">{t('any')}</option>{['session', 'project', 'global'].map((s) => <option key={s} value={s}>{s}</option>)}</select>
        </>)}</Field>
        <Field label={t('memory.kind')}>{(id) => (<>
          <select id={id} className="select" value={kind} onChange={(e) => setKind(e.target.value)}><option value="">{t('any')}</option>{KINDS.map((s) => <option key={s} value={s}>{s}</option>)}</select>
        </>)}</Field>
        <label className="row" style={{ gap: 6 }}><input type="checkbox" checked={reveal} onChange={(e) => setReveal(e.target.checked)} /><span>{t('memory.reveal')}</span></label>
        <button type="button" className="btn" onClick={exportAll}><Icon name="download" size={14} /> {t('memory.export')}</button>
      </form>

      {!items && <Spinner label={t('loading')} />}
      {items && items.length === 0 && !error && <Empty title={t('memory.empty')} />}
      <ul className="plain-list">
        {items?.map((r) => (
          <li key={r.id} className="card hist-item">
            <div className="row wrap" style={{ gap: 6 }}>
              <Pill tone={r.scope === 'global' ? 'warn' : r.scope === 'project' ? 'accent' : 'neutral'}>{r.scope}</Pill>
              <Pill>{r.kind}</Pill>
              {r.sensitive && <Pill tone="danger">{t('memory.sensitive')}</Pill>}
              {r.origin && <span className="faint">{t('memory.origin')}: {r.origin}</span>}
              {r.created_ms ? <span className="faint">{formatDateTime(r.created_ms, lang)}</span> : null}
            </div>
            <p style={{ margin: 0, overflowWrap: 'anywhere', whiteSpace: 'pre-wrap' }}>{r.text}</p>
            {(r.topic || (r.tags && r.tags.length > 0)) && <div className="hint">{[r.topic, ...(r.tags ?? [])].filter(Boolean).join(' · ')}</div>}
            <div className="row wrap" style={{ gap: 6 }}>
              {r.consultation_id && <a className="btn sm" href={`#/history/${encodeURIComponent(r.consultation_id)}`}>{t('memory.from_consultation')}</a>}
              <button type="button" className="btn sm" onClick={() => { setToEdit(r); setEditText(r.masked ? '' : r.text) }} disabled={r.masked}>{t('memory.correct')}</button>
              <button type="button" className="btn sm danger" onClick={() => setToDelete(r)}><Icon name="trash" size={14} /> {t('delete')}</button>
            </div>
            <Collapsible title={t('memory.provenance')}>
              <button type="button" className="btn sm" onClick={() => show(r.id)}>{t('memory.load_details')}</button>
              {details[r.id] ? <pre className="plain-pre" style={{ marginTop: 8 }}>{JSON.stringify(details[r.id], null, 2)}</pre> : null}
            </Collapsible>
          </li>
        ))}
      </ul>
      {toDelete && (
        <Dialog title={t('memory.delete_title')} tone="danger" busy={busy} confirmLabel={t('delete')} cancelLabel={t('cancel')} onConfirm={doDelete} onCancel={() => setToDelete(null)}>
          <p>{truncate(toDelete.text, 200)}</p>
          <p className="hint">{t('memory.delete_body')}</p>
        </Dialog>
      )}
      {toEdit && (
        <Dialog title={t('memory.correct_title')} busy={busy || !editText.trim()} confirmLabel={t('memory.correct')} cancelLabel={t('cancel')} onConfirm={doEdit} onCancel={() => setToEdit(null)}>
          <Field label={t('memory.text')}>{(id) => (<><textarea id={id} className="textarea" data-autofocus value={editText} maxLength={4000} onChange={(e) => setEditText(e.target.value)} /></>)}</Field>
          <p className="hint">{t('memory.correct_body')}</p>
        </Dialog>
      )}
    </div>
  )
}

function SessionsTab() {
  const { t, lang } = useI18n()
  const { setSessionId, newConversation } = useStore()
  const [rows, setRows] = useState<Record<string, unknown>[] | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [toClear, setToClear] = useState<string | null>(null)
  const load = useCallback(() => {
    api.sessions().then((r) => setRows(r.items)).catch((e: unknown) => { setRows([]); setError(e instanceof Error ? e.message : String(e)) })
  }, [])
  useEffect(load, [load])
  const clear = async () => {
    if (!toClear) return
    try { await api.clearSession(toClear); setToClear(null); load() } catch (e) { setError(e instanceof Error ? e.message : String(e)); setToClear(null) }
  }
  return (
    <div className="stack">
      {error && <Notice tone="danger">{error}</Notice>}
      <p className="hint">{t('memory.sessions_note')}</p>
      {!rows && <Spinner label={t('loading')} />}
      {rows && rows.length === 0 && !error && <Empty title={t('memory.no_sessions')} />}
      {rows?.map((s) => {
        const id = String(s.id)
        return (
          <div key={id} className="card row wrap" style={{ gap: 8 }}>
            <span className="mono">{id}</span>
            {typeof s.title === 'string' && s.title && <span style={{ flex: 1, minWidth: 0 }}>{truncate(s.title, 100)}</span>}
            <span className="faint">{t('memory.turns', { n: Number(s.turns ?? 0) })} · {formatDateTime(Number(s.last_used_ms ?? 0), lang)}</span>
            <span className="spacer" />
            <button type="button" className="btn sm" onClick={() => { newConversation(); setSessionId(id); navigate('/') }}>{t('history.continue')}</button>
            <button type="button" className="btn sm danger" onClick={() => setToClear(id)}>{t('memory.clear_session')}</button>
          </div>
        )
      })}
      {toClear && (
        <Dialog title={t('memory.clear_session')} tone="danger" confirmLabel={t('delete')} cancelLabel={t('cancel')} onConfirm={clear} onCancel={() => setToClear(null)}>
          <p className="mono">{toClear}</p>
          <p className="hint">{t('memory.clear_body')}</p>
        </Dialog>
      )}
    </div>
  )
}

function ContextTab() {
  const { t } = useI18n()
  const { sessionSettings, sessionId } = useStore()
  const [question, setQuestion] = useState('')
  const [res, setRes] = useState<{ report: unknown; block: string; note: string } | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [busy, setBusy] = useState(false)
  const run = async () => {
    setBusy(true)
    setError(null)
    try { setRes(await api.contextPreview({ question, session_settings: sessionSettings, session: sessionId, new_session: !sessionId })) } catch (e) { setRes(null); setError(e instanceof Error ? e.message : String(e)) } finally { setBusy(false) }
  }
  return (
    <div className="stack">
      <p className="muted">{t('memory.context_help')}</p>
      <Field label={t('memory.context_q')}>{(id) => (<><textarea id={id} className="textarea" value={question} onChange={(e) => setQuestion(e.target.value)} /></>)}</Field>
      <div><button type="button" className="btn primary" disabled={!question.trim() || busy} onClick={run}>{t('memory.context_run')}</button></div>
      {error && <Notice tone="danger">{error}</Notice>}
      {res && (
        <>
          <Notice tone="info">{t('memory.context_note')}</Notice>
          <h4>{t('memory.context_block')}</h4>
          <pre className="plain-pre">{res.block || t('memory.context_empty')}</pre>
          {res.report ? <Collapsible title={t('memory.context_report')}><pre className="plain-pre">{JSON.stringify(res.report, null, 2)}</pre></Collapsible> : null}
        </>
      )}
    </div>
  )
}

export function MemoryPage() {
  const { t } = useI18n()
  const [tab, setTab] = useState<Tab>('memories')
  return (
    <div className="page">
      <div className="page-head">
        <div>
          <h1>{t('memory.title')}</h1>
          <p>{t('memory.subtitle')}</p>
        </div>
      </div>
      <Tabs label={t('memory.title')} value={tab} onChange={setTab} tabs={[{ id: 'memories', label: t('memory.tab_memories') }, { id: 'sessions', label: t('memory.tab_sessions') }, { id: 'context', label: t('memory.tab_context') }]} />
      <TabPanel id={tab}>{tab === 'memories' ? <MemoriesTab /> : tab === 'sessions' ? <SessionsTab /> : <ContextTab />}</TabPanel>
    </div>
  )
}
