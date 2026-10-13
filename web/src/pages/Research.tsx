import { useEffect, useState } from 'react'
import { api } from '../api/client'
import type { HistoryItem, ResearchResponse } from '../api/types'
import { ResearchView } from '../components/ResearchView'
import { Empty, Notice, Pill, Spinner } from '../components/ui'
import { useI18n } from '../i18n'
import { formatDateTime, truncate } from '../lib/format'
import { navigate } from '../lib/router'

export function ResearchPage({ id }: { id?: string }) {
  const { t, lang } = useI18n()
  const [items, setItems] = useState<HistoryItem[] | null>(null)
  const [data, setData] = useState<ResearchResponse | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [focus, setFocus] = useState<number | null>(null)

  useEffect(() => {
    let alive = true
    api.history({ limit: 100 }).then((r) => alive && setItems(r.items.filter((i) => i.has_research))).catch((e: unknown) => alive && setError(e instanceof Error ? e.message : String(e)))
    return () => { alive = false }
  }, [])
  useEffect(() => {
    if (!id) return
    let alive = true
    setData(null)
    setFocus(null)
    api.research(id).then((r) => alive && setData(r)).catch((e: unknown) => alive && setError(e instanceof Error ? e.message : String(e)))
    return () => { alive = false }
  }, [id])

  return (
    <div className="page">
      <div className="page-head">
        <div>
          <h1>{t('research.title')}</h1>
          <p>{t('research.subtitle')}</p>
        </div>
      </div>
      {error && <Notice tone="danger">{error}</Notice>}
      <div className="split">
        <aside className="card flat split-list" aria-label={t('research.list')}>
          <h3>{t('research.list')}</h3>
          {!items && !error && <Spinner label={t('loading')} />}
          {items && items.length === 0 && <Empty title={t('research.none_saved')}><p className="hint">{t('research.none_saved_hint')}</p></Empty>}
          <ul className="plain-list">
            {items?.map((it) => (
              <li key={it.id}>
                <a href={`#/research/${encodeURIComponent(it.id)}`} aria-current={it.id === id ? 'page' : undefined} className={`pick${it.id === id ? ' on' : ''}`}>
                  <span>{truncate(it.question, 90)}</span>
                  <span className="faint" style={{ fontSize: 'var(--text-xs)' }}>
                    {formatDateTime(it.created_at_ms, lang)} · <Pill tone="info">{t('history.sources_n', { n: it.retrieved_sources ?? 0 })}</Pill>
                  </span>
                </a>
              </li>
            ))}
          </ul>
        </aside>
        <section className="split-main" aria-live="polite">
          {!id && <p className="muted">{t('research.pick')}</p>}
          {id && !data && !error && <Spinner label={t('loading')} />}
          {id && data && (
            <>
              <div className="row" style={{ marginBottom: 12 }}>
                <a className="btn sm" href={`#/history/${encodeURIComponent(id)}`}>{t('history.open')}</a>
                <button type="button" className="btn sm" onClick={() => navigate(`/theater/${encodeURIComponent(id)}`)}>{t('history.replay')}</button>
              </div>
              <ResearchView data={data} focus={focus} onFocus={setFocus} />
            </>
          )}
        </section>
      </div>
    </div>
  )
}
