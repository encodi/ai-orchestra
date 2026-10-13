// The animated theater above the chat, the conversation view, characters, the summary button and the usage report.
// Everything runs against the real binary with fake CLIs (no model); screenshots go to docs/screenshots.
import { mkdirSync } from 'node:fs'
import { dirname, join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'
import type { Page } from '@playwright/test'
import { ask, expect, test } from './fixtures'

const out = resolve(dirname(fileURLToPath(import.meta.url)), '..', '..', 'docs', 'screenshots')
mkdirSync(out, { recursive: true })

const choose = async (page: Page, label: string, value: string) => {
  const saved = page.waitForResponse((r) => r.url().includes('/ui-preferences') && r.request().method() === 'PUT')
  await page.getByLabel(label, { exact: true }).selectOption(value)
  await saved
}

test.describe.configure({ mode: 'serial' })

test('the animated theater opens with the consultation and its bubbles are real, attributed messages', async ({ page, open, server }, info) => {
  await open('/settings')
  await page.getByRole('tab', { name: 'Apariencia' }).click()
  await choose(page, 'Tema', 'dark')
  await page.goto(`${server.url}/#/`)
  await ask(page, 'Muéstrame el teatro animado', { strategy: 'thorough' })
  const stage = page.locator('.stage')
  await expect(stage).toBeVisible()
  await expect(stage).toHaveAttribute('aria-hidden', 'true') // decoration; the same messages are in the conversation below
  await expect(stage.locator('.stage-char')).toHaveCount(3)
  const names = await stage.locator('.stage-name').allInnerTexts()
  expect(new Set(names)).toEqual(new Set(['Claude Code', 'OpenAI Codex CLI', 'Google Antigravity CLI']))
  // The characters really walk (a stage change moves them) ...
  await expect(stage.locator('.stage-char[data-moving]').first()).toBeAttached({ timeout: 20_000 })
  // ... and a bubble appears only for a message that was recorded, carrying that agent's own words.
  const bubble = stage.locator('.stage-bubble').first()
  await expect(bubble).toBeVisible({ timeout: 30_000 })
  await expect(bubble).toContainText('answer after reading')
  const owner = await bubble.evaluate((el) => (el.closest('.stage-char') as HTMLElement).dataset.agent)
  expect(['claude', 'codex', 'google']).toContain(owner)
  await expect(stage).toHaveAttribute('data-curtains', 'open') // they are working, so the curtains are open
  await stage.screenshot({ path: join(out, `${info.project.name}-dark-stage-live.png`) })
  await expect(page.locator('.result')).toBeVisible({ timeout: 60_000 })
  await expect(stage).toHaveAttribute('data-activity', 'done')
  await expect(stage).toHaveAttribute('data-curtains', 'closed') // the show is over: the curtains close
  await page.waitForTimeout(2400) // the curtains take a moment to close
  await stage.screenshot({ path: join(out, `${info.project.name}-dark-stage-done.png`) })
  // The user can hide it.
  await page.getByRole('button', { name: 'Ocultar teatro' }).click()
  await expect(stage).toHaveCount(0)
  await page.getByRole('button', { name: 'Mostrar teatro' }).click()
  await expect(page.locator('.stage')).toBeVisible()
})

test('with reduced motion nobody walks and the stage still shows the same facts', async ({ page, open }) => {
  await page.emulateMedia({ reducedMotion: 'reduce' })
  await open()
  await ask(page, 'Sin animaciones por favor', { strategy: 'thorough' })
  await expect(page.locator('.stage .stage-char')).toHaveCount(3)
  await expect(page.locator('.result')).toBeVisible({ timeout: 60_000 })
  expect(await page.locator('.stage-char[data-moving]').count()).toBe(0)
  await expect(page.locator('.stage')).toHaveAttribute('data-activity', 'done')
})

test('the stage can be switched off in the settings and stays off', async ({ page, open }) => {
  await open('/settings')
  await page.getByRole('tab', { name: 'Apariencia' }).click()
  const saved = page.waitForResponse((r) => r.url().includes('/ui-preferences') && r.request().method() === 'PUT')
  await page.getByLabel('Mostrar el teatro animado sobre el chat mientras corre una consulta').uncheck()
  await saved
  await page.goto(page.url().split('#')[0] + '#/')
  await ask(page, 'Sin teatro', { strategy: 'fast' })
  await expect(page.locator('.result')).toBeVisible({ timeout: 60_000 })
  await expect(page.locator('.stage')).toHaveCount(0)
  await page.goto(page.url().split('#')[0] + '#/settings')
  await page.getByRole('tab', { name: 'Apariencia' }).click()
  const back = page.waitForResponse((r) => r.url().includes('/ui-preferences') && r.request().method() === 'PUT')
  await page.getByLabel('Mostrar el teatro animado sobre el chat mientras corre una consulta').check()
  await back
})

test('the conversation reads as a conversation: grouped by what the group did, with the agents’ own bubbles', async ({ page, open }, info) => {
  await open()
  await ask(page, 'Una pregunta para conversar', { strategy: 'thorough' })
  await expect(page.locator('.result')).toBeVisible({ timeout: 60_000 })
  const groups = await page.locator('.result .conversation .stage-title').allInnerTexts()
  expect(groups.map((g) => g.toLowerCase())).toEqual(expect.arrayContaining(['cada uno pensó por su cuenta', 'se revisaron entre sí', 'el coordinador redactó el informe']))
  await expect(page.locator('.result .bubble').first()).toBeVisible()
  await page.screenshot({ path: join(out, `${info.project.name}-dark-conversation.png`) })
})

test('summary: one explicit request to the coordinator, labelled as written by a model, kept with the consultation', async ({ page, open, server }, info) => {
  await open()
  await ask(page, 'Pregunta para resumir', { strategy: 'thorough' })
  await expect(page.locator('.result')).toBeVisible({ timeout: 60_000 })
  await expect(page.getByText('Pide al coordinador un resumen corto')).toBeVisible()
  const before = server.calls().length
  await page.getByRole('button', { name: 'Resumir el reporte' }).click()
  const box = page.locator('.summary-box')
  await expect(box.getByText(/Resumen escrito por Claude Code/)).toBeVisible({ timeout: 30_000 })
  await expect(box).toContainText('Bottom line: choose the entry model.')
  await expect(box).toContainText('Condensa, no verifica')
  expect(server.calls().length - before, 'exactly one call').toBe(1)
  expect(server.calls().at(-1)).toBe('claude') // the coordinator
  await page.screenshot({ path: join(out, `${info.project.name}-dark-summary.png`) })
  // It is saved with the consultation: open it again from the history.
  await page.getByRole('link', { name: 'Historial' }).click()
  await page.getByLabel('Buscar').fill('Pregunta para resumir')
  await page.getByRole('button', { name: 'Aplicar' }).click()
  await expect(page.locator('.hist-item')).toHaveCount(1)
  await page.getByRole('link', { name: 'Abrir' }).click()
  await expect(page.locator('.summary-box')).toContainText('Bottom line: choose the entry model.')
  await expect(page.getByRole('button', { name: 'Resumir de nuevo' })).toBeVisible()
})

test('characters: a name, a profile and a look are saved, drawn everywhere and written into the prompts', async ({ page, open, server }, info) => {
  await open('/settings')
  await page.getByRole('tab', { name: 'Personajes' }).click()
  await expect(page.getByText('no dan conciencia ni mente a un modelo', { exact: false })).toBeVisible()
  const card = page.getByRole('region', { name: 'Claude Code' })
  await card.getByLabel('Nombre', { exact: true }).fill('Ámbar')
  await card.getByRole('button', { name: 'Escéptico' }).click()
  await expect(card.getByLabel('Personalidad', { exact: true })).toHaveValue(/Escéptico y cuidadoso/)
  await card.locator('label.chip', { hasText: 'Colores 3' }).click()
  await card.locator('label.chip', { hasText: 'Lentes' }).click()
  await page.getByLabel('Idioma de las respuestas', { exact: true }).selectOption('es')
  const save = page.getByRole('button', { name: 'Guardar personajes' })
  await expect(save).toBeEnabled()
  await page.screenshot({ path: join(out, `${info.project.name}-dark-characters.png`), fullPage: true })
  await save.click()
  await expect(page.getByText('Personajes guardados.')).toBeVisible()
  await page.reload()
  await page.getByRole('tab', { name: 'Personajes' }).click()
  const again = page.getByRole('region', { name: 'Claude Code' })
  await expect(again.getByLabel('Nombre', { exact: true })).toHaveValue('Ámbar')
  await expect(again.locator('label.chip.on', { hasText: 'Lentes' })).toBeVisible()

  await page.goto(`${server.url}/#/`)
  await ask(page, 'Con personajes', { strategy: 'thorough' })
  await expect(page.locator('.stage .stage-name', { hasText: 'Ámbar' })).toBeVisible()
  await expect(page.locator('.result')).toBeVisible({ timeout: 60_000 })
  await page.getByRole('tab', { name: /^Conversación/ }).click()
  const bubbleName = page.locator('.result .bubble-body > .row > strong', { hasText: 'Ámbar' }).first()
  await expect(bubbleName).toBeVisible()
  await expect(page.locator('.result .bubble-body > .row', { hasText: 'Ámbar' }).first()).toContainText('Claude Code') // the real CLI stays visible
  const prompts = server.prompts()
  expect(prompts).toContain('In this group you go by the name "Ámbar"')
  expect(prompts).toContain('Escéptico y cuidadoso')
  expect(prompts).toContain('Write in Spanish')
  // First answers never name the other participants; reviews do.
  const blocks = prompts.split(/=== (?:claude|codex|google) ===\n/).filter(Boolean)
  const first = blocks.filter((b) => b.includes('you cannot see their work'))
  expect(first.length).toBeGreaterThanOrEqual(3)
  expect(first.every((b) => !b.includes('colleagues in a meeting'))).toBe(true)
  expect(prompts).toContain('colleagues in a meeting')
  await page.screenshot({ path: join(out, `${info.project.name}-dark-stage-characters.png`) })
})

test('usage: tokens per agent from what the CLIs reported, and no invented quota', async ({ page, open }, info) => {
  await open('/reports')
  const card = page.locator('.card', { has: page.getByRole('heading', { name: 'Uso por agente' }) })
  await expect(card).toBeVisible()
  await expect(card.getByText('Cuánta cuota te queda en cada plan', { exact: false })).toBeVisible()
  const rows = card.locator('tbody tr')
  // Claude plays "Ámbar" here; the seeded research record adds three more agent names.
  await expect(rows.filter({ hasText: /Ámbar|Claude Code/ })).toHaveCount(1)
  await expect(rows.filter({ hasText: 'OpenAI Codex CLI' })).toHaveCount(1)
  await expect(rows.filter({ hasText: /Ámbar|Claude Code/ })).toContainText(/\d+ \/ \d+/) // input / output tokens the fake CLI reported
  const text = await rows.allInnerTexts()
  expect(text.join('\n')).toMatch(/llamada\(s\)/)
  // The fake Antigravity reports no tokens: it shows "–", not zeros.
  const google = rows.filter({ hasText: 'Google Antigravity CLI' })
  await expect(google).toContainText('–')
  await expect(card.getByText('Ninguna registrada').first()).toBeVisible()
  await page.screenshot({ path: join(out, `${info.project.name}-dark-reports-usage.png`), fullPage: true })
})
