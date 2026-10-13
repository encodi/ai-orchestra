import { useEffect, useState } from 'react'
import { api } from '../api/client'
import type { Analytics, ReportRow } from '../api/types'
import { Notice, Pill, Spinner, TabPanel, Tabs } from '../components/ui'
import { useI18n } from '../i18n'
import { useAgentName } from '../components/agents'
import type { AgentUsage, UsageWindow } from '../api/types'
import { formatDateTime, formatDuration } from '../lib/format'

type Tab = 'analytics' | 'evaluations'

function cell(v: unknown): string {
  if (v === null || v === undefined) return '–'
  if (typeof v === 'number') return Number.isInteger(v) ? String(v) : v.toFixed(2)
  if (typeof v === 'string' || typeof v === 'boolean') return String(v)
  return JSON.stringify(v)
}

function JsonTable({ rows }: { rows: unknown }) {
  const { t } = useI18n()
  if (!Array.isArray(rows) || rows.length === 0 || typeof rows[0] !== 'object') return rows ? <pre className="plain-pre">{JSON.stringify(rows, null, 2)}</pre> : null
  const cols = Array.from(new Set(rows.flatMap((r) => Object.keys(r as object))))
  return (
    <div className="table-wrap" tabIndex={0} role="region" aria-label={t('table.scroll')}>
      <table className="table">
        <thead><tr>{cols.map((c) => <th key={c}>{c}</th>)}</tr></thead>
        <tbody>{rows.map((r, i) => <tr key={i}>{cols.map((c) => <td key={c}>{cell((r as Record<string, unknown>)[c])}</td>)}</tr>)}</tbody>
      </table>
    </div>
  )
}

function Bars({ data }: { data: Record<string, number> }) {
  const max = Math.max(1, ...Object.values(data))
  return (
    <div className="bars" role="list">
      {Object.entries(data).map(([k, v]) => (
        <div key={k} className="bar-row" role="listitem">
          <span className="bar-k">{k}</span>
          <span className="bar"><span style={{ width: `${(v / max) * 100}%` }} /></span>
          <span className="mono">{v}</span>
        </div>
      ))}
    </div>
  )
}

const tok = (w: UsageWindow): string => (w.calls_reporting_tokens > 0 ? `${w.input.toLocaleString()} / ${w.output.toLocaleString()}` : '–')

