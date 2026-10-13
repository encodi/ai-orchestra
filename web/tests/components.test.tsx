import { describe, expect, it, vi } from 'vitest'
import { fireEvent, render, screen, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { useState } from 'react'
import type { SettingsObj, ResearchResponse } from '../src/api/types'
import { PixelRobot, type RobotState } from '../src/components/PixelRobot'
import { SettingsFields } from '../src/components/SettingsPanel'
import { ResearchView } from '../src/components/ResearchView'
import { Dialog, Tabs } from '../src/components/ui'
import { MessageCard } from '../src/components/MessageCard'
import { caps } from './fixtures'
import { renderI18n, renderWithApp } from './helpers'

describe('pixel robots', () => {
  const kinds = ['claude', 'codex', 'google'] as const
  const states: RobotState[] = ['idle', 'thinking', 'speaking', 'reviewing', 'waiting', 'finished', 'error']

  it('draws every robot in every state as crisp SVG rectangles', () => {
    for (const k of kinds) {
      for (const s of states) {
        const { container, unmount } = render(<PixelRobot kind={k} state={s} />)
        const svg = container.querySelector('svg')
        expect(svg, `${k}/${s}`).not.toBeNull()
        expect(svg?.getAttribute('shape-rendering')).toBe('crispEdges')
        expect(svg?.querySelectorAll('rect').length).toBeGreaterThan(20)
        expect(container.querySelector('image, img, foreignObject')).toBeNull()
        unmount()
      }
    }
  })

  it('has a text alternative that names the agent and its state, unless decorative', () => {
    const { rerender } = render(<PixelRobot kind="codex" state="thinking" />)
    expect(screen.getByRole('img', { name: 'Circuit Bot: thinking' })).toBeInTheDocument()
    rerender(<PixelRobot kind="codex" state="thinking" decorative />)
    expect(screen.queryByRole('img')).toBeNull()
  })

  it('the three robots are visibly different sprites', () => {
    const sig = (k: (typeof kinds)[number]) => {
      const { container, unmount } = render(<PixelRobot kind={k} />)
      const s = Array.from(container.querySelectorAll('rect')).map((r) => `${r.getAttribute('x')},${r.getAttribute('y')},${r.getAttribute('width')}`).join('|')
      unmount()
      return s
    }
    expect(new Set(kinds.map(sig)).size).toBe(3)
  })

  it('shows an error face that differs from the normal one', () => {
    const face = (s: RobotState) => {
      const { container, unmount } = render(<PixelRobot kind="claude" state={s} />)
      const html = container.innerHTML
      unmount()
      return html
    }
    expect(face('error')).not.toBe(face('idle'))
    expect(face('error')).toContain('px-x')
  })
})

function Panel({ initial = {}, inForce }: { initial?: SettingsObj; inForce?: Record<string, never> }) {
  const [v, setV] = useState<SettingsObj>(initial)
  return (
    <div>
      <SettingsFields caps={caps} value={v} onChange={setV} inForce={inForce} originOf={() => 'request'} />
      <output data-testid="value">{JSON.stringify(v)}</output>
    </div>
  )
}

describe('settings panel', () => {
  it('shows every setting the backend schema declares', () => {
    renderI18n(<Panel />)
    for (const label of ['Modo', 'Estrategia', 'Coordinador', 'Agentes participantes', 'Rondas de revisión', 'Profundidad de investigación', 'Búsqueda web', 'Extensión', 'Tiempo por agente (s)', 'Máximo de llamadas', 'Memoria', 'Presupuesto de contexto', 'Vista', 'Máximo de fuentes', 'Frescura de fuentes (días)', 'Duración máxima (s)', 'Llamadas en paralelo']) {
      expect(screen.getByText(label), label).toBeInTheDocument()
    }
  })

  it('starts on Auto and a manual choice is kept separate until reset', async () => {
    renderI18n(<Panel />)
    expect(screen.getByTestId('value').textContent).toBe('{}')
    await userEvent.selectOptions(screen.getByLabelText('Estrategia'), 'thorough')
    expect(JSON.parse(screen.getByTestId('value').textContent ?? '')).toEqual({ strategy: 'thorough' })
    await userEvent.click(screen.getByRole('button', { name: /Volver a Auto: Estrategia/ }))
    expect(screen.getByTestId('value').textContent).toBe('{}')
  })

  it('accepts a list of agents and returns to Auto when none is selected', async () => {
    renderI18n(<Panel />)
    await userEvent.click(screen.getByLabelText('claude'))
    await userEvent.click(screen.getByLabelText('codex'))
    expect(JSON.parse(screen.getByTestId('value').textContent ?? '')).toEqual({ agents: ['claude', 'codex'] })
    await userEvent.click(screen.getByLabelText('claude'))
    await userEvent.click(screen.getByLabelText('codex'))
    expect(screen.getByTestId('value').textContent).toBe('{}')
  })

  it('numbers carry their bounds and become integers; clearing returns to Auto', () => {
    renderI18n(<Panel />)
    const input = screen.getByLabelText('Máximo de fuentes') as HTMLInputElement
    expect([input.min, input.max]).toEqual(['1', '20'])
    fireEvent.change(input, { target: { value: '7' } })
    expect(JSON.parse(screen.getByTestId('value').textContent ?? '')).toEqual({ max_sources: 7 })
    fireEvent.change(input, { target: { value: '' } })
    expect(screen.getByTestId('value').textContent).toBe('{}')
  })

  it('disables what cannot be honoured and says why', () => {
    renderI18n(<Panel />)
    const web = screen.getByLabelText('Búsqueda web') as HTMLSelectElement
    const on = Array.from(web.options).find((o) => o.value === 'on')
    expect(on?.disabled).toBe(true)
    expect(on?.textContent).toContain('no disponible')
    expect(screen.getByLabelText('Memoria')).toBeDisabled()
    expect(screen.getByText('memory is not available in this server')).toBeInTheDocument()
  })

  it('marks a manually set value so it cannot be mistaken for an automatic one', () => {
    const { container } = renderI18n(<Panel initial={{ depth: 'deep' }} />)
    expect(container.querySelectorAll('.setting.manual')).toHaveLength(1)
    expect(screen.getByText('Solo esta pregunta')).toBeInTheDocument()
  })
})

describe('tabs, dialogs and keyboard use', () => {
  it('moves between tabs with the arrow keys', async () => {
    const Demo = () => {
      const [t, setT] = useState<'a' | 'b'>('a')
      return <Tabs label="demo" value={t} onChange={setT} tabs={[{ id: 'a', label: 'Uno' }, { id: 'b', label: 'Dos' }]} />
    }
    render(<Demo />)
    const first = screen.getByRole('tab', { name: 'Uno' })
    first.focus()
    await userEvent.keyboard('{ArrowRight}')
    expect(screen.getByRole('tab', { name: 'Dos' })).toHaveAttribute('aria-selected', 'true')
    expect(screen.getByRole('tab', { name: 'Dos' })).toHaveFocus()
    expect(screen.getByRole('tab', { name: 'Uno' })).toHaveAttribute('tabindex', '-1')
  })

  it('a dialog is modal, closes with Escape and traps focus', async () => {
    const onCancel = vi.fn()
    const onConfirm = vi.fn()
    render(
      <Dialog title="¿Eliminar?" confirmLabel="Eliminar" cancelLabel="Cancelar" tone="danger" onConfirm={onConfirm} onCancel={onCancel}>
        <p>cuerpo</p>
      </Dialog>,
    )
    const dlg = screen.getByRole('dialog', { name: '¿Eliminar?' })
    expect(dlg).toHaveAttribute('aria-modal', 'true')
    expect(within(dlg).getByRole('button', { name: 'Cancelar' })).toHaveFocus()
    await userEvent.tab()
    expect(within(dlg).getByRole('button', { name: 'Eliminar' })).toHaveFocus()
    await userEvent.tab()
    expect(within(dlg).getByRole('button', { name: 'Cancelar' })).toHaveFocus()
    await userEvent.keyboard('{Escape}')
    expect(onCancel).toHaveBeenCalled()
    expect(onConfirm).not.toHaveBeenCalled()
  })
})

describe('message cards', () => {
  const base = { key: 'k', agent: 'codex', role: 'perspective', stage: 'independent_analysis', round: 0, technical: false, durationMs: 900, offsetMs: 1000 }

  it('shows exactly the text the agent returned', async () => {
    renderWithApp(<MessageCard m={{ ...base, ok: true, text: 'Codex says **buy A**.', error: null }} />)
    expect(screen.getByText(/Codex says buy A/)).toBeInTheDocument()
    await userEvent.click(screen.getByRole('button', { name: /Ver completo/ }))
    expect(screen.getByText('buy A').tagName).toBe('STRONG')
  })

  it('shows a failure as a failure with its error and no invented reply', () => {
    renderWithApp(<MessageCard m={{ ...base, ok: false, text: null, error: 'timed out after 30s' }} />)
    expect(screen.getByRole('alert')).toHaveTextContent('La llamada falló')
    expect(screen.getByRole('alert')).toHaveTextContent('timed out after 30s')
    expect(screen.getByText(/no se inventa ninguna/)).toBeInTheDocument()
    expect(screen.queryByRole('button', { name: /Ver completo/ })).toBeNull()
  })
})

const research: ResearchResponse = {
  consultation_id: 'c-1', web_occurred: true, retrieved_sources: 1, depth: 'standard', max_sources: 6, judgement_note: 'model-dependent',
  sources: [
    { id: 1, original_url: 'https://vendor.example/ds', display_url: 'https://vendor.example/ds', domain: 'vendor.example', title: 'Datasheet', status: 'retrieved', retrieved: true, retrieved_at_ms: 1_785_542_400_000, age_days: 70, provenance: 'retrieved by AI Orchestra itself', text_chars: 100, text_preview: 'x', excerpts: [{ text: 'has 100 MHz of bandwidth', offset: 0 }] },
    { id: 2, original_url: 'javascript:alert(1)', display_url: 'javascript:alert(1)', domain: 'evil', title: 'Odd', status: 'inaccessible', retrieved: false, provenance: 'reported or found but NOT retrieved: it is not evidence', text_chars: 0, text_preview: '', error: 'the page answered 404' },
  ],
  claims: [
    { id: 1, text: 'The Model X has 100 MHz', originating_agents: ['claude'], status: 'CONTRADICTED', supporting_independent_sources: 1, contradicting_independent_sources: 1, cited_by_agents: [1], evidence: [{ source_id: 1, relation: 'supports', passage: 'has 100 MHz of bandwidth', offset: 0, assessed_by: ['codex'] }] },
    { id: 2, text: 'Unchecked claim', originating_agents: ['claude'], status: 'NOT_CHECKED', supporting_independent_sources: 0, contradicting_independent_sources: 0, evidence: [] },
  ],
  contradictions: [{ claim_id: 1, supporting_sources: [1], contradicting_sources: [2] }],
  limitations: ['One page could not be retrieved.'],
}

describe('research view', () => {
  it('keeps retrieved sources apart from candidates that are not evidence', async () => {
    renderI18n(<ResearchView data={research} focus={null} onFocus={() => undefined} />)
    expect(screen.getByText('1 fuente(s) descargada(s)')).toBeInTheDocument()
    expect(screen.getByText('1 candidata(s) no descargada(s)')).toBeInTheDocument()
    expect(screen.getByText(/Solo una página que AI Orchestra descargó cuenta como fuente/)).toBeInTheDocument()
    await userEvent.click(screen.getByRole('button', { name: /Candidatas sin descargar/ }))
    expect(screen.getByText(/No son evidencia/)).toBeInTheDocument()
  })

  it('never makes a non-http URL clickable and opens real ones safely', async () => {
    renderI18n(<ResearchView data={research} focus={null} onFocus={() => undefined} />)
    await userEvent.click(screen.getByRole('button', { name: /Candidatas sin descargar/ }))
    const links = Array.from(document.querySelectorAll('a[href]'))
    expect(links.some((a) => a.getAttribute('href')?.startsWith('javascript:'))).toBe(false)
    const vendor = links.find((a) => a.getAttribute('href') === 'https://vendor.example/ds')
    expect(vendor?.getAttribute('rel')).toBe('noopener noreferrer')
    expect(vendor?.getAttribute('target')).toBe('_blank')
  })

  it('shows each claim status and its evidence with the model-dependence label', async () => {
    renderI18n(<ResearchView data={research} focus={null} onFocus={() => undefined} />)
    expect(screen.getByText('Contradicha')).toBeInTheDocument()
    expect(screen.getByText('No comprobada')).toBeInTheDocument()
    await userEvent.click(screen.getByRole('button', { name: /Evidencia \(1\)/ }))
    expect(screen.getAllByText(/has 100 MHz of bandwidth/).length).toBeGreaterThan(0)
    expect(screen.getAllByText(/depende de él|depende del modelo|lectura la hace un modelo/).length).toBeGreaterThan(0)
  })

  it('jumps to a source when its number is chosen', async () => {
    const onFocus = vi.fn()
    renderI18n(<ResearchView data={research} focus={null} onFocus={onFocus} />)
    await userEvent.click(screen.getByRole('button', { name: /Evidencia \(1\)/ }))
    await userEvent.click(screen.getByRole('button', { name: 'Ir a la fuente 1' }))
    expect(onFocus).toHaveBeenCalledWith(1)
  })

  it('says plainly when no web research was done', () => {
    renderI18n(<ResearchView data={{ consultation_id: 'c', web_occurred: false, note: 'No web research was done for this consultation.' }} focus={null} onFocus={() => undefined} />)
    expect(screen.getByText(/No se hizo investigación web en esta consulta/)).toBeInTheDocument()
  })
})
