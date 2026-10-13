import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import { ApiError, api } from '../api/client'
import type { DryRunResponse, SettingsObj } from '../api/types'
import { Icon } from '../components/Icons'
import { PixelRobot } from '../components/PixelRobot'
import { SettingsFields, hasAny } from '../components/SettingsPanel'
import { TurnView } from '../components/Turn'
import { Collapsible, Field, Notice, Pill, TabPanel, Tabs } from '../components/ui'
import { useI18n } from '../i18n'
import { useStore } from '../state/store'

function byteLength(s: string): number {
  return new TextEncoder().encode(s).length
}

export function ChatPage() {
  const { t } = useI18n()
  const { caps, capsError, turns, addTurn, sessionSettings, setSessionSettings, sessionId, newConversation } = useStore()
  const [question, setQuestion] = useState('')
  const [urlsText, setUrlsText] = useState('')
  const [remember, setRemember] = useState(true)
  const [advanced, setAdvanced] = useState(false)
  const [scope, setScope] = useState<'request' | 'session'>('request')
  const [requestSettings, setRequestSettings] = useState<SettingsObj>({})
  const [preview, setPreview] = useState<DryRunResponse | null>(null)
  const [busy, setBusy] = useState<'send' | 'plan' | null>(null)
  const [error, setError] = useState<{ message: string; field?: string } | null>(null)
  const inputRef = useRef<HTMLTextAreaElement>(null)
  const endRef = useRef<HTMLDivElement>(null)
  const [savedDefaults, setSavedDefaults] = useState<SettingsObj>({})

  useEffect(() => {
    api.getSettings().then((r) => setSavedDefaults(r.defaults)).catch(() => undefined)
  }, [])
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const tag = (e.target as HTMLElement | null)?.tagName
      if (e.key === '/' && tag !== 'INPUT' && tag !== 'TEXTAREA' && tag !== 'SELECT') {
        e.preventDefault()
        inputRef.current?.focus()
      }
    }
    document.addEventListener('keydown', onKey)
    return () => document.removeEventListener('keydown', onKey)
  }, [])

  const urls = useMemo(() => urlsText.split(/\s+/).map((u) => u.trim()).filter(Boolean), [urlsText])
  const maxBytes = caps?.limits.max_question_bytes ?? 100_000
  const tooLong = byteLength(question) > maxBytes
  const noAgents = caps ? !caps.providers.some((p) => p.usable) : false
  const memoryOff = (requestSettings.memory ?? sessionSettings.memory ?? savedDefaults.memory) === 'off'

  const buildRequest = useCallback(
    (dry: boolean) => ({
      question: question.trim(),
      session_settings: sessionSettings,
      settings: requestSettings,
      urls,
      session: memoryOff ? null : sessionId,
      new_session: !sessionId && !memoryOff,
      remember,
      dry_run: dry,
      explain_context: dry,
    }),
    [question, sessionSettings, requestSettings, urls, sessionId, memoryOff, remember],
  )

  const fail = (e: unknown) => {
    if (e instanceof ApiError) setError({ message: e.message, field: e.field })
    else setError({ message: e instanceof Error ? e.message : String(e) })
  }

  const send = async () => {
    if (!question.trim() || busy || tooLong) return
    setBusy('send')
    setError(null)
    setPreview(null)
    try {
      const r = await api.create(buildRequest(false))
      addTurn({ id: crypto.randomUUID(), question: question.trim(), jobId: r.job_id, startedMs: Date.now(), planText: r.plan_text, requestSettings })
      setQuestion('')
      setUrlsText('')
      setRequestSettings({}) // per-question settings apply to one question only
      setAdvanced(false)
      window.setTimeout(() => endRef.current?.scrollIntoView({ behavior: 'smooth', block: 'end' }), 60)
    } catch (e) {
      fail(e)
    } finally {
      setBusy(null)
    }
  }

  const plan = async () => {
    if (!question.trim() || busy) return
    setBusy('plan')
    setError(null)
    try {
      setPreview(await api.plan(buildRequest(true)))
    } catch (e) {
      setPreview(null)
      fail(e)
    } finally {
      setBusy(null)
    }
  }

  const onFollowUp = useCallback((text: string) => {
    setQuestion(text)
    inputRef.current?.focus()
  }, [])

  const examples = [t('ex.1'), t('ex.2'), t('ex.3')]
  const origin = () => scope

  return (
    <div className="chat-layout">
      <div className="page chat-scroll">
        <div className="page-head">
          <div>
            <h1>{t('chat.title')}</h1>
            <p>
              {sessionId ? <>{t('chat.session')}: <span className="mono">{sessionId}</span></> : t('chat.no_session')}
            </p>
          </div>
          <span className="spacer" />
          <button type="button" className="btn" onClick={() => { newConversation(); setPreview(null) }} disabled={turns.length === 0 && !sessionId}>
            <Icon name="plus" size={16} /> {t('chat.new')}
          </button>
        </div>

        {capsError && <Notice tone="danger">{t('chat.caps_error')}: {capsError}</Notice>}
        {noAgents && <Notice tone="warn">{t('chat.no_agents')}</Notice>}

        {turns.length === 0 && (
          <div className="hero card">
            <div className="hero-bots" aria-hidden="true">
              <PixelRobot kind="claude" size={72} decorative />
              <PixelRobot kind="codex" size={72} decorative />
              <PixelRobot kind="google" size={72} decorative />
            </div>
            <h2>{t('chat.hero_title')}</h2>
            <p className="muted" style={{ maxWidth: '58ch', margin: '0 auto' }}>{t('chat.hero_body')}</p>
            <div className="followups" style={{ justifyContent: 'center' }}>
              {examples.map((e) => (
                <button key={e} type="button" className="btn sm" onClick={() => onFollowUp(e)}>{e}</button>
              ))}
            </div>
            <p className="hint">{t('chat.no_calls_on_load')}</p>
          </div>
        )}

        <div className="turns">
          {turns.map((turn, ti) => (
            <div key={turn.id} className="turn">
              <div className="bubble-user">
                <Icon name="user" size={16} />
                <div style={{ overflowWrap: 'anywhere', whiteSpace: 'pre-wrap' }}>{turn.question}</div>
              </div>
              <TurnView turn={turn} onFollowUp={onFollowUp} isLatest={ti === turns.length - 1} />
            </div>
          ))}
          <div ref={endRef} />
        </div>
      </div>

      <form className="composer" onSubmit={(e) => { e.preventDefault(); void send() }} aria-label={t('composer.form')}>
        <div className="composer-inner">
          {error && (
            <Notice tone="danger" role="alert">
              {error.message}
              {error.field && <span className="hint"> ({error.field})</span>}
            </Notice>
          )}
          {preview && (
            <section className="card flat preview" aria-label={t('preview.title')}>
              <div className="row">
                <h3 style={{ margin: 0 }}>{t('preview.title')}</h3>
                <Pill tone="info">{t('preview.nothing_ran')}</Pill>
                <span className="spacer" />
                <button type="button" className="btn ghost sm" onClick={() => setPreview(null)}>{t('close')}</button>
              </div>
              <pre className="plain-pre">{preview.plan_text}</pre>
              {preview.context ? (
                <Collapsible title={t('preview.context')}>
                  <pre className="plain-pre">{JSON.stringify(preview.context, null, 2)}</pre>
                </Collapsible>
              ) : null}
              <p className="hint">{t('preview.note')}</p>
            </section>
          )}
          {advanced && caps && (
            <section className="card flat advanced" aria-label={t('adv.title')}>
              <Tabs
                label={t('adv.scope')}
                value={scope}
                onChange={setScope}
                tabs={[
                  { id: 'request', label: t('adv.request'), badge: hasAny(requestSettings) ? '●' : undefined },
                  { id: 'session', label: t('adv.session'), badge: hasAny(sessionSettings) ? '●' : undefined },
                ]}
              />
              <TabPanel id={scope}>
                <p className="hint" style={{ marginTop: 0 }}>{scope === 'request' ? t('adv.request_help') : t('adv.session_help')}</p>
                {hasAny(savedDefaults) && <p className="hint">{t('adv.saved_present')}</p>}
                <SettingsFields
                  caps={caps}
                  value={scope === 'request' ? requestSettings : sessionSettings}
                  onChange={scope === 'request' ? setRequestSettings : setSessionSettings}
                  originOf={origin}
                  idPrefix={`c-${scope}`}
                />
                <div className="row" style={{ marginTop: 12 }}>
                  <button
                    type="button"
                    className="btn sm"
                    onClick={() => (scope === 'request' ? setRequestSettings({}) : setSessionSettings({}))}
                    disabled={!hasAny(scope === 'request' ? requestSettings : sessionSettings)}
                  >
                    <Icon name="reset" size={14} /> {t('reset_all_auto')}
                  </button>
                  <label className="row" style={{ gap: 6 }}>
                    <input type="checkbox" checked={remember} onChange={(e) => setRemember(e.target.checked)} disabled={!caps.features.memory || memoryOff} />
                    <span>{t('adv.remember')}</span>
                  </label>
                </div>
                <Field label={t('adv.urls')} style={{ marginTop: 12 }}>
                  {(id) => (
                    <>
                      <textarea id={id} className="textarea" style={{ minHeight: 56 }} value={urlsText} onChange={(e) => setUrlsText(e.target.value)} placeholder="https://…" spellCheck={false} aria-describedby={`${id}-help`} />
                      <span id={`${id}-help`} className="hint">{t('adv.urls_help', { n: caps.limits.max_urls })}</span>
                    </>
                  )}
                </Field>
              </TabPanel>
            </section>
          )}
          <div className="composer-box">
            <label htmlFor="question" className="sr-only">{t('composer.label')}</label>
            <textarea
              id="question"
              ref={inputRef}
              className="textarea composer-input"
              placeholder={t('composer.placeholder')}
              value={question}
              onChange={(e) => setQuestion(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === 'Enter' && (e.metaKey || e.ctrlKey)) {
                  e.preventDefault()
                  void send()
                }
              }}
              rows={3}
              aria-describedby="composer-hint"
              aria-invalid={tooLong || undefined}
            />
            <div className="row wrap composer-actions">
              <button type="button" className="btn" aria-expanded={advanced} onClick={() => setAdvanced(!advanced)}>
                <Icon name="sliders" size={16} /> {t('adv.toggle')}
                {(hasAny(requestSettings) || hasAny(sessionSettings)) && <span className="dot" style={{ color: 'var(--accent)' }} aria-label={t('adv.has_overrides')} />}
              </button>
              <button type="button" className="btn" onClick={plan} disabled={!question.trim() || busy !== null} title={t('preview.hint')}>
                <Icon name="list" size={16} /> {busy === 'plan' ? t('loading') : t('preview.button')}
              </button>
              <span id="composer-hint" className="hint" style={{ flex: 1, minWidth: 140 }}>
                {tooLong ? <span style={{ color: 'var(--danger)' }}>{t('composer.too_long', { n: maxBytes })}</span> : t('composer.hint')}
              </span>
              <button type="submit" className="btn primary" disabled={!question.trim() || busy !== null || tooLong || noAgents}>
                {busy === 'send' ? t('composer.sending') : t('composer.send')} <Icon name="send" size={16} />
              </button>
            </div>
          </div>
        </div>
      </form>
    </div>
  )
}
