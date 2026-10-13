import { useCallback, useEffect, useState } from 'react'
import { api } from '../api/client'
import type { Consultation, HistoryItem, HistoryResponse, OneResponse } from '../api/types'
import { Icon } from '../components/Icons'
import { ResultPanel } from '../components/Turn'
import { Dialog, Empty, Field, Notice, Pill, Spinner, statusTone } from '../components/ui'
import { useI18n, type TKey } from '../i18n'
import { download, formatDateTime, truncate } from '../lib/format'
import { navigate } from '../lib/router'
import { emptyView } from '../state/job'
import { useStore } from '../state/store'

const PAGE = 15
const STALE_DAYS = 30

type Filters = { q: string; mode: string; provider: string; strategy: string; status: string; from: string; to: string }
const NO_FILTERS: Filters = { q: '', mode: '', provider: '', strategy: '', status: '', from: '', to: '' }

const dayStart = (d: string) => (d ? new Date(`${d}T00:00:00`).getTime() : undefined)
const dayEnd = (d: string) => (d ? new Date(`${d}T23:59:59.999`).getTime() : undefined)

export function HistoryPage() {
  const { t, lang } = useI18n()
  const { caps } = useStore()
  const [filters, setFilters] = useState<Filters>(NO_FILTERS)
  const [applied, setApplied] = useState<Filters>(NO_FILTERS)
  const [offset, setOffset] = useState(0)
  const [data, setData] = useState<HistoryResponse | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [toDelete, setToDelete] = useState<HistoryItem | null>(null)
  const [busy, setBusy] = useState(false)
  const [notice, setNotice] = useState<string | null>(null)

  const load = useCallback(() => {
    setError(null)
    api
      .history({
        q: applied.q, mode: applied.mode, provider: applied.provider, strategy: applied.strategy, status: applied.status,
        from: dayStart(applied.from), to: dayEnd(applied.to), limit: PAGE, offset,
      })
      .then(setData)
      .catch((e: unknown) => setError(e instanceof Error ? e.message : String(e)))
  }, [applied, offset])
  useEffect(load, [load])

  const submit = (e: React.FormEvent) => {
    e.preventDefault()
    setOffset(0)
    setApplied(filters)
  }
  const reset = () => {
    setFilters(NO_FILTERS)
    setApplied(NO_FILTERS)
    setOffset(0)
  }
  const remove = async () => {
    if (!toDelete) return
    setBusy(true)
    try {
      const r = await api.remove(toDelete.id)
      void r
      setNotice(t('history.deleted'))
      setToDelete(null)
      load()
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e))
      setToDelete(null)
    } finally {
      setBusy(false)
    }
  }
  const exportOne = async (id: string) => {
    try {
      const [one, tl] = await Promise.all([api.one(id), api.theater(id).catch(() => null)])
      download(`consultation-${id}.json`, 'application/json', JSON.stringify({ exported_by: 'AI Orchestra web', consultation: one.consultation, recorded_steps: tl }, null, 2))
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e))
    }
  }
  const providers = caps?.providers.map((p) => p.id) ?? []
  const set = (k: keyof Filters) => (e: React.ChangeEvent<HTMLInputElement | HTMLSelectElement>) => setFilters({ ...filters, [k]: e.target.value })
  const total = data?.total ?? 0

  return (
    <div className="page">
      <div className="page-head">
        <div>
          <h1>{t('history.title')}</h1>
          <p>{t('history.subtitle')}</p>
        </div>
      </div>
      <form className="card flat filters" onSubmit={submit} role="search" aria-label={t('history.filters')}>
        <label className="field" style={{ flex: 2, minWidth: 220 }}>
          {t('history.search')}
          <input className="input" type="search" value={filters.q} onChange={set('q')} placeholder={t('history.search_ph')} />
        </label>
        <Field label={t('d.mode')}>{(id) => (<>
          <select id={id} className="select" value={filters.mode} onChange={set('mode')}>
            <option value="">{t('any')}</option>
            {(caps?.modes ?? []).map((m) => <option key={m} value={m}>{m}</option>)}
          </select>
        </>)}</Field>
        <Field label={t('d.strategy')}>{(id) => (<>
          <select id={id} className="select" value={filters.strategy} onChange={set('strategy')}>
            <option value="">{t('any')}</option>
            {['single', 'adaptive', 'panel', 'debate', 'fast', 'balanced', 'thorough'].map((m) => <option key={m} value={m}>{m}</option>)}
          </select>
        </>)}</Field>
        <Field label={t('history.provider')}>{(id) => (<>
          <select id={id} className="select" value={filters.provider} onChange={set('provider')}>
            <option value="">{t('any')}</option>
            {providers.map((m) => <option key={m} value={m}>{m}</option>)}
          </select>
        </>)}</Field>
        <Field label={t('d.status')}>{(id) => (<>
          <select id={id} className="select" value={filters.status} onChange={set('status')}>
            <option value="">{t('any')}</option>
            {['complete', 'partial', 'failed', 'cancelled'].map((m) => <option key={m} value={m}>{t(`status.${m}` as TKey)}</option>)}
          </select>
        </>)}</Field>
        <Field label={t('history.from')}>{(id) => (<><input id={id} className="input" type="date" value={filters.from} onChange={set('from')} /></>)}</Field>
        <Field label={t('history.to')}>{(id) => (<><input id={id} className="input" type="date" value={filters.to} onChange={set('to')} /></>)}</Field>
        <div className="row" style={{ alignItems: 'flex-end' }}>
          <button type="submit" className="btn primary">{t('history.apply')}</button>
          <button type="button" className="btn" onClick={reset}>{t('history.clear')}</button>
        </div>
      </form>

      {error && <Notice tone="danger">{error}</Notice>}
      {notice && <Notice tone="ok" role="status">{notice}</Notice>}
      {!data && !error && <Spinner label={t('loading')} />}
      {data && !data.history_enabled && <Notice tone="warn">{t('history.disabled')}</Notice>}

      {data && data.running.length > 0 && (
        <section className="card" style={{ margin: '16px 0' }}>
          <h2>{t('history.running')}</h2>
          {data.running.map((j) => (
            <div key={j.job_id} className="row wrap hist-row">
              <Pill tone="info">{t('status.running')}</Pill>
              <span style={{ flex: 1, minWidth: 0 }}>{truncate(j.question_preview, 120)}</span>
              <a className="btn sm" href={`#/theater/${encodeURIComponent(j.job_id)}`}>{t('history.watch')}</a>
            </div>
          ))}
        </section>
      )}

      {data && data.history_enabled && data.items.length === 0 && <Empty title={t('history.empty')}><p>{t('history.empty_hint')}</p></Empty>}
      <ul className="plain-list hist-list">
        {data?.items.map((it) => {
          const age = Math.floor((Date.now() - it.created_at_ms) / 86_400_000)
          return (
            <li key={it.id} className="card hist-item">
              <div className="row wrap" style={{ gap: 8 }}>
                <Pill tone={statusTone(it.status)}>{t(`status.${it.status}` as TKey)}</Pill>
                {it.mode && <Pill>{it.mode}</Pill>}
                {it.strategy && <Pill>{it.strategy}</Pill>}
                {it.has_research && <Pill tone="info">{t('history.sources_n', { n: it.retrieved_sources ?? 0 })}</Pill>}
                <span className="faint">{formatDateTime(it.created_at_ms, lang)}</span>
                <span className="faint">{[it.coordinator, ...it.participants].filter((v, i, a) => a.indexOf(v) === i).join(' · ')}</span>
              </div>
              <a className="hist-q" href={`#/history/${encodeURIComponent(it.id)}`}>{truncate(it.question, 200)}</a>
              {it.answer_preview && <p className="muted" style={{ margin: 0 }}>{truncate(it.answer_preview, 220)}</p>}
              {age > STALE_DAYS && it.has_research && <div className="hint" style={{ color: 'var(--warn)' }}>{t('history.stale', { n: age })}</div>}
              <div className="row wrap" style={{ gap: 6 }}>
                <a className="btn sm" href={`#/history/${encodeURIComponent(it.id)}`}>{t('history.open')}</a>
                <a className="btn sm" href={`#/theater/${encodeURIComponent(it.id)}`}><Icon name="theater" size={14} /> {t('history.replay')}</a>
                <button type="button" className="btn sm" onClick={() => exportOne(it.id)}><Icon name="download" size={14} /> {t('result.export')}</button>
                <button type="button" className="btn sm danger" onClick={() => setToDelete(it)}><Icon name="trash" size={14} /> {t('delete')}</button>
              </div>
            </li>
          )
        })}
      </ul>
      {data && total > PAGE && (
        <nav className="row" style={{ justifyContent: 'center', marginTop: 16 }} aria-label={t('history.pagination')}>
          <button type="button" className="btn" disabled={offset === 0} onClick={() => setOffset(Math.max(0, offset - PAGE))}>{t('prev')}</button>
          <span className="muted">{t('history.range', { a: offset + 1, b: Math.min(total, offset + PAGE), n: total })}</span>
          <button type="button" className="btn" disabled={offset + PAGE >= total} onClick={() => setOffset(offset + PAGE)}>{t('next')}</button>
        </nav>
      )}
      {toDelete && (
        <Dialog
          title={t('history.delete_title')} tone="danger" busy={busy} confirmLabel={t('delete')} cancelLabel={t('cancel')} onConfirm={remove} onCancel={() => setToDelete(null)}
        >
          <p>{truncate(toDelete.question, 160)}</p>
          <p className="hint">{t('history.delete_body')}</p>
        </Dialog>
      )}
    </div>
  )
}

