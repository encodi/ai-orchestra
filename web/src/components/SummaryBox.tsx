// "Resumir": one explicit request to the coordinator for a short summary of the finished report. It is one call,
// it only starts when the button is pressed, it can be cancelled, and the result is labelled as written by a model.
import { useRef, useState } from 'react'
import { ApiError, api } from '../api/client'
import type { AiSummary, Consultation } from '../api/types'
import { useI18n } from '../i18n'
import { formatDateTime } from '../lib/format'
import { Markdown } from '../lib/markdown'
import { useAgentName } from './agents'
import { Icon } from './Icons'
import { Notice, Pill, Spinner } from './ui'

export function SummaryBox({ c }: { c: Consultation }) {
  const { t, lang } = useI18n()
  const name = useAgentName()
  const [summaries, setSummaries] = useState<AiSummary[]>(c.summaries ?? [])
  const [agent, setAgent] = useState('')
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const abort = useRef<AbortController | null>(null)
  const canSummarize = c.final_answer.trim().length > 0 && (c.status === 'complete' || c.status === 'partial')
  if (!canSummarize && summaries.length === 0) return null
  const writers = [c.coordinator, ...c.participants.filter((p) => p !== c.coordinator)]

  const run = async () => {
    setBusy(true)
    setError(null)
    const ctl = new AbortController()
    abort.current = ctl
    try {
      const r = await api.summarize(c.id, agent || undefined, ctl.signal)
      setSummaries((x) => [...x, r.summary].slice(-5))
    } catch (e) {
      if (!(e instanceof DOMException && e.name === 'AbortError')) setError(e instanceof ApiError || e instanceof Error ? e.message : String(e))
    } finally {
      setBusy(false)
      abort.current = null
    }
  }
  const newest = summaries[summaries.length - 1]
  return (
    <section className="summary-box" aria-label={t('summary.title')}>
      <div className="row wrap" style={{ gap: 8 }}>
        <strong>{t('summary.title')}</strong>
        <span className="spacer" />
        {canSummarize && !busy && (
          <>
            {writers.length > 1 && (
              <>
                <label htmlFor={`sum-agent-${c.id}`} className="sr-only">{t('summary.by')}</label>
                <select id={`sum-agent-${c.id}`} className="select" style={{ width: 'auto' }} value={agent} onChange={(e) => setAgent(e.target.value)}>
                  <option value="">{t('summary.by_coordinator', { name: name(c.coordinator) })}</option>
                  {writers.slice(1).map((w) => <option key={w} value={w}>{name(w)}</option>)}
                </select>
              </>
            )}
            <button type="button" className="btn primary sm" onClick={run}>
              <Icon name="list" size={14} /> {summaries.length ? t('summary.again') : t('summary.button')}
            </button>
          </>
        )}
        {busy && (
          <>
            <Spinner label={t('summary.writing', { name: name(agent || c.coordinator) })} />
            <button type="button" className="btn danger sm" onClick={() => abort.current?.abort()}>{t('cancel')}</button>
          </>
        )}
      </div>
      {error && <Notice tone="danger">{error}</Notice>}
      {newest && (
        <div className="summary-text">
          <div className="row wrap" style={{ gap: 8, marginBottom: 6 }}>
            <Pill tone="info">{t('summary.written_by', { name: name(newest.agent) })}</Pill>
            <span className="faint" style={{ fontSize: 'var(--text-xs)' }}>{formatDateTime(newest.created_ms, lang)}</span>
          </div>
          <Markdown text={newest.text} />
          <p className="hint">{t('summary.note')}{newest.report_truncated ? ` ${t('summary.truncated')}` : ''}</p>
        </div>
      )}
      {summaries.length > 1 && (
        <details>
          <summary className="hint">{t('summary.older', { n: summaries.length - 1 })}</summary>
          {summaries.slice(0, -1).reverse().map((s) => (
            <div key={s.id} className="summary-text" style={{ marginTop: 8 }}>
              <div className="hint">{name(s.agent)} · {formatDateTime(s.created_ms, lang)}</div>
              <Markdown text={s.text} />
            </div>
          ))}
        </details>
      )}
      {!newest && !busy && canSummarize && <p className="hint" style={{ margin: 0 }}>{t('summary.hint')}</p>}
    </section>
  )
}
