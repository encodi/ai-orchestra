import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { act, fireEvent, screen, waitFor, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'

const m = vi.hoisted(() => {
  const fns: Record<string, ReturnType<typeof import('vitest').vi.fn>> = {}
  return fns
})

vi.mock('../src/api/client', async (orig) => {
  const real = await orig<typeof import('../src/api/client')>()
  const names = [
    'health', 'capabilities', 'getSettings', 'putSettings', 'getUi', 'putUi', 'providers', 'create', 'plan', 'one', 'cancel', 'remove', 'theater', 'research',
    'feedback', 'history', 'sessions', 'session', 'clearSession', 'memory', 'memoryShow', 'memoryAdd', 'memoryCorrect', 'memoryForget', 'memoryExport',
    'contextPreview', 'reports', 'report', 'analytics', 'characters', 'putCharacters', 'summarize',
  ]
  const api: Record<string, unknown> = {}
  for (const n of names) {
    m[n] = vi.fn()
    api[n] = m[n]
  }
  return { ...real, api }
})

import App from '../src/App'
import { ChatPage } from '../src/pages/Chat'
import { HistoryPage } from '../src/pages/History'
import { MemoryPage } from '../src/pages/Memory'
import { ReportsPage } from '../src/pages/Reports'
import { SettingsPage } from '../src/pages/Settings'
import { TheaterPage } from '../src/pages/Theater'
import { ApiError } from '../src/api/client'
import { caps, consultation, ev, resetSeq, threeAgentRun, timeline } from './fixtures'
import { FakeEventSource, renderWithApp } from './helpers'

const flush = () => act(async () => { await Promise.resolve() })

beforeEach(() => {
  for (const f of Object.values(m)) f.mockReset()
  FakeEventSource.instances = []
  vi.stubGlobal('EventSource', FakeEventSource)
  sessionStorage.clear()
  window.location.hash = ''
  document.documentElement.removeAttribute('data-theme')
  m.capabilities?.mockResolvedValue(caps)
  m.getUi?.mockResolvedValue({ preferences: { theme: 'dark', language: 'es', reduced_motion: 'system', theater_default: false } })
  m.putUi?.mockImplementation(async (p: unknown) => ({ preferences: p }))
  m.getSettings?.mockResolvedValue({ defaults: {}, path_known: true, note: 'note' })
  m.putSettings?.mockImplementation(async (d: unknown) => ({ defaults: d }))
  m.history?.mockResolvedValue({ items: [], total: 0, running: [], history_enabled: true })
  m.theater?.mockResolvedValue(timeline())
  m.characters?.mockResolvedValue({ settings: { reply_language: 'auto', conversational: true, characters: {} }, limits: { name_chars: 24, personality_chars: 600, palettes: 3, accessories: ['none', 'glasses', 'cap', 'headphones'] }, note: 'n', writable: true })
  m.putCharacters?.mockImplementation(async (c: unknown) => ({ settings: c }))
})
afterEach(() => vi.unstubAllGlobals())

const jobHeader = (status = 'running') => ({ job_id: 'job-1', status, created_ms: 1, finished_ms: null, question_preview: 'Which one?', consultation_id: null, settings_in_force: {}, error: null, events: 0, events_dropped: 0 })

describe('chat', () => {
  it('opening the page starts nothing: no consultation, no plan, no agent', async () => {
    renderWithApp(<ChatPage />)
    await flush()
    expect(screen.getByText(/nunca llama a un agente/)).toBeInTheDocument()
    expect(m.create).not.toHaveBeenCalled()
    expect(m.plan).not.toHaveBeenCalled()
    expect(FakeEventSource.instances).toHaveLength(0)
  })

  it('does not send an empty question', async () => {
    renderWithApp(<ChatPage />)
    await flush()
    expect(screen.getByRole('button', { name: /^Enviar/ })).toBeDisabled()
    expect(screen.getByRole('button', { name: /Ver plan/ })).toBeDisabled()
  })

  it('sends the question with Auto settings, then follows the job through its events', async () => {
    m.create?.mockResolvedValue({ job_id: 'job-1', status: 'running', events_url: '', plan: {}, plan_text: 'Mode: decision', settings_in_force: {} })
    m.one?.mockResolvedValue({ kind: 'job', job: jobHeader() })
    renderWithApp(<ChatPage />)
    await flush()
    await userEvent.type(screen.getByLabelText('Tu pregunta'), '¿Cuál compro?')
    await userEvent.click(screen.getByRole('button', { name: /^Enviar/ }))
    await waitFor(() => expect(m.create).toHaveBeenCalledTimes(1))
    expect(m.create?.mock.calls[0]?.[0]).toMatchObject({ question: '¿Cuál compro?', settings: {}, session_settings: {}, urls: [], dry_run: false, new_session: true, remember: true })
    expect(await screen.findByText('¿Cuál compro?')).toBeInTheDocument()
    await waitFor(() => expect(FakeEventSource.instances).toHaveLength(1))
    const es = FakeEventSource.instances[0] as FakeEventSource
    expect(es.url).toBe('/api/v1/consultations/job-1/events')
    resetSeq()
    for (const e of threeAgentRun()) act(() => es.emit(e.kind, e))
    expect(await screen.findByText('Notas del motor', { exact: false }).catch(() => null)).toBeNull()
    expect(screen.getByRole('heading', { level: 3, name: /^En curso: / })).toBeInTheDocument()
    expect(screen.getAllByText(/Análisis independiente/).length).toBeGreaterThan(0)
    expect(screen.queryByText('Completa')).toBeNull()
  })

  it('keeps per-question settings, session settings and Auto apart', async () => {
    m.create?.mockResolvedValue({ job_id: 'job-1', status: 'running', events_url: '', plan: {}, plan_text: '', settings_in_force: {} })
    m.one?.mockResolvedValue({ kind: 'job', job: jobHeader() })
    renderWithApp(<ChatPage />)
    await flush()
    await userEvent.click(screen.getByRole('button', { name: /Ajustes de esta consulta/ }))
    await userEvent.selectOptions(screen.getByLabelText('Estrategia'), 'thorough')
    await userEvent.click(screen.getByRole('tab', { name: /Esta sesión del navegador/ }))
    fireEvent.change(screen.getByLabelText('Tiempo por agente (s)'), { target: { value: '90' } })
    await userEvent.type(screen.getByLabelText('Tu pregunta'), 'hola')
    await userEvent.keyboard('{Control>}{Enter}{/Control}')
    await waitFor(() => expect(m.create).toHaveBeenCalled())
    const body = m.create?.mock.calls[0]?.[0]
    expect(body.settings).toEqual({ strategy: 'thorough' })
    expect(body.session_settings).toEqual({ timeout: 90 })
    // The per-question choice was used once; the session choice stays.
    await userEvent.click(screen.getByRole('button', { name: /Ajustes de esta consulta/ }))
    await userEvent.click(screen.getByRole('tab', { name: /Solo esta pregunta/ }))
    expect(screen.getByLabelText('Estrategia')).toHaveValue('')
    await userEvent.click(screen.getByRole('tab', { name: /Esta sesión del navegador/ }))
    expect(screen.getByLabelText('Tiempo por agente (s)')).toHaveValue(90)
  })

  it('shows the server’s validation error and starts nothing', async () => {
    m.create?.mockRejectedValue(new ApiError(422, 'validation', 'rounds accepts: auto, 1, 2', 'settings.rounds'))
    renderWithApp(<ChatPage />)
    await flush()
    await userEvent.type(screen.getByLabelText('Tu pregunta'), 'x')
    await userEvent.click(screen.getByRole('button', { name: /^Enviar/ }))
    expect(await screen.findByRole('alert')).toHaveTextContent('rounds accepts: auto, 1, 2')
    expect(screen.getByRole('alert')).toHaveTextContent('settings.rounds')
    expect(FakeEventSource.instances).toHaveLength(0)
  })

  it('previews the plan without running anything', async () => {
    m.plan?.mockResolvedValue({ dry_run: true, plan: {}, plan_text: 'Mode: decision\nStrategy: balanced', context: null, settings_in_force: {}, note: 'Nothing was run: no agent was called and no page was fetched.' })
    renderWithApp(<ChatPage />)
    await flush()
    await userEvent.type(screen.getByLabelText('Tu pregunta'), 'x')
    await userEvent.click(screen.getByRole('button', { name: /Ver plan/ }))
    expect(await screen.findByText(/Strategy: balanced/)).toBeInTheDocument()
    expect(screen.getByText('No se ejecutó nada')).toBeInTheDocument()
    expect(m.create).not.toHaveBeenCalled()
  })

  it('warns when no agent is usable', async () => {
    m.capabilities?.mockResolvedValue({ ...caps, providers: caps.providers.map((p) => ({ ...p, usable: false })) })
    renderWithApp(<ChatPage />)
    expect(await screen.findByText(/Ningún agente está disponible/)).toBeInTheDocument()
  })

  it('cancels a running consultation and never shows it as complete', async () => {
    m.create?.mockResolvedValue({ job_id: 'job-1', status: 'running', events_url: '', plan: {}, plan_text: '', settings_in_force: {} })
    m.one?.mockResolvedValue({ kind: 'job', job: jobHeader() })
    m.cancel?.mockResolvedValue({ job_id: 'job-1', cancelling: true, note: '' })
    renderWithApp(<ChatPage />)
    await flush()
    await userEvent.type(screen.getByLabelText('Tu pregunta'), 'tarda')
    await userEvent.click(screen.getByRole('button', { name: /^Enviar/ }))
    await screen.findByRole('button', { name: /^Cancelar$/ })
    await userEvent.click(screen.getByRole('button', { name: /^Cancelar$/ }))
    expect(m.cancel).toHaveBeenCalledWith('job-1')
    const es = FakeEventSource.instances[0] as FakeEventSource
    resetSeq()
    act(() => es.emit('consultation_cancelled', ev('consultation_cancelled', { status: 'cancelled', error: { kind: 'cancelled', message: 'cancelled' } })))
    expect(await screen.findByText(/Cancelada\. Los agentes en ejecución se terminaron/)).toBeInTheDocument()
    expect(screen.queryByText('Completa')).toBeNull()
  })

  it('continues the session of the last answer in the next question', async () => {
    m.create?.mockResolvedValue({ job_id: 'job-1', status: 'running', events_url: '', plan: {}, plan_text: '', settings_in_force: {} })
    const done = consultation({ meta: { context: { session: 'ses-42' } } })
    m.one?.mockResolvedValue({ kind: 'job', job: jobHeader('completed'), consultation: done })
    renderWithApp(<ChatPage />)
    await flush()
    await userEvent.type(screen.getByLabelText('Tu pregunta'), 'primera')
    await userEvent.click(screen.getByRole('button', { name: /^Enviar/ }))
    await waitFor(() => expect(FakeEventSource.instances).toHaveLength(1))
    resetSeq()
    act(() => FakeEventSource.instances[0]?.emit('consultation_completed', ev('consultation_completed', { status: 'completed', consultation_id: 'c-1', calls: 4, duration_ms: 10, warnings: [] })))
    expect(await screen.findByText('Sesión:', { exact: false })).toBeInTheDocument()
    await waitFor(() => expect(screen.getByText('ses-42')).toBeInTheDocument())
    await userEvent.type(screen.getByLabelText('Tu pregunta'), 'segunda')
    await userEvent.click(screen.getByRole('button', { name: /^Enviar/ }))
    await waitFor(() => expect(m.create).toHaveBeenCalledTimes(2))
    expect(m.create?.mock.calls[1]?.[0]).toMatchObject({ session: 'ses-42', new_session: false })
  })

  it('renders the answer as Markdown with a clickable verified citation', async () => {
    m.create?.mockResolvedValue({ job_id: 'job-1', status: 'running', events_url: '', plan: {}, plan_text: '', settings_in_force: {} })
    m.one?.mockResolvedValue({ kind: 'job', job: jobHeader('completed'), consultation: consultation({ research: { sources: 1, retrieved: 1, claims: 1 } }) })
    m.research?.mockResolvedValue({ consultation_id: 'c-1', web_occurred: true, sources: [{ id: 1, original_url: 'https://a.example/', display_url: 'https://a.example/', domain: 'a.example', title: 'A', status: 'retrieved', retrieved: true, provenance: 'p', text_chars: 1, text_preview: '' }], claims: [] })
    renderWithApp(<ChatPage />)
    await flush()
    await userEvent.type(screen.getByLabelText('Tu pregunta'), 'x')
    await userEvent.click(screen.getByRole('button', { name: /^Enviar/ }))
    await waitFor(() => expect(FakeEventSource.instances).toHaveLength(1))
    resetSeq()
    act(() => FakeEventSource.instances[0]?.emit('consultation_completed', ev('consultation_completed', { status: 'completed', consultation_id: 'c-1', calls: 4, duration_ms: 10, warnings: [] })))
    await userEvent.click(await screen.findByRole('tab', { name: 'Respuesta' }))
    const cite = await screen.findByRole('button', { name: 'Source 1' })
    expect(screen.getByText('B').tagName).toBe('STRONG')
    await userEvent.click(cite)
    expect(await screen.findByText('1 fuente(s) descargada(s)')).toBeInTheDocument()
  })

  it('marks a partial result as partial and says what failed', async () => {
    m.create?.mockResolvedValue({ job_id: 'job-1', status: 'running', events_url: '', plan: {}, plan_text: '', settings_in_force: {} })
    m.one?.mockResolvedValue({ kind: 'job', job: jobHeader('partial'), consultation: consultation({ status: 'partial' }) })
    renderWithApp(<ChatPage />)
    await flush()
    await userEvent.type(screen.getByLabelText('Tu pregunta'), 'x')
    await userEvent.click(screen.getByRole('button', { name: /^Enviar/ }))
    await waitFor(() => expect(FakeEventSource.instances).toHaveLength(1))
    resetSeq()
    const es = FakeEventSource.instances[0] as FakeEventSource
    act(() => es.emit('provider_completed', ev('provider_completed', { agent: 'codex', stage: 'independent_analysis', role: 'perspective', ok: false, error: 'quota used up', duration_ms: 5 })))
    act(() => es.emit('consultation_completed', ev('consultation_completed', { status: 'partial', consultation_id: 'c-1', calls: 4, duration_ms: 10, warnings: [] })))
    expect(await screen.findByText(/Resultado parcial\./)).toBeInTheDocument()
    expect(screen.getByText('Parcial')).toBeInTheDocument()
    expect(screen.getByText(/codex: quota used up/)).toBeInTheDocument()
    expect(screen.queryByText('Completa')).toBeNull()
  })
})

describe('agent theater', () => {
  it('a saved consultation shows exactly its recorded messages, failures included, and invents none', async () => {
    m.one?.mockResolvedValue({ kind: 'saved', consultation: consultation({ status: 'partial' }) })
    m.theater?.mockResolvedValue(timeline())
    renderWithApp(<TheaterPage id="c-1" />)
    const articles = await screen.findAllByRole('article')
    expect(articles).toHaveLength(3)
    expect(screen.getByText('Claude says B.', { exact: false })).toBeInTheDocument()
    const failed = articles.find((a) => a.className.includes('bubble-fail')) as HTMLElement
    expect(within(failed).getByRole('alert')).toHaveTextContent('timed out')
    expect(within(failed).queryByRole('button', { name: /Ver completo/ })).toBeNull()
    expect(screen.getByText(/No hay razonamiento oculto ni diálogo inventado/)).toBeInTheDocument()
    expect(m.create).not.toHaveBeenCalled()
  })

  it('every participant has a named avatar', async () => {
    m.one?.mockResolvedValue({ kind: 'saved', consultation: consultation() })
    m.theater?.mockResolvedValue(timeline())
    renderWithApp(<TheaterPage id="c-1" />)
    await screen.findAllByRole('article')
    const names = Array.from(document.querySelectorAll('.stage-name')).map((e) => e.textContent)
    expect(names.sort()).toEqual(['Claude Code', 'Google Antigravity CLI', 'OpenAI Codex CLI'])
  })

  it('switches to a plain transcript and back', async () => {
    m.one?.mockResolvedValue({ kind: 'saved', consultation: consultation() })
    m.theater?.mockResolvedValue(timeline())
    renderWithApp(<TheaterPage id="c-1" />)
    await screen.findAllByRole('article')
    await userEvent.click(screen.getByRole('button', { name: 'Transcripción' }))
    expect(document.querySelectorAll('.msg')).toHaveLength(3)
    expect(document.querySelectorAll('.bubble')).toHaveLength(0)
    await userEvent.click(screen.getByRole('button', { name: 'Teatro' }))
    expect(document.querySelectorAll('.bubble')).toHaveLength(3)
  })

  it('replays the record in order: nothing is shown before its recorded time', async () => {
    m.one?.mockResolvedValue({ kind: 'saved', consultation: consultation() })
    m.theater?.mockResolvedValue(timeline())
    renderWithApp(<TheaterPage id="c-1" />)
    await screen.findAllByRole('article')
    vi.useFakeTimers({ shouldAdvanceTime: false })
    try {
      fireEvent.click(screen.getByRole('button', { name: /Reproducir desde el inicio/ }))
      expect(document.querySelectorAll('.bubble')).toHaveLength(0)
      // Step to the first recorded message, then the next ones.
      fireEvent.click(screen.getByRole('button', { name: 'Pausar' }))
      fireEvent.click(screen.getByRole('button', { name: 'Siguiente mensaje' }))
      expect(document.querySelectorAll('.bubble')).toHaveLength(1)
      expect(screen.getByRole('article', { name: /Claude Code/ })).toBeInTheDocument()
      fireEvent.click(screen.getByRole('button', { name: 'Siguiente mensaje' }))
      expect(document.querySelectorAll('.bubble')).toHaveLength(2)
      expect(document.querySelector('.bubble-fail')).not.toBeNull()
      fireEvent.click(screen.getByRole('button', { name: 'Siguiente mensaje' }))
      expect(document.querySelectorAll('.bubble')).toHaveLength(3)
      fireEvent.click(screen.getByRole('button', { name: 'Reiniciar' }))
      expect(document.querySelectorAll('.bubble')).toHaveLength(0)
      expect(screen.getByText(/solo comprime el tiempo/)).toBeInTheDocument()
    } finally {
      vi.useRealTimers()
    }
  })

  it('replay reaches the last message even when the record ends a few ms before it', async () => {
    m.one?.mockResolvedValue({ kind: 'saved', consultation: consultation() })
    m.theater?.mockResolvedValue({ ...timeline(), duration_ms: 5950 }) // the last call finished at 6000
    renderWithApp(<TheaterPage id="c-1" />)
    await screen.findAllByRole('article')
    fireEvent.click(screen.getByRole('button', { name: /Reproducir desde el inicio/ }))
    fireEvent.click(screen.getByRole('button', { name: 'Pausar' }))
    const slider = screen.getByRole('slider', { name: 'Posición de la reproducción' })
    fireEvent.change(slider, { target: { value: (slider as HTMLInputElement).max } })
    expect(document.querySelectorAll('.bubble')).toHaveLength(3)
  })

  it('a live consultation appears message by message as events are recorded', async () => {
    m.one?.mockResolvedValue({ kind: 'job', job: jobHeader() })
    renderWithApp(<TheaterPage id="job-1" />)
    await waitFor(() => expect(FakeEventSource.instances).toHaveLength(1))
    const es = FakeEventSource.instances[0] as FakeEventSource
    expect(screen.queryAllByRole('article')).toHaveLength(0)
    expect(await screen.findByText('Esperando el primer mensaje registrado…')).toBeInTheDocument()
    resetSeq()
    const events = threeAgentRun()
    for (const e of events.slice(0, 6)) act(() => es.emit(e.kind, e))
    expect(screen.queryAllByRole('article')).toHaveLength(0) // started and completed, but no message recorded yet
    act(() => es.emit(events[6]?.kind ?? '', events[6]))
    expect(await screen.findAllByRole('article')).toHaveLength(1)
    expect(screen.getAllByText(/Codex says buy A/).length).toBeGreaterThan(0)
    expect(screen.queryByText(/Claude says buy B/)).toBeNull()
    act(() => es.emit(events[8]?.kind ?? '', events[8]))
    expect(screen.getAllByRole('article')).toHaveLength(2)
    expect(screen.getByText('En vivo')).toBeInTheDocument()
    // A reconnect that replays an old event adds nothing.
    act(() => es.emit(events[6]?.kind ?? '', events[6]))
    expect(screen.getAllByRole('article')).toHaveLength(2)
  })
})

describe('history', () => {
  const item = (id: string, over = {}) => ({ id, created_at_ms: Date.now() - 86_400_000, status: 'complete', question: `Pregunta ${id}`, mode: 'decision', coordinator: 'claude', strategy: 'panel', participants: ['codex'], has_research: false, answer_preview: 'resp', ...over })

  it('lists, searches, filters and pages', async () => {
    m.history?.mockResolvedValue({ items: [item('a'), item('b', { status: 'partial' })], total: 40, running: [], history_enabled: true, limit: 15, offset: 0 })
    renderWithApp(<HistoryPage />)
    expect(await screen.findByText('Pregunta a')).toBeInTheDocument()
    expect(m.history?.mock.calls[0]?.[0]).toMatchObject({ limit: 15, offset: 0 })
    await userEvent.type(screen.getByLabelText('Buscar'), 'osciloscopio')
    await userEvent.selectOptions(screen.getByLabelText('Estado'), 'partial')
    await userEvent.click(screen.getByRole('button', { name: 'Aplicar' }))
    await waitFor(() => expect(m.history?.mock.calls.at(-1)?.[0]).toMatchObject({ q: 'osciloscopio', status: 'partial', offset: 0 }))
    await userEvent.click(screen.getByRole('button', { name: 'Siguiente' }))
    await waitFor(() => expect(m.history?.mock.calls.at(-1)?.[0]).toMatchObject({ offset: 15 }))
    expect(screen.getByText('16–30 de 40')).toBeInTheDocument()
  })

  it('warns that an old consultation with sources may be out of date, and opening does not re-run anything', async () => {
    m.history?.mockResolvedValue({ items: [item('old', { created_at_ms: Date.now() - 90 * 86_400_000, has_research: true, retrieved_sources: 2 })], total: 1, running: [], history_enabled: true })
    renderWithApp(<HistoryPage />)
    expect(await screen.findByText(/pueden haber cambiado\. Abrirla no la comprueba de nuevo/)).toBeInTheDocument()
    expect(screen.getByRole('link', { name: 'Reproducir' })).toHaveAttribute('href', '#/theater/old')
    expect(m.create).not.toHaveBeenCalled()
  })

  it('deleting needs a confirmation dialog', async () => {
    m.history?.mockResolvedValue({ items: [item('a')], total: 1, running: [], history_enabled: true })
    m.remove?.mockResolvedValue({ deleted: 'a', note: 'Solo se borró este registro.' })
    renderWithApp(<HistoryPage />)
    await screen.findByText('Pregunta a')
    await userEvent.click(screen.getByRole('button', { name: /Eliminar/ }))
    expect(m.remove).not.toHaveBeenCalled()
    const dlg = screen.getByRole('dialog')
    await userEvent.click(within(dlg).getByRole('button', { name: 'Cancelar' }))
    expect(m.remove).not.toHaveBeenCalled()
    await userEvent.click(screen.getByRole('button', { name: /Eliminar/ }))
    await userEvent.click(within(screen.getByRole('dialog')).getByRole('button', { name: 'Eliminar' }))
    await waitFor(() => expect(m.remove).toHaveBeenCalledWith('a'))
    expect(await screen.findByText(/Consulta eliminada\. Solo se borró su registro guardado/)).toBeInTheDocument()
  })

  it('shows running consultations and says so when history is disabled', async () => {
    m.history?.mockResolvedValue({ items: [], total: 0, running: [jobHeader()], history_enabled: false })
    renderWithApp(<HistoryPage />)
    expect(await screen.findByText('En curso ahora')).toBeInTheDocument()
    expect(screen.getByText(/El historial está desactivado/)).toBeInTheDocument()
  })
})

describe('memory', () => {
  const rec = (over = {}) => ({ id: 'm1', text: 'Mi presupuesto es 8000 MXN', scope: 'project', kind: 'requirement', origin: 'user', created_ms: 1, ...over })

  it('never remembers anything without the explicit confirmation', async () => {
    m.memory?.mockResolvedValue({ items: [], project: null, note: '' })
    m.memoryAdd?.mockResolvedValue({ id: 'm1', duplicate: false })
    renderWithApp(<MemoryPage />)
    await userEvent.click(await screen.findByRole('button', { name: /Añadir un recuerdo/ }))
    await userEvent.type(screen.getByLabelText('Texto'), 'Prefiero marcas conocidas')
    const save = screen.getByRole('button', { name: 'Guardar recuerdo' })
    expect(save).toBeDisabled()
    await userEvent.click(screen.getByLabelText(/Confirmo que quiero que AI Orchestra recuerde esto/))
    expect(save).toBeEnabled()
    await userEvent.click(save)
    await waitFor(() => expect(m.memoryAdd).toHaveBeenCalled())
    expect(m.memoryAdd?.mock.calls[0]?.[0]).toMatchObject({ text: 'Prefiero marcas conocidas', scope: 'project', confirm: true })
    expect(await screen.findByText('Recuerdo guardado.')).toBeInTheDocument()
  })

  it('shows scope and provenance, masks sensitive text and deletes only after confirming', async () => {
    m.memory?.mockResolvedValue({ items: [rec(), rec({ id: 'm2', scope: 'global', sensitive: true, masked: true, text: '[marked sensitive: hidden. It is never sent to an agent.]' })], project: null, note: '' })
    m.memoryForget?.mockResolvedValue({ deleted: 'm1' })
    renderWithApp(<MemoryPage />)
    expect(await screen.findByText('Mi presupuesto es 8000 MXN')).toBeInTheDocument()
    expect(screen.getAllByText('global').length).toBeGreaterThan(0)
    expect(screen.getByText(/marked sensitive: hidden/)).toBeInTheDocument()
    expect(screen.getAllByRole('button', { name: 'Corregir' })[1]).toBeDisabled()
    await userEvent.click(screen.getAllByRole('button', { name: /Eliminar/ })[0] as HTMLElement)
    expect(m.memoryForget).not.toHaveBeenCalled()
    await userEvent.click(within(screen.getByRole('dialog')).getByRole('button', { name: 'Eliminar' }))
    await waitFor(() => expect(m.memoryForget).toHaveBeenCalledWith('m1'))
  })

  it('says when memory is unavailable', async () => {
    m.capabilities?.mockResolvedValue({ ...caps, features: { ...caps.features, memory: false } })
    m.memory?.mockResolvedValue({ items: [], project: null, note: '' })
    renderWithApp(<MemoryPage />)
    expect(await screen.findByText(/La memoria no está disponible/)).toBeInTheDocument()
  })
})

describe('reports', () => {
  it('labels measurements as real and the memory reduction as an estimate', async () => {
    m.analytics?.mockResolvedValue({
      history_enabled: true, consultations: 3, by_status: { complete: 2, partial: 1 }, by_strategy: { panel: { consultations: 3, median_duration_ms: 12000, median_calls: 6, provider_failures: 1 } },
      by_provider: { claude: { recorded_calls: 5, failed_calls: 0, timeouts: 0, median_success_ms: 2000 } },
      tokens_reported_by_the_clis: { calls_reporting: 0, input: 0, output: 0, note: 'Only reported usage is counted.' },
      memory_context: { consultations_with_memory: 2, median_reduction_vs_full_history_percent: 41, kind: 'estimate', note: 'A reference figure.' },
      note: 'These are real measurements.',
    })
    renderWithApp(<ReportsPage />)
    expect(await screen.findByText('medición real')).toBeInTheDocument()
    expect(screen.getByText('estimación')).toBeInTheDocument()
    expect(screen.getByText(/Ningún CLI informó tokens; no se estiman/)).toBeInTheDocument()
    expect(screen.getByText(/una cifra de referencia, no un conteo de tokens/)).toBeInTheDocument()
  })

  it('does not invent numbers when there is no data', async () => {
    m.analytics?.mockResolvedValue({ history_enabled: true, consultations: 0 })
    renderWithApp(<ReportsPage />)
    expect(await screen.findByText(/Aún no hay consultas guardadas que medir/)).toBeInTheDocument()
  })

  it('shows each evaluation with its provenance and its meaning', async () => {
    m.analytics?.mockResolvedValue({ history_enabled: false, consultations: 0 })
    m.reports?.mockResolvedValue({ note: 'This page only reads their results.', items: [{ id: 'eval-1', created_ms: 1, kind: 'quality', dataset: 'starter', dataset_title: 'Starter', dataset_version: '1.0.1', synthetic: false, arms: [], cases: 6, judgments: 2, provenance: [{ class: 'mock', meaning: 'Scripted agents, not a real model.' }] }] })
    renderWithApp(<ReportsPage />)
    await userEvent.click(await screen.findByRole('tab', { name: 'Evaluaciones' }))
    expect(await screen.findByText('Starter')).toBeInTheDocument()
    expect(screen.getByText('mock')).toBeInTheDocument()
    expect(screen.getAllByText('Scripted agents, not a real model.').length).toBeGreaterThan(0)
  })
})

describe('settings, themes and language', () => {
  it('saves the defaults the user chose and nothing else', async () => {
    renderWithApp(<SettingsPage />)
    await screen.findByText('Consulta')
    await userEvent.selectOptions(screen.getByLabelText('Estrategia'), 'fast')
    await userEvent.click(screen.getByRole('button', { name: 'Guardar valores' }))
    await waitFor(() => expect(m.putSettings).toHaveBeenCalledWith({ strategy: 'fast' }))
    expect(await screen.findByText('Valores guardados.')).toBeInTheDocument()
  })

  it('changes the theme at once and saves it', async () => {
    renderWithApp(<SettingsPage />)
    await userEvent.click(await screen.findByRole('tab', { name: 'Apariencia' }))
    await userEvent.selectOptions(screen.getByLabelText('Tema'), 'light')
    await waitFor(() => expect(document.documentElement.dataset.theme).toBe('light'))
    expect(m.putUi?.mock.calls.at(-1)?.[0]).toMatchObject({ theme: 'light' })
  })

  it('switches the whole interface to English', async () => {
    renderWithApp(<SettingsPage />)
    await userEvent.click(await screen.findByRole('tab', { name: 'Apariencia' }))
    await userEvent.selectOptions(screen.getByLabelText('Idioma'), 'en')
    expect(await screen.findByRole('heading', { name: 'Settings' })).toBeInTheDocument()
    expect(document.documentElement.lang).toBe('en')
  })

  it('shows providers without claiming to know who is signed in', async () => {
    m.providers?.mockResolvedValue({ checked_ms: 1, cached_for_secs: 60, history_note: 'h', providers: [{ id: 'claude', display_name: 'Claude Code', aliases: [], enabled: true, executable: '/usr/bin/claude', installed: true, version: '2.1.0', usable: true, reason: null, authentication: { status: 'unknown', note: 'n' }, capabilities: { web_search: 'documented' }, configured_model: null, configured_effort: null, history: { invocations: 4, successes: 3, failures: 1, timeouts: 0, quota_errors: 0, auth_errors: 0, latency_ms: { answer: { mean: 2300, samples: 3 } }, last_success_ms: 1, last_failure_ms: null, last_failure_kind: null, failure_streak: 0, tokens_reported: { calls: 0, input: 0, output: 0 } } }] })
    renderWithApp(<SettingsPage />)
    await userEvent.click(await screen.findByRole('tab', { name: 'Agentes' }))
    expect(await screen.findByText('2.1.0')).toBeInTheDocument()
    expect(screen.getByText('Desconocido')).toBeInTheDocument()
    expect(screen.getByText(/abrir esta página nunca la hace/)).toBeInTheDocument()
    expect(screen.getByText(/solo ejecuta `--version`/)).toBeInTheDocument()
  })
})

describe('application shell', () => {
  it('has labelled navigation, a skip link and marks the current page', async () => {
    window.location.hash = '#/history'
    renderWithApp(<App />)
    await flush()
    const nav = screen.getByRole('navigation', { name: 'Navegación principal' })
    expect(within(nav).getAllByRole('link')).toHaveLength(7)
    expect(within(nav).getByRole('link', { name: /Historial/ })).toHaveAttribute('aria-current', 'page')
    expect(screen.getByRole('link', { name: 'Saltar al contenido' })).toBeInTheDocument()
    expect(screen.getByRole('main')).toBeInTheDocument()
  })

  it('navigates with the keyboard and lists the shortcuts', async () => {
    renderWithApp(<App />)
    await flush()
    await userEvent.keyboard('{Alt>}{2}{/Alt}')
    await waitFor(() => expect(window.location.hash).toBe('#/theater'))
    await userEvent.keyboard('?')
    expect(await screen.findByRole('dialog', { name: 'Atajos de teclado' })).toBeInTheDocument()
    await userEvent.keyboard('{Escape}')
    expect(screen.queryByRole('dialog')).toBeNull()
  })

  it('dark is the default theme and the theme button toggles it', async () => {
    renderWithApp(<App />)
    await flush()
    await waitFor(() => expect(document.documentElement.dataset.theme).toBe('dark'))
    await userEvent.click(screen.getByRole('button', { name: /Cambiar entre tema oscuro y claro/ }))
    await waitFor(() => expect(document.documentElement.dataset.theme).toBe('light'))
  })

  it('reduced motion from the preference is applied to the page', async () => {
    m.getUi?.mockResolvedValue({ preferences: { theme: 'dark', language: 'es', reduced_motion: 'on', theater_default: false } })
    renderWithApp(<App />)
    await waitFor(() => expect(document.documentElement.dataset.motion).toBe('reduced'))
  })
})