export function HistoryDetailPage({ id }: { id: string }) {
  const { t, lang } = useI18n()
  const { setSessionId, newConversation } = useStore()
  const [one, setOne] = useState<OneResponse | null>(null)
  const [error, setError] = useState<string | null>(null)
  useEffect(() => {
    let alive = true
    api.one(id).then((r) => alive && setOne(r)).catch((e: unknown) => alive && setError(e instanceof Error ? e.message : String(e)))
    return () => { alive = false }
  }, [id])
  const c: Consultation | undefined = one?.consultation
  const session = (c?.meta as { context?: { session?: string | null } } | null | undefined)?.context?.session ?? null
  const proceed = () => {
    newConversation()
    if (session) setSessionId(session)
    navigate('/')
  }
  return (
    <div className="page">
      <div className="page-head">
        <div>
          <h1>{t('history.detail')}</h1>
          {c && <p>{formatDateTime(c.created_at_ms, lang)}</p>}
        </div>
        <span className="spacer" />
        <a className="btn" href="#/history">{t('theater.back')}</a>
        {c && session && <button type="button" className="btn primary" onClick={proceed}>{t('history.continue')}</button>}
      </div>
      {error && <Notice tone="danger">{error}</Notice>}
      {!one && !error && <Spinner label={t('loading')} />}
      {c && (
        <>
          <div className="card flat" style={{ marginBottom: 16 }}>
            <h3>{t('history.question')}</h3>
            <p style={{ whiteSpace: 'pre-wrap', overflowWrap: 'anywhere', margin: 0 }}>{c.question}</p>
          </div>
          {c.age_days > STALE_DAYS && (
            <Notice tone="warn">{t('history.stale_detail', { n: c.age_days })}</Notice>
          )}
          {!session && <p className="hint">{t('history.no_session')}</p>}
          <ResultPanel c={c} view={emptyView()} onFollowUp={() => navigate('/')} />
        </>
      )}
    </div>
  )
}
