import { execFileSync } from 'node:child_process'
import { ask, expect, test } from './fixtures'

const callsShown = async (page: import('@playwright/test').Page): Promise<number> => {
  const text = await page.locator('.result .pill', { hasText: /llamada/ }).first().innerText()
  return Number(/(\d+)/.exec(text)?.[1])
}

test.describe.configure({ mode: 'serial' })

test('opening, reloading and browsing every page calls no agent', async ({ page, open, server }) => {
  const before = server.calls().length
  await open()
  await expect(page.getByText('Abrir o recargar esta página nunca llama a un agente')).toBeVisible()
  await page.reload()
  for (const hash of ['/theater', '/history', '/research', '/reports', '/memory', '/settings']) {
    await page.goto(`${server.url}/#${hash}`)
    await expect(page.locator('main h1')).toBeVisible()
  }
  await page.getByRole('tab', { name: 'Agentes' }).click()
  await expect(page.locator('.provider').first()).toBeVisible()
  expect(server.calls().length).toBe(before)
})

test('a three-agent consultation shows live progress, then the answer, every agent and the details', async ({ page, open, server }) => {
  const before = server.calls().length
  await open()
  await ask(page, 'Compara tres osciloscopios para aprender', { strategy: 'thorough' })
  const progress = page.locator('.progress')
  await expect(progress).toBeVisible()
  for (const name of ['Claude Code', 'OpenAI Codex CLI', 'Google Antigravity CLI']) await expect(progress.getByText(name).first()).toBeVisible()
  await expect(progress.getByRole('button', { name: 'Cancelar' })).toBeVisible()
  await expect(progress.locator('.show-line')).toHaveText(/…$/) // theatre wording, not "In progress / Working"
  await expect(progress).not.toContainText(/Trabajando|Working/)
  await expect(page.locator('.result')).toBeVisible({ timeout: 60_000 })
  await expect(page.locator('.result').getByText('Completa', { exact: true })).toBeVisible()
  // A live consultation opens on the conversation; the formal answer is one tab away.
  await expect(page.getByRole('tab', { name: /^Conversación/ })).toHaveAttribute('aria-selected', 'true')
  await page.getByRole('tab', { name: 'Respuesta' }).click()
  await expect(page.locator('.result table')).toBeVisible() // the answer's Markdown table
  await expect(page.locator('.result h2', { hasText: 'Recomendación' })).toBeVisible() // English headings, shown translated
  const n = await callsShown(page)
  expect(n).toBeGreaterThanOrEqual(6)
  expect(server.calls().length - before).toBe(n) // one fake-CLI call per call the interface reports
  await page.getByRole('tab', { name: /^Conversación/ }).click()
  await expect(page.locator('.result .bubble')).toHaveCount(n)
  const names = await page.locator('.result .bubble-body > .row > strong').allInnerTexts()
  expect(new Set(names)).toEqual(new Set(['Claude Code', 'OpenAI Codex CLI', 'Google Antigravity CLI']))
  await page.getByRole('tab', { name: 'Detalles' }).click()
  const row = page.locator('tr', { hasText: 'strategy' })
  await expect(row).toContainText('thorough')
  await expect(row).toContainText('this question only')
  await expect(page.locator('tr', { hasText: /^mode/ })).toContainText('AUTO')
  await page.getByRole('tab', { name: 'Investigación' }).click()
  await expect(page.getByText('No se hizo investigación web')).toBeVisible()
})

test('refreshing mid-run follows the same consultation and never repeats a call', async ({ page, open, server }) => {
  const before = server.calls().length
  await open()
  await ask(page, 'Refresca durante la ejecución', { strategy: 'thorough' })
  await expect(page.locator('.progress')).toBeVisible()
  await page.waitForTimeout(1200)
  await page.reload()
  await expect(page.locator('.result')).toBeVisible({ timeout: 60_000 })
  const n = await callsShown(page)
  expect(server.calls().length - before).toBe(n)
  await expect(page.locator('.bubble-user')).toHaveCount(1)
})

test('a follow-up continues the same session', async ({ page, open }) => {
  await open()
  await ask(page, 'Primera pregunta de la sesión', { strategy: 'fast' })
  await expect(page.locator('.result')).toBeVisible({ timeout: 60_000 })
  const session = await page.locator('.page-head .mono').innerText()
  expect(session).toMatch(/^ses-/)
  await ask(page, 'Segunda pregunta de la misma sesión', { strategy: 'fast' })
  await expect(page.locator('.result')).toHaveCount(2, { timeout: 60_000 })
  await expect(page.locator('.page-head .mono')).toHaveText(session)
  await page.getByRole('button', { name: 'Nueva conversación' }).click()
  await expect(page.locator('.bubble-user')).toHaveCount(0)
  await expect(page.getByText('Conversación nueva')).toBeVisible()
})

