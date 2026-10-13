// Sources and claims, exactly as AI Orchestra recorded them. Retrieved pages are evidence; anything an
// agent only reported is shown as a candidate and never as a source. Passages are slices of the extracted
// page text; the relation to a claim is a model's reading and is labelled as such.
import { useEffect, useRef } from 'react'
import type { Claim, ResearchResponse, Source } from '../api/types'
import { useI18n, type TKey } from '../i18n'
import { formatDateTime, safeHttpUrl } from '../lib/format'
import { Icon } from './Icons'
import { Collapsible, Notice, Pill } from './ui'

const CLAIM_TONE: Record<string, 'ok' | 'danger' | 'warn' | 'info' | 'neutral'> = {
  SUPPORTED: 'ok', CONTRADICTED: 'danger', MIXED_EVIDENCE: 'warn', INSUFFICIENT_EVIDENCE: 'info', NOT_CHECKED: 'neutral',
}
const CLAIM_KEY: Record<string, TKey> = {
  SUPPORTED: 'claim.supported', CONTRADICTED: 'claim.contradicted', MIXED_EVIDENCE: 'claim.mixed', INSUFFICIENT_EVIDENCE: 'claim.insufficient', NOT_CHECKED: 'claim.not_checked',
}
const REL_KEY: Record<string, TKey> = { supports: 'rel.supports', contradicts: 'rel.contradicts', unrelated: 'rel.unrelated', unclear: 'rel.unclear' }
const REL_TONE: Record<string, 'ok' | 'danger' | 'neutral' | 'warn'> = { supports: 'ok', contradicts: 'danger', unrelated: 'neutral', unclear: 'warn' }
const FRESH_DAYS = 30

function SourceCard({ s, focused, lang }: { s: Source; focused: boolean; lang: string }) {
  const { t } = useI18n()
  const ref = useRef<HTMLDivElement>(null)
  useEffect(() => {
    if (focused) ref.current?.scrollIntoView({ block: 'center', behavior: 'smooth' })
  }, [focused])
  const href = safeHttpUrl(s.display_url)
  const old = typeof s.age_days === 'number' && s.age_days > FRESH_DAYS
  return (
    <div ref={ref} id={`source-${s.id}`} className={`source-card${focused ? ' focused' : ''}`} tabIndex={-1}>
      <div className="row wrap" style={{ gap: 8 }}>
        <span className="cite plain" style={{ cursor: 'default' }}>{s.id}</span>
        <strong style={{ minWidth: 0, overflowWrap: 'anywhere' }}>{s.title || s.domain}</strong>
        {s.retrieved ? <Pill tone="ok">{t('src.retrieved')}</Pill> : <Pill tone="warn" title={s.provenance}>{t('src.not_retrieved')}</Pill>}
        {s.kind && <Pill>{s.kind}</Pill>}
      </div>
      <div className="muted" style={{ fontSize: 'var(--text-sm)', overflowWrap: 'anywhere' }}>
        {href ? (
          <a href={href} target="_blank" rel="noopener noreferrer">
            {s.domain} <Icon name="external" size={13} />
          </a>
        ) : (
          s.display_url
        )}
      </div>
      <div className="faint" style={{ fontSize: 'var(--text-xs)' }}>
        {s.published ? `${t('src.published')}: ${s.published} · ` : ''}
        {s.updated ? `${t('src.updated')}: ${s.updated} · ` : ''}
        {s.retrieved_at_ms ? `${t('src.retrieved_at')}: ${formatDateTime(s.retrieved_at_ms, lang)}` : ''}
        {typeof s.age_days === 'number' ? ` (${t('src.age', { n: s.age_days })})` : ''}
      </div>
      {old && <div style={{ color: 'var(--warn)', fontSize: 'var(--text-xs)' }}>{t('src.old', { n: FRESH_DAYS })}</div>}
      {!s.retrieved && (
        <div className="hint">
          {s.provenance}
          {s.error ? ` — ${s.error}` : ''}
        </div>
      )}
      {s.excerpts && s.excerpts.length > 0 && (
        <Collapsible title={t('src.passages', { n: s.excerpts.length })}>
          {s.excerpts.map((e, i) => (
            <blockquote key={i} className="passage">{e.text}</blockquote>
          ))}
          <div className="hint">{t('src.passage_note')}</div>
        </Collapsible>
      )}
      {s.limitations && s.limitations.length > 0 && <div className="hint">{s.limitations.join(' · ')}</div>}
    </div>
  )
}

