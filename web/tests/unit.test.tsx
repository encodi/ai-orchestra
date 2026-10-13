import { describe, expect, it, vi, beforeEach, afterEach } from 'vitest'
import { render } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { renderMarkdown } from '../src/lib/markdown'
import { agentKey, plainPreview, safeHttpUrl, formatDuration, formatOffset } from '../src/lib/format'
import { es } from '../src/i18n/es'
import { en } from '../src/i18n/en'
import { applyEvent, emptyView, reduceEvents } from '../src/state/job'
import { ApiError, followEvents, request } from '../src/api/client'
import { FakeEventSource } from './helpers'
import { ev, resetSeq, threeAgentRun } from './fixtures'

describe('markdown renderer', () => {
  const html = (src: string, opts = {}) => {
    const { container } = render(<div>{renderMarkdown(src, opts)}</div>)
    return container
  }

  it('renders headings, lists, tables and code', () => {
    const c = html('## Title\n\n- one\n- two\n\n| A | B |\n|---|---|\n| 1 | 2 |\n\n```\ncode here\n```')
    expect(c.querySelector('h2')?.textContent).toBe('Title')
    expect(c.querySelectorAll('li')).toHaveLength(2)
    expect(c.querySelectorAll('th')).toHaveLength(2)
    expect(c.querySelector('td')?.textContent).toBe('1')
    expect(c.querySelector('pre code')?.textContent).toBe('code here')
  })

  it('never turns agent text into HTML: tags are shown as text', () => {
    const c = html('<script>window.hacked = 1</script> <img src=x onerror="window.hacked=1"> **bold**')
    expect(c.querySelector('script')).toBeNull()
    expect(c.querySelector('img')).toBeNull()
    expect(c.textContent).toContain('<script>window.hacked = 1</script>')
    expect(c.querySelector('strong')?.textContent).toBe('bold')
    expect((window as unknown as { hacked?: number }).hacked).toBeUndefined()
  })

  it('links only to http(s) and opens them safely', () => {
    const c = html('[ok](https://example.com/a) [bad](javascript:alert(1)) [data](data:text/html,x)')
    const links = c.querySelectorAll('a')
    expect(links).toHaveLength(1)
    expect(links[0]?.getAttribute('href')).toBe('https://example.com/a')
    expect(links[0]?.getAttribute('rel')).toBe('noopener noreferrer')
    expect(links[0]?.getAttribute('target')).toBe('_blank')
    expect(c.textContent).toContain('[bad](javascript:alert(1))')
  })

  it('makes a citation clickable only when the source was retrieved', async () => {
    const onCite = vi.fn()
    const c = html('Claim [1] and another [7].', { knownSources: new Set([1]), onCite })
    const buttons = c.querySelectorAll('button.cite')
    expect(buttons).toHaveLength(1)
    await userEvent.click(buttons[0] as HTMLElement)
    expect(onCite).toHaveBeenCalledWith(1)
    expect(c.querySelector('.cite.plain')?.textContent).toBe('7')
  })
})

describe('formatting helpers', () => {
  it('only accepts http and https URLs', () => {
    expect(safeHttpUrl('https://a.example/x')).toBe('https://a.example/x')
    expect(safeHttpUrl('javascript:alert(1)')).toBeNull()
    expect(safeHttpUrl('file:///etc/passwd')).toBeNull()
    expect(safeHttpUrl('not a url')).toBeNull()
  })
  it('maps provider ids to a robot kind', () => {
    expect([agentKey('claude'), agentKey('codex'), agentKey('google'), agentKey('agy'), agentKey('lead')]).toEqual(['claude', 'codex', 'google', 'google', 'other'])
  })
  it('previews markdown as plain words', () => {
    expect(plainPreview('## Answer\n\n**bold** `x` | a | b |', 100)).toBe('Answer bold x a b')
    expect(plainPreview('x'.repeat(50), 10)).toHaveLength(10)
  })
  it('formats durations and offsets', () => {
    expect(formatDuration(450)).toBe('450 ms')
    expect(formatDuration(12_300)).toBe('12 s')
    expect(formatDuration(65_000)).toBe('1 min 5 s')
    expect(formatDuration(null)).toBe('–')
    expect(formatOffset(65_000)).toBe('01:05')
  })
})

describe('localization', () => {
  it('has the same keys in Spanish and English, with the same placeholders', () => {
    expect(Object.keys(en).sort()).toEqual(Object.keys(es).sort())
    const holes = (s: string) => (s.match(/\{\w+\}/g) ?? []).sort().join(',')
    for (const k of Object.keys(es) as (keyof typeof es)[]) {
      expect(holes(en[k]), `placeholders of ${k}`).toBe(holes(es[k]))
      expect(en[k].trim().length, `${k} is not empty`).toBeGreaterThan(0)
    }
  })
})