test('one agent: a single call and a single message', async ({ page, open, server }) => {
  const before = server.calls().length
  await open()
  await ask(page, 'Pregunta simple a un solo agente', { strategy: 'fast', agents: 'Claude Code' })
  await expect(page.locator('.result')).toBeVisible({ timeout: 60_000 })
  expect(await callsShown(page)).toBe(1)
  expect(server.calls().length - before).toBe(1)
  await page.getByRole('tab', { name: /^Conversación/ }).click()
  await expect(page.locator('.result .bubble')).toHaveCount(1)
})

test('a disagreement the coordinator recorded is flagged in the theater', async ({ page, open }) => {
  await open()
  await ask(page, 'Dilema con DISAGREE entre los agentes', { strategy: 'thorough' })
  await expect(page.locator('.result')).toBeVisible({ timeout: 60_000 })
  await page.getByRole('link', { name: 'Ver en el Teatro' }).click()
  await expect(page.getByText('El coordinador dejó desacuerdos sin resolver.')).toBeVisible()
  await expect(page.locator('.bubble').first()).toBeVisible()
})

test('a failing agent makes the result partial and is shown as a failure, never as a reply', async ({ page, open }) => {
  await open()
  await ask(page, 'Esto falla en uno FAIL_CODEX', { strategy: 'thorough' })
  await expect(page.locator('.result')).toBeVisible({ timeout: 60_000 })
  await expect(page.getByText('Resultado parcial.')).toBeVisible()
  await expect(page.locator('.result').getByText('Parcial', { exact: true })).toBeVisible()
  await expect(page.locator('.result').getByText('Completa', { exact: true })).toHaveCount(0)
  await page.getByRole('tab', { name: /^Conversación/ }).click()
  const failed = page.locator('.result .bubble-fail')
  await expect(failed.first()).toBeVisible()
  await expect(failed.first()).toContainText('OpenAI Codex CLI')
  await expect(failed.first()).toContainText('no se inventa ninguna')
})

test('an agent that exceeds its time limit is reported as a timeout', async ({ page, open }) => {
  await open()
  await ask(page, 'Este tarda demasiado SLOW_CODEX', { strategy: 'thorough', timeout: '2' })
  await expect(page.locator('.result')).toBeVisible({ timeout: 80_000 })
  await expect(page.getByText('Resultado parcial.')).toBeVisible()
  await page.getByRole('tab', { name: /^Conversación/ }).click()
  await expect(page.locator('.result .bubble-fail').first()).toContainText(/tim|tiempo|exceed/i)
})

test('cancelling stops the agents and is never shown as complete', async ({ page, open, server }) => {
  await open()
  await ask(page, 'Esto no termina SLEEPALL', { strategy: 'thorough' })
  await expect(page.locator('.progress')).toBeVisible()
  await page.waitForTimeout(1200)
  await page.getByRole('button', { name: 'Cancelar', exact: true }).click()
  await expect(page.getByText(/Cancelada\. Los agentes en ejecución se terminaron/)).toBeVisible({ timeout: 20_000 })
  await expect(page.locator('.result').getByText('Completa', { exact: true })).toHaveCount(0)
  await page.waitForTimeout(800)
  let left = ''
  try {
    left = execFileSync('pgrep', ['-f', `${server.dir}/bin/`], { encoding: 'utf8' }) // no shell: a shell's own command line would match too
  } catch {
    left = '' // pgrep exits 1 when nothing matches
  }
  expect(left.trim(), 'no fake agent process is left running').toBe('')
})

test('a validation error from the server is shown and nothing starts', async ({ page, open, server }) => {
  const before = server.calls().length
  await open()
  await page.getByRole('button', { name: /Ajustes de esta consulta/ }).click()
  await page.locator('#c-request-web').selectOption('off')
  await page.getByRole('textbox', { name: /Páginas HTTPS/ }).fill('https://example.com/a')
  await page.getByLabel('Tu pregunta').fill('Con URL pero sin web')
  await page.getByRole('button', { name: /^Enviar/ }).click()
  await expect(page.getByRole('alert').filter({ hasText: 'web access is off' })).toBeVisible()
  expect(server.calls().length).toBe(before)
})

test('the plan preview runs nothing and explains the choices', async ({ page, open, server }) => {
  const before = server.calls().length
  await open()
  await page.getByLabel('Tu pregunta').fill('¿Qué plan seguirías para esto?')
  await page.getByRole('button', { name: /Ver plan/ }).click()
  await expect(page.getByText('No se ejecutó nada')).toBeVisible()
  await expect(page.locator('.preview .plain-pre')).not.toBeEmpty()
  expect(server.calls().length).toBe(before)
})