function ClaimCard({ c, onCite }: { c: Claim; onCite: (id: number) => void }) {
  const { t } = useI18n()
  return (
    <div className="claim-card">
      <div className="row wrap" style={{ gap: 8, alignItems: 'flex-start' }}>
        <Pill tone={CLAIM_TONE[c.status] ?? 'neutral'}>{t(CLAIM_KEY[c.status] ?? 'claim.not_checked')}</Pill>
        <span style={{ flex: 1, minWidth: 200, overflowWrap: 'anywhere' }}>{c.text}</span>
      </div>
      <div className="hint">
        {t('claim.made_by')}: {c.originating_agents.join(', ') || '–'}
        {c.cited_by_agents && c.cited_by_agents.length > 0 ? ` · ${t('claim.agent_cited')}: ${c.cited_by_agents.map((n) => `[${n}]`).join(' ')}` : ''}
      </div>
      {c.evidence.length > 0 && (
        <Collapsible title={t('claim.evidence', { n: c.evidence.length })} meta={<span>{t('claim.independent', { s: c.supporting_independent_sources, c: c.contradicting_independent_sources })}</span>}>
          {c.evidence.map((e, i) => (
            <div key={i} className="evidence">
              <div className="row wrap" style={{ gap: 8 }}>
                <Pill tone={REL_TONE[e.relation] ?? 'neutral'}>{t(REL_KEY[e.relation] ?? 'rel.unclear')}</Pill>
                <button type="button" className="cite" onClick={() => onCite(e.source_id)} aria-label={t('src.go', { n: e.source_id })}>{e.source_id}</button>
                <span className="faint" style={{ fontSize: 'var(--text-xs)' }}>{t('claim.assessed_by', { a: e.assessed_by.join(', ') })}</span>
              </div>
              <blockquote className="passage">{e.passage}</blockquote>
              {e.reason && <div className="hint">{e.reason}</div>}
            </div>
          ))}
          <div className="hint">{t('claim.model_dependent')}</div>
        </Collapsible>
      )}
      {c.notes && c.notes.length > 0 && <div className="hint">{c.notes.join(' · ')}</div>}
    </div>
  )
}

export function ResearchView({ data, focus, onFocus }: { data: ResearchResponse; focus: number | null; onFocus: (id: number | null) => void }) {
  const { t, lang } = useI18n()
  if (!data.web_occurred) {
    return <Notice tone="info">{t('research.none')}</Notice>
  }
  const sources = data.sources ?? []
  const claims = data.claims ?? []
  const retrieved = sources.filter((s) => s.retrieved)
  const others = sources.filter((s) => !s.retrieved)
  const goTo = (id: number) => {
    onFocus(id)
    window.setTimeout(() => document.getElementById(`source-${id}`)?.focus({ preventScroll: true }), 50)
  }
  return (
    <div className="stack" style={{ gap: 20 }}>
      <div className="row wrap" style={{ gap: 8 }}>
        <Pill tone="ok">{t('research.retrieved_n', { n: retrieved.length })}</Pill>
        {others.length > 0 && <Pill tone="warn">{t('research.not_retrieved_n', { n: others.length })}</Pill>}
        <Pill>{t('research.claims_n', { n: claims.length })}</Pill>
        {data.depth && <Pill>{t('research.depth')}: {data.depth}</Pill>}
        {data.max_sources !== undefined && <Pill>{t('research.max_sources')}: {data.max_sources}</Pill>}
      </div>
      <Notice tone="info">
        {t('research.evidence_rule')}
      </Notice>
      <section>
        <h3>{t('research.sources')}</h3>
        <div className="stack">
          {retrieved.map((s) => <SourceCard key={s.id} s={s} focused={focus === s.id} lang={lang} />)}
          {others.length > 0 && (
            <Collapsible title={t('research.candidates', { n: others.length })}>
              <p className="hint">{t('research.candidates_note')}</p>
              <div className="stack">{others.map((s) => <SourceCard key={s.id} s={s} focused={focus === s.id} lang={lang} />)}</div>
            </Collapsible>
          )}
          {sources.length === 0 && <p className="muted">{t('research.no_sources')}</p>}
        </div>
      </section>
      <section>
        <h3>{t('research.claims')}</h3>
        <div className="stack">
          {claims.map((c) => <ClaimCard key={c.id} c={c} onCite={goTo} />)}
          {claims.length === 0 && <p className="muted">{t('research.no_claims')}</p>}
        </div>
      </section>
      {data.contradictions && data.contradictions.length > 0 && (
        <section>
          <h3>{t('research.contradictions')}</h3>
          <ul>
            {data.contradictions.map((c, i) => (
              <li key={i}>
                {t('research.contradiction', { id: c.claim_id, sup: c.supporting_sources.map((n) => `[${n}]`).join(' ') || '–', con: c.contradicting_sources.map((n) => `[${n}]`).join(' ') || '–' })}
              </li>
            ))}
          </ul>
        </section>
      )}
      {data.limitations && data.limitations.length > 0 && (
        <section>
          <h3>{t('research.limitations')}</h3>
          <ul>{data.limitations.map((l, i) => <li key={i}>{l}</li>)}</ul>
        </section>
      )}
      {((data.search_queries?.length ?? 0) > 0 || (data.subquestions?.length ?? 0) > 0) && (
        <Collapsible title={t('research.how')}>
          {data.discovery_agent && <p className="hint">{t('research.discovery_agent', { a: data.discovery_agent })}</p>}
          {data.subquestions && data.subquestions.length > 0 && <ul>{data.subquestions.map((q, i) => <li key={i}>{q}</li>)}</ul>}
          {data.search_queries && data.search_queries.length > 0 && <ul>{data.search_queries.map((q, i) => <li key={i} className="mono">{q}</li>)}</ul>}
        </Collapsible>
      )}
      <p className="hint">{t('research.freshness_note')}</p>
    </div>
  )
}