describe('the live job reducer', () => {
  beforeEach(() => resetSeq())

  it('derives progress only from recorded events', () => {
    const v = reduceEvents(threeAgentRun())
    expect(v.phase).toBe('running')
    expect(v.order.sort()).toEqual(['claude', 'codex', 'google'])
    expect(v.agents.codex?.state).toBe('finished')
    expect(v.agents.claude?.state).toBe('finished')
    expect(v.agents.google?.state).toBe('waiting')
    expect(v.messages.map((m) => m.text)).toEqual(['Codex says buy A.', 'Claude says buy B.'])
    expect(v.stages.independent_analysis).toBe('done')
    expect(v.stages.synthesis).toBe('pending')
  })

  it('ignores an event it already applied (a reconnect never duplicates)', () => {
    const events = threeAgentRun()
    const once = reduceEvents(events)
    const twice = events.reduce(applyEvent, once)
    expect(twice.messages).toHaveLength(once.messages.length)
    expect(twice.events).toHaveLength(events.length)
  })

  it('invents no message: one message per agent_message_recorded event', () => {
    const events = threeAgentRun()
    const v = reduceEvents(events)
    expect(v.messages).toHaveLength(events.filter((e) => e.kind === 'agent_message_recorded').length)
  })

  it('records a failed call as a failure with its own error and no reply', () => {
    resetSeq()
    const v = reduceEvents([
      ev('consultation_created', { consultation_id: 'c', mode: 'decision', coordinator: 'claude', participants: ['codex'], skipped: [], rounds: 1 }),
      ev('provider_started', { agent: 'codex', stage: 'independent_analysis', role: 'perspective' }),
      ev('provider_completed', { agent: 'codex', stage: 'independent_analysis', role: 'perspective', ok: false, error: 'quota used up', duration_ms: 50 }),
    ])
    expect(v.agents.codex?.state).toBe('error')
    expect(v.failures).toEqual([expect.objectContaining({ agent: 'codex', error: 'quota used up' })])
    expect(v.messages).toHaveLength(0)
  })

  it('never calls a partial or cancelled run complete', () => {
    const base = threeAgentRun()
    const partial = reduceEvents([...base, ev('consultation_completed', { status: 'partial', consultation_id: 'c-1', calls: 4, duration_ms: 10, warnings: [] })])
    expect(partial.phase).toBe('partial')
    resetSeq()
    const cancelled = reduceEvents([...threeAgentRun(), ev('consultation_cancelled', { status: 'cancelled', error: { kind: 'cancelled', message: 'stopped' } })])
    expect(cancelled.phase).toBe('cancelled')
    expect(Object.values(cancelled.agents).every((a) => a.state !== 'thinking')).toBe(true)
    expect(emptyView().phase).toBe('loading')
  })
})

describe('the API client', () => {
  const realFetch = globalThis.fetch
  afterEach(() => {
    globalThis.fetch = realFetch
    document.head.innerHTML = ''
  })

  it('sends the run token only with state-changing requests, as JSON', async () => {
    document.head.innerHTML = '<meta name="aio-token" content="tok123">'
    const calls: { url: string; init: RequestInit }[] = []
    globalThis.fetch = vi.fn(async (url: RequestInfo | URL, init?: RequestInit) => {
      calls.push({ url: String(url), init: init ?? {} })
      return new Response('{"ok":true}', { status: 200 })
    }) as typeof fetch
    await request('/health')
    await request('/settings', { method: 'PUT', body: { defaults: {} } })
    const get = calls[0]?.init.headers as Record<string, string>
    const put = calls[1]?.init.headers as Record<string, string>
    expect(get['X-AIO-Token']).toBeUndefined()
    expect(put['X-AIO-Token']).toBe('tok123')
    expect(put['Content-Type']).toBe('application/json')
    expect(calls[1]?.url).toBe('/api/v1/settings')
  })

  it('turns a structured error into an ApiError with its kind and field', async () => {
    globalThis.fetch = vi.fn(async () => new Response(JSON.stringify({ error: { kind: 'validation', message: 'bad rounds', field: 'settings.rounds' } }), { status: 422 })) as typeof fetch
    await expect(request('/consultations', { method: 'POST', body: {} })).rejects.toMatchObject({ kind: 'validation', field: 'settings.rounds', status: 422, message: 'bad rounds' })
  })

  it('reports an unreachable server in plain words', async () => {
    globalThis.fetch = vi.fn(async () => {
      throw new TypeError('failed')
    }) as typeof fetch
    const err = await request('/health').catch((e: unknown) => e)
    expect(err).toBeInstanceOf(ApiError)
    expect((err as ApiError).kind).toBe('network')
  })
})

describe('following events', () => {
  beforeEach(() => {
    FakeEventSource.instances = []
    vi.stubGlobal('EventSource', FakeEventSource)
  })
  afterEach(() => vi.unstubAllGlobals())

  it('drops events it already saw after a reconnect and closes after the final one', () => {
    const seen: number[] = []
    const states: string[] = []
    followEvents('job-1', (e) => seen.push(e.seq), (s) => states.push(s))
    const es = FakeEventSource.instances[0] as FakeEventSource
    es.emit('provider_started', { seq: 0, ts_ms: 1, kind: 'provider_started', data: {} })
    es.emit('provider_started', { seq: 1, ts_ms: 2, kind: 'provider_started', data: {} })
    es.emit('provider_started', { seq: 1, ts_ms: 2, kind: 'provider_started', data: {} }) // the server replays after a reconnect
    es.emit('provider_started', { seq: 0, ts_ms: 1, kind: 'provider_started', data: {} })
    es.emit('consultation_completed', { seq: 2, ts_ms: 3, kind: 'consultation_completed', data: {} })
    expect(seen).toEqual([0, 1, 2])
    expect(es.closed).toBe(true)
    expect(states.at(-1)).toBe('closed')
  })

  it('asks the server to resume after a given event', () => {
    followEvents('job-1', () => undefined, () => undefined, 7)
    expect(FakeEventSource.instances[0]?.url).toBe('/api/v1/consultations/job-1/events?after=7')
  })
})
