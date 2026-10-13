// Milestone 10: the animated stage, the conversation, characters, summary, headings and usage.
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { act, render, screen, waitFor, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'

const m = vi.hoisted(() => ({}) as Record<string, ReturnType<typeof import('vitest').vi.fn>>)

vi.mock('../src/api/client', async (orig) => {
  const real = await orig<typeof import('../src/api/client')>()
  const names = ['getUi', 'putUi', 'characters', 'putCharacters', 'summarize', 'analytics', 'reports', 'capabilities', 'theater', 'one', 'research', 'getSettings']
  const api: Record<string, unknown> = {}
  for (const n of names) {
    m[n] = vi.fn()
    api[n] = m[n]
  }
  return { ...real, api }
})

import { ApiError } from '../src/api/client'
import type { AgentUsage, CharactersFile } from '../src/api/types'
import { Bubble, Conversation, type TMsg } from '../src/components/Bubble'
import { PixelRobot } from '../src/components/PixelRobot'
import { Stage, activityOfView, recentSpeech, spotFor } from '../src/components/Stage'
import { SummaryBox } from '../src/components/SummaryBox'
import { renderMarkdown } from '../src/lib/markdown'
import { ES_HEADINGS } from '../src/lib/headings'
import { ReportsPage } from '../src/pages/Reports'
import { SettingsPage } from '../src/pages/Settings'
import { reduceEvents } from '../src/state/job'
import { caps, consultation, ev, resetSeq } from './fixtures'
import { FakeEventSource, Providers, renderWithApp } from './helpers'

const FILE: CharactersFile = { reply_language: 'auto', conversational: true, characters: {} }
const LIMITS = { name_chars: 24, personality_chars: 600, palettes: 3, accessories: ['none', 'glasses', 'cap', 'headphones'] }

beforeEach(() => {
  for (const f of Object.values(m)) f.mockReset()
  m.capabilities?.mockResolvedValue(caps)
  m.getUi?.mockResolvedValue({ preferences: { theme: 'dark', language: 'es', reduced_motion: 'system', theater_default: false, show_stage: true } })
  m.putUi?.mockImplementation(async (p: unknown) => ({ preferences: p }))
  m.characters?.mockResolvedValue({ settings: FILE, limits: LIMITS, note: 'n', writable: true })
  m.putCharacters?.mockImplementation(async (c: unknown) => ({ settings: c }))
  m.getSettings?.mockResolvedValue({ defaults: {}, path_known: true, note: '' })
  m.analytics?.mockResolvedValue({ history_enabled: false, consultations: 0 })
  m.reports?.mockResolvedValue({ items: [], note: '' })
})
afterEach(() => vi.unstubAllGlobals())

describe('where the characters stand', () => {
  it('each stage of the work puts them somewhere different, from recorded facts only', () => {
    const at = (a: Parameters<typeof spotFor>[2], i: number, coord = false) => spotFor(i, 3, a, coord, `a${i}`, 0, false)
    const desks = [0, 1, 2].map((i) => at('independent_analysis', i))
    expect(new Set(desks.map((d) => d.x)).size).toBe(3)
    expect(new Set(desks.map((d) => d.y)).size).toBe(1)
    const table = [0, 1, 2].map((i) => at('review', i))
    expect(table.map((t) => t.x)).toEqual([34, 50, 66]) // gathered around the middle
    expect(at('cross_review', 1)).toEqual(table[1])
    expect(at('synthesis', 0, true).x).toBe(50) // the coordinator takes the front
    expect(at('synthesis', 0, true).y).toBeGreaterThan(at('synthesis', 1).y)
    expect(at('synthesis', 1)).toEqual(desks[1]) // the others stay at their desks
    expect(at('done', 0).y).toBeGreaterThan(desks[0]?.y ?? 0) // everybody lines up at the front
  })

  it('wandering is deterministic, stays on the floor and only happens when allowed', () => {
    const a = spotFor(0, 3, 'idle', false, 'claude', 4, true)
    expect(spotFor(0, 3, 'idle', false, 'claude', 4, true)).toEqual(a)
    expect(spotFor(0, 3, 'idle', false, 'claude', 5, true)).not.toEqual(a)
    for (let t = 0; t < 40; t++) {
      const s = spotFor(1, 3, 'planning', false, 'codex', t, true)
      expect(s.x).toBeGreaterThanOrEqual(14)
      expect(s.x).toBeLessThanOrEqual(86)
      expect(s.y).toBeGreaterThanOrEqual(70)
      expect(s.y).toBeLessThanOrEqual(92)
    }
    expect(spotFor(0, 3, 'idle', false, 'claude', 4, false)).toEqual(spotFor(0, 3, 'independent_analysis', false, 'claude', 0, false)) // reduced motion: they stay at their desks
  })
})

describe('the stage', () => {
  const cast = [{ id: 'claude', state: 'speaking' as const, coordinator: true }, { id: 'codex', state: 'thinking' as const }, { id: 'google', state: 'waiting' as const }]

  it('is decoration: hidden from assistive technology, with every agent named below its character', () => {
    const { container } = renderWithApp(<Stage cast={cast} speech={[]} activity="independent_analysis" />)
    const stage = container.querySelector('.stage')
    expect(stage).toHaveAttribute('aria-hidden', 'true')
    expect(Array.from(stage?.querySelectorAll('.stage-name') ?? []).map((e) => e.textContent)).toEqual(['Claude', 'Codex', 'Google'])
    expect(stage?.querySelectorAll('.stage-bubble')).toHaveLength(0) // nobody has said anything, so nobody has a bubble
  })

  it('a bubble holds the real recorded text of the agent that wrote it, and nobody else gets one', () => {
    const { container } = renderWithApp(<Stage cast={cast} speech={[{ key: 'm1', agent: 'codex', text: '## Answer\n\nBuy **the entry model** [1].' }]} activity="review" />)
    const bubbles = container.querySelectorAll('.stage-bubble')
    expect(bubbles).toHaveLength(1)
    expect(bubbles[0]?.closest('.stage-char')).toHaveAttribute('data-agent', 'codex')
    expect(bubbles[0]?.textContent).toBe('Answer Buy the entry model [1].')
  })

  it('a failed call shows a fixed status, never invented words or the raw error', () => {
    const { container } = renderWithApp(<Stage cast={cast} speech={[{ key: 'f1', agent: 'google', text: 'secret internal error', failed: true }]} activity="independent_analysis" />)
    const b = container.querySelector('[data-agent="google"] .stage-bubble')
    expect(b?.textContent).toBe('No pudo responder')
    expect(b?.className).toContain('failed')
  })

  it('uses the names and the look the user chose', async () => {
    m.characters?.mockResolvedValue({
      settings: { ...FILE, characters: { claude: { name: 'Ámbar', personality: '', appearance: { palette: 2, accessory: 'cap' } } } }, limits: LIMITS, note: '', writable: true,
    })
    const { container } = renderWithApp(<Stage cast={cast} speech={[]} activity="done" />)
    await waitFor(() => expect(container.querySelector('[data-agent="claude"] .stage-name')?.textContent).toBe('Ámbar'))
    const robot = container.querySelector('[data-agent="claude"] svg')
    expect(robot?.getAttribute('class')).toContain('pal-2')
    expect(robot?.querySelector('.px-cap')).not.toBeNull()
  })

  it('characters walk when their place changes, and not with reduced motion', async () => {
    const walkers = (c: HTMLElement) => c.querySelectorAll('.stage-char[data-moving]').length
    const { container, rerender } = renderWithApp(<Stage cast={cast} speech={[]} activity="independent_analysis" />)
    rerender(<Providers><Stage cast={cast} speech={[]} activity="review" /></Providers>)
    await waitFor(() => expect(walkers(container)).toBe(3))
    expect(container.querySelector('.stage-char[data-moving] svg')?.getAttribute('class')).toContain('walking')
    // The same change with reduced motion: they are simply there.
    m.getUi?.mockResolvedValue({ preferences: { theme: 'dark', language: 'es', reduced_motion: 'on', theater_default: false, show_stage: true } })
    const calm = renderWithApp(<Stage cast={cast} speech={[]} activity="independent_analysis" />)
    await act(async () => { await Promise.resolve() })
    calm.rerender(<Providers><Stage cast={cast} speech={[]} activity="review" /></Providers>)
    await act(async () => { await new Promise((r) => setTimeout(r, 30)) })
    expect(walkers(calm.container)).toBe(0)
  })
})

describe('the curtains', () => {
  const cast = [{ id: 'claude', state: 'idle' as const }, { id: 'codex', state: 'idle' as const }]
  const curtains = (a: Parameters<typeof Stage>[0]['activity'], extra = {}) => renderWithApp(<Stage cast={cast} speech={[]} activity={a} {...extra} />).container.querySelector('.stage')?.getAttribute('data-curtains')

  it('open while the agents really work and talk, closed before and after', () => {
    expect(curtains('idle')).toBe('closed')
    expect(curtains('planning')).toBe('closed')
    expect(curtains('research')).toBe('closed')
    for (const a of ['independent_analysis', 'review', 'cross_review', 'synthesis'] as const) expect(curtains(a), a).toBe('open')
    expect(curtains('done')).toBe('closed')
  })

  it('a saved record can be shown with the curtains open, and a bubble never shows behind a closed curtain', () => {
    expect(curtains('done', { curtains: 'open' })).toBe('open')
    const { container } = renderWithApp(<Stage cast={cast} speech={[{ key: 'm', agent: 'claude', text: 'hola' }]} activity="done" />)
    expect(container.querySelectorAll('.stage-bubble')).toHaveLength(0)
    expect(container.querySelectorAll('.stage-curtain')).toHaveLength(2)
  })

  it('is a theatre: a platform, an arch and footlights are drawn, and the light follows the last speaker', () => {
    const { container } = renderWithApp(<Stage cast={cast} speech={[{ key: 'm', agent: 'codex', text: 'hola' }]} activity="review" />)
    expect(container.querySelector('.stage-backdrop')?.innerHTML).toContain('var(--stage-wood-a)')
    expect(container.querySelector('.stage-backdrop')?.innerHTML).toContain('var(--stage-bulb)')
    expect(container.querySelector('.stage-backdrop')?.innerHTML).toContain('var(--stage-gold)')
    expect(container.querySelector('.stage-spot')).not.toBeNull()
  })
})

describe('what the stage reads from a live consultation', () => {
  it('shows the stage that is really running and the last two recorded things said', () => {
    resetSeq()
    const view = reduceEvents([
      ev('consultation_created', { consultation_id: 'c', mode: 'decision', coordinator: 'claude', participants: ['codex'], skipped: [], rounds: 1 }),
      ev('provider_started', { agent: 'codex', stage: 'independent_analysis', role: 'perspective' }),
      ev('agent_message_recorded', { agent: 'codex', stage: 'independent_analysis', role: 'perspective', technical: true, text: 'search plan', duration_ms: 1 }),
      ev('provider_completed', { agent: 'codex', stage: 'independent_analysis', role: 'perspective', ok: true, duration_ms: 1 }),
      ev('agent_message_recorded', { agent: 'codex', stage: 'independent_analysis', role: 'perspective', technical: false, text: 'one', duration_ms: 1 }),
      ev('provider_started', { agent: 'claude', stage: 'review', role: 'review' }),
    ])
    expect(activityOfView(view)).toBe('review')
    const speech = recentSpeech(view, (id) => id)
    expect(speech.map((s) => s.text)).toEqual(['one']) // the technical message is not dialogue
    resetSeq()
    const done = reduceEvents([ev('consultation_completed', { status: 'completed', consultation_id: 'c', calls: 1, duration_ms: 1, warnings: [] })])
    expect(activityOfView(done)).toBe('done')
    resetSeq()
    expect(activityOfView(reduceEvents([ev('consultation_cancelled', { status: 'cancelled', error: { kind: 'cancelled', message: 'x' } })]))).toBe('idle')
  })
})

describe('the robots look', () => {
  it('accessories, palettes and the walk cycle are drawn as pixels, not images', () => {
    const { container, rerender } = render(<PixelRobot kind="claude" accessory="glasses" palette={1} />)
    expect(container.querySelectorAll('.px-acc').length).toBeGreaterThan(3)
    expect(container.querySelector('svg')?.getAttribute('class')).toContain('pal-1')
    rerender(<PixelRobot kind="codex" accessory="headphones" />)
    expect(container.querySelectorAll('.px-acc').length).toBe(3)
    rerender(<PixelRobot kind="google" accessory="cap" walking flip />)
    expect(container.querySelectorAll('.px-cap').length).toBe(3)
    const cls = container.querySelector('svg')?.getAttribute('class') ?? ''
    expect(cls).toContain('walking')
    expect(cls).toContain('flip')
    expect(container.querySelectorAll('.legs').length).toBe(2) // two leg poses to alternate
    expect(container.querySelector('image, img')).toBeNull()
    rerender(<PixelRobot kind="google" accessory="none" />)
    expect(container.querySelectorAll('.px-acc, .px-cap').length).toBe(0)
  })
})

describe('conversation bubbles', () => {
  const base = { key: 'k', agent: 'codex', round: 0, technical: false, durationMs: 900, startMs: 0, endMs: 1000, offsetMs: 1000 }
  const msgs: TMsg[] = [
    { ...base, key: 'a', role: 'perspective', stage: 'independent_analysis', ok: true, text: 'Opinion A', error: null, usage: { input_tokens: 12_345, output_tokens: 80 } },
    { ...base, key: 'b', agent: 'google', role: 'perspective', stage: 'independent_analysis', ok: false, text: null, error: 'timed out' },
    { ...base, key: 'c', agent: 'claude', role: 'cross_review', stage: 'cross_review', ok: true, text: 'I disagree with Codex', error: null },
    { ...base, key: 'd', agent: 'claude', role: 'synthesis', stage: 'synthesis', ok: true, text: '## Answer\n\nFinal', error: null },
  ]

  it('groups what was said by what the group was doing, in the voice of a conversation', () => {
    renderWithApp(<Conversation msgs={msgs} known={new Set()} />)
    expect(screen.getAllByRole('heading', { level: 3 }).map((h) => h.textContent)).toEqual(['Cada uno pensó por su cuenta', 'Se respondieron entre ellos', 'El coordinador redactó el informe'])
    expect(screen.getAllByRole('article')).toHaveLength(4)
  })

  it('shows tokens only when the CLI reported them, and a failure without words', () => {
    renderWithApp(<Conversation msgs={msgs} known={new Set()} />)
    expect(screen.getByText(/12\.3k in \/ 80 out/)).toBeInTheDocument()
    const failed = screen.getAllByRole('article').find((a) => a.className.includes('bubble-fail')) as HTMLElement
    expect(within(failed).getByRole('alert')).toHaveTextContent('timed out')
    expect(screen.getAllByText(/ in \/ /)).toHaveLength(1)
  })

  it('shows the character name with the real CLI beside it', async () => {
    m.characters?.mockResolvedValue({ settings: { ...FILE, characters: { codex: { name: 'Circuito', personality: '', appearance: { palette: 0, accessory: 'none' } } } }, limits: LIMITS, note: '', writable: true })
    renderWithApp(<Bubble m={msgs[0] as TMsg} avatarState="finished" known={new Set()} n={0} />)
    await waitFor(() => expect(screen.getByText('Circuito')).toBeInTheDocument())
    expect(screen.getByText('OpenAI Codex CLI')).toBeInTheDocument()
  })
})

describe('headings', () => {
  it('known English section headings are shown in Spanish, the rest is untouched', () => {
    const html = (opts = {}) => render(<div>{renderMarkdown('## Answer\n\n## Significant disagreements:\n\n## My own heading\n\n```\n## Answer\n```', opts)}</div>).container
    const es = html({ headings: ES_HEADINGS })
    expect(Array.from(es.querySelectorAll('h2')).map((h) => h.textContent)).toEqual(['Respuesta', 'Desacuerdos importantes', 'My own heading'])
    expect(es.querySelector('pre code')?.textContent).toBe('## Answer')
    const en = html()
    expect(en.querySelector('h2')?.textContent).toBe('Answer')
  })
})

describe('the progress line', () => {
  const show = () => document.querySelector('.show-line')?.textContent ?? ''

  const start = async () => {
    m.create = vi.fn()
    const { TurnView } = await import('../src/components/Turn')
    vi.stubGlobal('EventSource', FakeEventSource)
    FakeEventSource.instances = []
    renderWithApp(<TurnView turn={{ id: 't', question: 'q', jobId: 'job-1', startedMs: Date.now(), requestSettings: {} }} onFollowUp={() => undefined} />)
    await waitFor(() => expect(FakeEventSource.instances).toHaveLength(1))
    return FakeEventSource.instances[0] as FakeEventSource
  }

  it('says something of the theatre that matches the stage really running, and never "Working"', async () => {
    const es = await start()
    expect(show()).toMatch(/teatro|luces|telón|escenario/i) // planning
    resetSeq()
    act(() => es.emit('provider_started', ev('provider_started', { agent: 'claude', stage: 'independent_analysis', role: 'perspective' })))
    await waitFor(() => expect(show()).toMatch(/instrumentos|voz|parte|guion|atriles/i))
    act(() => es.emit('provider_started', ev('provider_started', { agent: 'claude', stage: 'synthesis', role: 'synthesis' })))
    await waitFor(() => expect(show()).toMatch(/director|final|traje|programa/i))
    expect(document.body.textContent).not.toMatch(/Trabajando|Working/)
  })

  it('keeps one stable, screen-reader friendly status while the decorative line changes', async () => {
    const es = await start()
    resetSeq()
    act(() => es.emit('provider_started', ev('provider_started', { agent: 'claude', stage: 'review', role: 'review' })))
    const sr = await screen.findByRole('heading', { level: 3, name: 'En curso: Revisión' })
    expect(document.querySelector('.show-line')).toHaveAttribute('aria-hidden', 'true')
    expect(sr).toBeInTheDocument()
  })

  it('rotates inside the stage over time, and stands still with reduced motion', async () => {
    m.getUi?.mockResolvedValue({ preferences: { theme: 'dark', language: 'es', reduced_motion: 'off', theater_default: false, show_stage: true } })
    vi.useFakeTimers({ shouldAdvanceTime: true })
    try {
      const es = await start()
      resetSeq()
      act(() => es.emit('provider_started', ev('provider_started', { agent: 'claude', stage: 'independent_analysis', role: 'perspective' })))
      const first = show()
      await act(async () => { vi.advanceTimersByTime(3700) })
      expect(show()).not.toBe(first)
      expect(show()).toMatch(/instrumentos|voz|parte|guion|atriles/i)
    } finally {
      vi.useRealTimers()
    }
  })
})

describe('the summary button', () => {
  const done = consultation()

  it('offers nothing for a consultation that produced no report', () => {
    const { container } = renderWithApp(<SummaryBox c={consultation({ status: 'failed', final_answer: '' })} />)
    expect(container.querySelector('.summary-box')).toBeNull()
    const cancelled = renderWithApp(<SummaryBox c={consultation({ status: 'cancelled' })} />)
    expect(cancelled.container.querySelector('.summary-box')).toBeNull()
  })

  it('does nothing until pressed, then makes exactly one request and labels the result as written by a model', async () => {
    let finish: (v: unknown) => void = () => undefined
    m.summarize?.mockImplementation(() => new Promise((r) => { finish = r }))
    renderWithApp(<SummaryBox c={done} />)
    expect(m.summarize).not.toHaveBeenCalled()
    expect(screen.getByText(/solo ocurre si lo pides/)).toBeInTheDocument()
    await userEvent.click(screen.getByRole('button', { name: 'Resumir el reporte' }))
    expect(m.summarize).toHaveBeenCalledTimes(1)
    expect(m.summarize?.mock.calls[0]?.[0]).toBe('c-1')
    expect(screen.queryByRole('button', { name: 'Resumir el reporte' })).toBeNull() // no double click
    expect(screen.getByText(/está escribiendo el resumen/)).toBeInTheDocument()
    await act(async () => finish({ summary: { id: 's1', agent: 'claude', text: '**Bottom line**: buy B.', created_ms: 1, duration_ms: 5, origin: 'ai' }, note: 'n' }))
    expect(await screen.findByText('Resumen escrito por Claude Code')).toBeInTheDocument()
    expect(screen.getByText('Bottom line').tagName).toBe('STRONG')
    expect(screen.getByText(/Condensa, no verifica/)).toBeInTheDocument()
    expect(screen.getByRole('button', { name: 'Resumir de nuevo' })).toBeInTheDocument()
  })

  it('can be cancelled and then says nothing went wrong', async () => {
    m.summarize?.mockImplementation((_id: string, _a: string | undefined, signal: AbortSignal) => new Promise((_ok, fail) => signal.addEventListener('abort', () => fail(new DOMException('aborted', 'AbortError')))))
    renderWithApp(<SummaryBox c={done} />)
    await userEvent.click(screen.getByRole('button', { name: 'Resumir el reporte' }))
    await userEvent.click(screen.getByRole('button', { name: 'Cancelar' }))
    expect(await screen.findByRole('button', { name: 'Resumir el reporte' })).toBeInTheDocument()
    expect(screen.queryByRole('alert')).toBeNull()
  })

  it('shows the server’s reason when it cannot be written', async () => {
    m.summarize?.mockRejectedValue(new ApiError(409, 'conflict', 'a summary of this consultation is already being written'))
    renderWithApp(<SummaryBox c={done} />)
    await userEvent.click(screen.getByRole('button', { name: 'Resumir el reporte' }))
    expect(await screen.findByRole('alert')).toHaveTextContent('already being written')
  })

  it('lets a participant write it, and keeps earlier summaries', async () => {
    m.summarize?.mockResolvedValue({ summary: { id: 's2', agent: 'codex', text: 'Second', created_ms: 2, duration_ms: 1, origin: 'ai' }, note: '' })
    renderWithApp(<SummaryBox c={consultation({ summaries: [{ id: 's1', agent: 'claude', text: 'First', created_ms: 1, duration_ms: 1, origin: 'ai' }] })} />)
    expect(screen.getByText('First')).toBeInTheDocument()
    await userEvent.selectOptions(screen.getByLabelText('Quién lo escribe'), 'codex')
    await userEvent.click(screen.getByRole('button', { name: 'Resumir de nuevo' }))
    await waitFor(() => expect(m.summarize?.mock.calls[0]?.[1]).toBe('codex'))
    expect(await screen.findByText('Second')).toBeInTheDocument()
    expect(screen.getByText('Resúmenes anteriores (1)')).toBeInTheDocument()
  })
})

describe('characters settings', () => {
  const open = async () => {
    renderWithApp(<SettingsPage />)
    await userEvent.click(await screen.findByRole('tab', { name: 'Personajes' }))
    return await screen.findByRole('region', { name: 'Claude Code' })
  }

  it('says plainly what a personality is and is not', async () => {
    await open()
    expect(screen.getByText(/no dan conciencia ni mente a un modelo/)).toBeInTheDocument()
    expect(screen.getByText(/siguen en solo lectura/)).toBeInTheDocument()
    expect(screen.getByText(/ningún agente recibe el nombre de los demás hasta la revisión/)).toBeInTheDocument()
  })

  it('edits a name, a profile and a look and saves exactly that', async () => {
    const card = await open()
    expect(screen.getByRole('button', { name: 'Guardar personajes' })).toBeDisabled()
    await userEvent.type(within(card).getByLabelText('Nombre'), 'Ámbar')
    expect(within(card).getByRole('heading', { name: 'Ámbar' })).toBeInTheDocument()
    await userEvent.click(within(card).getByRole('button', { name: 'Escéptico' }))
    expect((within(card).getByLabelText('Personalidad') as HTMLTextAreaElement).value).toMatch(/Escéptico y cuidadoso/)
    await userEvent.click(within(card).getByLabelText('Colores 3'))
    await userEvent.click(within(card).getByLabelText('Lentes'))
    await userEvent.selectOptions(screen.getByLabelText('Idioma de las respuestas'), 'es')
    await userEvent.click(screen.getByRole('checkbox', { name: /Que se hablen entre ellos/ }))
    await userEvent.click(screen.getByRole('button', { name: 'Guardar personajes' }))
    await waitFor(() => expect(m.putCharacters).toHaveBeenCalledTimes(1))
    const sent = m.putCharacters?.mock.calls[0]?.[0] as CharactersFile
    expect(sent.reply_language).toBe('es')
    expect(sent.conversational).toBe(false)
    expect(Object.keys(sent.characters)).toEqual(['claude']) // only the agent that was edited
    expect(sent.characters.claude).toMatchObject({ name: 'Ámbar', appearance: { palette: 2, accessory: 'glasses' } })
    expect(await screen.findByText('Personajes guardados.')).toBeInTheDocument()
  })

  it('respects the length limits of the server and can reset a character', async () => {
    const card = await open()
    expect(within(card).getByLabelText('Nombre')).toHaveAttribute('maxlength', '24')
    expect(within(card).getByLabelText('Personalidad')).toHaveAttribute('maxlength', '600')
    await userEvent.type(within(card).getByLabelText('Nombre'), 'X')
    await userEvent.click(within(card).getByRole('button', { name: 'Restablecer' }))
    expect(within(card).getByLabelText('Nombre')).toHaveValue('')
    expect(screen.getByRole('button', { name: 'Guardar personajes' })).toBeDisabled() // back to what is saved
  })

  it('shows the server’s refusal', async () => {
    m.putCharacters?.mockRejectedValue(new ApiError(422, 'validation', 'a character name has at most 24 characters'))
    const card = await open()
    await userEvent.type(within(card).getByLabelText('Nombre'), 'Z')
    await userEvent.click(screen.getByRole('button', { name: 'Guardar personajes' }))
    expect(await screen.findByRole('status')).toHaveTextContent('at most 24 characters')
  })

  it('the stage can be turned off from the appearance tab', async () => {
    renderWithApp(<SettingsPage />)
    await userEvent.click(await screen.findByRole('tab', { name: 'Apariencia' }))
    await userEvent.click(screen.getByLabelText(/Mostrar el teatro animado sobre el chat/))
    await waitFor(() => expect(m.putUi?.mock.calls.at(-1)?.[0]).toMatchObject({ show_stage: false }))
  })
})

describe('usage report', () => {
  const win = (calls: number, input = 0, output = 0) => ({ calls, calls_reporting_tokens: input || output ? calls : 0, input, output })
  const usage: Record<string, AgentUsage> = {
    claude: { total: win(5, 12_000, 3_000), cached_input_tokens: 400, last_5_hours: win(2, 1_000, 200), last_24_hours: win(3, 5_000, 900), last_7_days: win(5, 12_000, 3_000), by_day: { [new Date().toISOString().slice(0, 10)]: win(2, 1_000, 200) }, quota_errors: 0, last_quota_error_ms: null },
    google: { total: win(4), cached_input_tokens: 0, last_5_hours: win(1), last_24_hours: win(2), last_7_days: win(4), by_day: {}, quota_errors: 2, last_quota_error_ms: Date.now() },
  }

  it('shows what the CLIs reported, dashes where they report nothing, and never a remaining quota', async () => {
    m.analytics?.mockResolvedValue({ history_enabled: true, consultations: 3, by_status: { complete: 3 }, usage_by_agent: usage, quota: { remaining_known: false, note: 'x' }, tokens_reported_by_the_clis: { calls_reporting: 5, input: 12000, output: 3000, note: '' } })
    renderWithApp(<ReportsPage />)
    const card = (await screen.findByRole('heading', { name: 'Uso por agente' })).closest('.card') as HTMLElement
    expect(within(card).getByText(/Cuánta cuota te queda en cada plan/)).toBeInTheDocument()
    const rows = within(card).getAllByRole('row').slice(1)
    expect(rows).toHaveLength(2)
    expect(within(rows[0] as HTMLElement).getAllByText('12,000 / 3,000', { exact: false }).length).toBeGreaterThan(0)
    expect(within(rows[0] as HTMLElement).getByText(/400 en caché/)).toBeInTheDocument()
    expect(rows[1]?.textContent).toContain('–') // a CLI that reports nothing shows a dash, not zeros
    expect(rows[1]?.textContent).toMatch(/2 vez\/veces/)
    expect(rows[0]?.textContent).toContain('Ninguna registrada')
    expect(card.querySelectorAll('.daybar')).toHaveLength(28) // 14 days for each of the two agents
    expect(card.textContent).not.toMatch(/restante|remaining|quedan/i)
  })
})