function UsageCard({ usage }: { usage: Record<string, AgentUsage> }) {
  const { t, lang } = useI18n()
  const name = useAgentName()
  const days = Array.from({ length: 14 }, (_, i) => new Date(Date.now() - (13 - i) * 86_400_000).toISOString().slice(0, 10))
  const max = Math.max(1, ...Object.values(usage).flatMap((u) => days.map((d) => (u.by_day[d]?.input ?? 0) + (u.by_day[d]?.output ?? 0))))
  return (
    <div className="card">
      <h3>{t('reports.usage')}</h3>
      <Notice tone="info">{t('reports.quota_note')}</Notice>
      <div className="table-wrap" tabIndex={0} role="region" aria-label={t('table.scroll')} style={{ marginTop: 12 }}>
        <table className="table">
          <thead>
            <tr>
              <th>{t('reports.group')}</th><th>{t('reports.usage.total')}</th><th>{t('reports.usage.5h')}</th><th>{t('reports.usage.24h')}</th><th>{t('reports.usage.7d')}</th><th>{t('reports.usage.quota')}</th>
            </tr>
          </thead>
          <tbody>
            {Object.entries(usage).map(([id, u]) => (
              <tr key={id}>
                <td><strong>{name(id)}</strong></td>
                <td>{t('reports.usage.calls', { n: u.total.calls })}<div className="hint">{tok(u.total)}{u.cached_input_tokens > 0 ? ` · ${t('reports.usage.cached', { n: u.cached_input_tokens.toLocaleString() })}` : ''}</div></td>
                <td>{t('reports.usage.calls', { n: u.last_5_hours.calls })}<div className="hint">{tok(u.last_5_hours)}</div></td>
                <td>{t('reports.usage.calls', { n: u.last_24_hours.calls })}<div className="hint">{tok(u.last_24_hours)}</div></td>
                <td>{t('reports.usage.calls', { n: u.last_7_days.calls })}<div className="hint">{tok(u.last_7_days)}</div></td>
                <td>{u.quota_errors > 0 ? <Pill tone="warn">{t('reports.usage.limit_hit', { n: u.quota_errors, when: u.last_quota_error_ms ? formatDateTime(u.last_quota_error_ms, lang) : '–' })}</Pill> : <span className="faint">{t('reports.usage.no_limit')}</span>}</td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>
      <p className="hint">{t('reports.usage.units')}</p>
      <h4 style={{ marginTop: 16 }}>{t('reports.usage.by_day')}</h4>
      <div className="daybars" role="list">
        {Object.entries(usage).map(([id, u]) => (
          <div key={id} className="daybar-row" role="listitem">
            <span className="bar-k">{name(id)}</span>
            <span className="daybars-cols" aria-label={t('reports.usage.by_day_for', { name: name(id) })}>
              {days.map((d) => {
                const w = u.by_day[d]
                const v = (w?.input ?? 0) + (w?.output ?? 0)
                return <span key={d} className="daybar" title={`${d}: ${w ? t('reports.usage.calls', { n: w.calls }) : '0'}${v ? ` · ${v.toLocaleString()} tokens` : ''}`}><span style={{ height: `${Math.max(v ? 8 : 0, (v / max) * 100)}%` }} /></span>
              })}
            </span>
          </div>
        ))}
      </div>
    </div>
  )
}

function AnalyticsView() {
  const { t } = useI18n()
  const [a, setA] = useState<Analytics | null>(null)
  const [err, setErr] = useState<string | null>(null)
  useEffect(() => {
    api.analytics().then(setA).catch((e: unknown) => setErr(e instanceof Error ? e.message : String(e)))
  }, [])
  if (err) return <Notice tone="danger">{err}</Notice>
  if (!a) return <Spinner label={t('loading')} />
  if (!a.history_enabled || a.consultations === 0) return <Notice tone="info">{t('reports.no_data')}</Notice>
  const group = (title: string, g?: Record<string, { consultations: number; median_duration_ms: number | null; median_calls: number | null; provider_failures: number }>) =>
    g && Object.keys(g).length > 0 ? (
      <div className="card">
        <h3>{title}</h3>
        <div className="table-wrap" tabIndex={0} role="region" aria-label={t('table.scroll')}>
          <table className="table">
            <thead><tr><th>{t('reports.group')}</th><th>{t('reports.consultations')}</th><th>{t('reports.median_duration')}</th><th>{t('reports.median_calls')}</th><th>{t('reports.failures')}</th></tr></thead>
            <tbody>
              {Object.entries(g).map(([k, v]) => (
                <tr key={k}><td>{k}</td><td>{v.consultations}</td><td>{formatDuration(v.median_duration_ms)}</td><td>{v.median_calls ?? '–'}</td><td>{v.provider_failures}</td></tr>
              ))}
            </tbody>
          </table>
        </div>
      </div>
    ) : null
  return (
    <div className="stack" style={{ gap: 16 }}>
      <Notice tone="info"><Pill tone="ok">{t('reports.kind_measured')}</Pill> {t('reports.note_measured')}</Notice>
      <div className="grid3">
        <div className="card"><div className="faint">{t('reports.consultations')}</div><div className="big">{a.consultations}</div></div>
        <div className="card"><h3>{t('reports.by_status')}</h3>{a.by_status && <Bars data={a.by_status} />}</div>
        <div className="card">
          <h3>{t('reports.tokens')}</h3>
          {a.tokens_reported_by_the_clis && a.tokens_reported_by_the_clis.calls_reporting > 0 ? (
            <p style={{ margin: 0 }}>{t('reports.tokens_value', { i: a.tokens_reported_by_the_clis.input, o: a.tokens_reported_by_the_clis.output, n: a.tokens_reported_by_the_clis.calls_reporting })}</p>
          ) : <p className="muted" style={{ margin: 0 }}>{t('d.tokens_none')}</p>}
          <p className="hint">{t('reports.tokens_note')}</p>
        </div>
      </div>
      {a.usage_by_agent && Object.keys(a.usage_by_agent).length > 0 && <UsageCard usage={a.usage_by_agent} />}
      {group(t('reports.by_strategy'), a.by_strategy)}
      {group(t('reports.by_mode'), a.by_mode)}
      {a.by_provider && Object.keys(a.by_provider).length > 0 && (
        <div className="card">
          <h3>{t('reports.by_provider')}</h3>
          <div className="table-wrap" tabIndex={0} role="region" aria-label={t('table.scroll')}>
            <table className="table">
              <thead><tr><th>{t('reports.group')}</th><th>{t('reports.calls')}</th><th>{t('reports.failed')}</th><th>{t('reports.timeouts')}</th><th>{t('reports.median_success')}</th></tr></thead>
              <tbody>
                {Object.entries(a.by_provider).map(([k, v]) => (
                  <tr key={k}><td>{k}</td><td>{v.recorded_calls}</td><td>{v.failed_calls}</td><td>{v.timeouts}</td><td>{formatDuration(v.median_success_ms)}</td></tr>
                ))}
              </tbody>
            </table>
          </div>
        </div>
      )}
      {a.memory_context && a.memory_context.consultations_with_memory > 0 && (
        <div className="card">
          <h3>{t('reports.memory')} <Pill tone="warn">{t('reports.kind_estimate')}</Pill></h3>
          <p>{t('reports.memory_value', { n: a.memory_context.consultations_with_memory, p: a.memory_context.median_reduction_vs_full_history_percent === null ? '–' : Math.round(a.memory_context.median_reduction_vs_full_history_percent) })}</p>
          <p className="hint">{t('reports.memory_note')}</p>
        </div>
      )}
    </div>
  )
}

const PROV_TONE: Record<string, 'neutral' | 'warn' | 'info' | 'ok'> = { fixture: 'neutral', mock: 'neutral', recorded_live: 'info', live: 'ok' }

function EvaluationsView() {
  const { t, lang } = useI18n()
  const [rows, setRows] = useState<ReportRow[] | null>(null)
  const [err, setErr] = useState<string | null>(null)
  const [open, setOpen] = useState<string | null>(null)
  const [detail, setDetail] = useState<Record<string, unknown> | null>(null)
  const [derr, setDerr] = useState<string | null>(null)
  useEffect(() => {
    api.reports().then((r) => setRows(r.items)).catch((e: unknown) => setErr(e instanceof Error ? e.message : String(e)))
  }, [])
  useEffect(() => {
    if (!open) return
    setDetail(null)
    setDerr(null)
    api.report(open).then(setDetail).catch((e: unknown) => setDerr(e instanceof Error ? e.message : String(e)))
  }, [open])
  if (err) return <Notice tone="danger">{err}</Notice>
  if (!rows) return <Spinner label={t('loading')} />
  return (
    <div className="stack" style={{ gap: 16 }}>
      <Notice tone="info">{t('reports.note_evals')}</Notice>
      {rows.length === 0 && <p className="muted">{t('reports.no_evals')}</p>}
      {rows.map((r) => (
        <div key={r.id} className="card">
          <div className="row wrap" style={{ gap: 8 }}>
            <strong>{r.dataset_title}</strong>
            <span className="faint">v{r.dataset_version}</span>
            {r.provenance.map((p) => <Pill key={p.class} tone={PROV_TONE[p.class] ?? 'neutral'} title={p.meaning}>{p.class}</Pill>)}
            {r.synthetic && <Pill tone="warn">{t('reports.synthetic')}</Pill>}
            <span className="faint">{formatDateTime(r.created_ms, lang)} · {t('reports.cases', { n: r.cases })} · {t('reports.judgments', { n: r.judgments })}</span>
            <span className="spacer" />
            <button type="button" className="btn sm" aria-expanded={open === r.id} onClick={() => setOpen(open === r.id ? null : r.id)}>{open === r.id ? t('collapse') : t('reports.view')}</button>
          </div>
          <p className="hint">{r.provenance.map((p) => p.meaning).join(' ')}</p>
          {open === r.id && (
            <div className="stack" style={{ marginTop: 8 }}>
              {derr && <Notice tone="danger">{derr}</Notice>}
              {!detail && !derr && <Spinner label={t('loading')} />}
              {detail && (
                <>
                  <Notice tone="warn">{t('reports.reading')}</Notice>
                  <h4>{t('reports.summaries')}</h4>
                  <JsonTable rows={detail.summaries} />
                  <h4>{t('reports.pairs')}</h4>
                  <JsonTable rows={detail.pairs} />
                  <h4>{t('reports.judgments_h')}</h4>
                  <JsonTable rows={detail.judgments} />
                  <h4>{t('reports.full_text')}</h4>
                  <pre className="plain-pre">{String(detail.text ?? '')}</pre>
                </>
              )}
            </div>
          )}
        </div>
      ))}
    </div>
  )
}

export function ReportsPage() {
  const { t } = useI18n()
  const [tab, setTab] = useState<Tab>('analytics')
  return (
    <div className="page">
      <div className="page-head">
        <div>
          <h1>{t('reports.title')}</h1>
          <p>{t('reports.subtitle')}</p>
        </div>
      </div>
      <Tabs label={t('reports.title')} value={tab} onChange={setTab} tabs={[{ id: 'analytics', label: t('reports.tab_analytics') }, { id: 'evaluations', label: t('reports.tab_evals') }]} />
      <TabPanel id={tab}>{tab === 'analytics' ? <AnalyticsView /> : <EvaluationsView />}</TabPanel>
    </div>
  )
}
