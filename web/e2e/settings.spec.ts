import type { Page } from '@playwright/test'
import { expect, test } from './fixtures'

/** Chooses a value and waits until the server has stored the preference (it is saved right after it is applied). */
const choose = async (page: Page, label: string, value: string) => {
  const saved = page.waitForResponse((r) => r.url().includes('/ui-preferences') && r.request().method() === 'PUT')
  await page.getByLabel(label, { exact: true }).selectOption(value)
  await saved
}

test.describe.configure({ mode: 'serial' })

test('saved defaults persist across reloads and Reset to Auto removes them', async ({ page, open }) => {
  await open('/settings')
  await page.getByLabel('Estrategia', { exact: true }).selectOption('fast')
  await page.getByRole('button', { name: 'Guardar valores' }).click()
  await expect(page.getByText('Valores guardados.')).toBeVisible()
  await page.reload()
  await expect(page.getByLabel('Estrategia', { exact: true })).toHaveValue('fast')
  await page.goto(page.url().split('#')[0] + '#/')
  await page.getByLabel('Tu pregunta').fill('¿Cómo se aplican mis valores?')
  await page.getByRole('button', { name: /Ver plan/ }).click()
  await expect(page.locator('.preview')).toBeVisible()
  await page.goto(page.url().split('#')[0] + '#/settings')
  await page.getByRole('button', { name: /Volver a Auto: Estrategia/ }).click()
  await page.getByRole('button', { name: 'Guardar valores' }).click()
  await page.reload()
  await expect(page.getByLabel('Estrategia', { exact: true })).toHaveValue('')
})

test('the settings form lists every consultation setting', async ({ page, open }) => {
  await open('/settings')
  for (const label of ['Modo', 'Estrategia', 'Coordinador', 'Rondas de revisión', 'Profundidad de investigación', 'Búsqueda web', 'Extensión', 'Tiempo por agente (s)', 'Máximo de llamadas', 'Memoria', 'Presupuesto de contexto', 'Vista', 'Máximo de fuentes', 'Frescura de fuentes (días)', 'Duración máxima (s)', 'Llamadas en paralelo']) {
    await expect(page.getByLabel(label, { exact: true })).toBeVisible()
  }
  await expect(page.getByText('Agentes participantes')).toBeVisible()
})

test('theme and language persist through the server', async ({ page, open }) => {
  await open('/settings')
  await page.getByRole('tab', { name: 'Apariencia' }).click()
  await choose(page, 'Tema', 'light')
  await expect(page.locator('html')).toHaveAttribute('data-theme', 'light')
  await choose(page, 'Idioma', 'en')
  await expect(page.getByRole('heading', { name: 'Settings' })).toBeVisible()
  await page.reload()
  await expect(page.locator('html')).toHaveAttribute('data-theme', 'light')
  await expect(page.getByRole('heading', { name: 'Settings' })).toBeVisible()
  await page.getByRole('tab', { name: 'Appearance' }).click()
  await choose(page, 'Theme', 'dark')
  await choose(page, 'Language', 'es')
  await expect(page.getByRole('heading', { name: 'Ajustes' })).toBeVisible()
})

test('providers show their version and never claim to know the login state', async ({ page, open }) => {
  await open('/settings')
  await page.getByRole('tab', { name: 'Agentes' }).click()
  await expect(page.getByText('9.9.9 (Fake Claude)')).toBeVisible()
  await expect(page.getByText('codex-cli 9.9.9')).toBeVisible()
  await expect(page.getByText('Desconocido').first()).toBeVisible()
  await page.getByRole('tab', { name: 'Seguridad' }).click()
  await expect(page.getByText('Solo escucha en 127.0.0.1')).toBeVisible()
})

test('memory is created only after an explicit confirmation, and can be corrected and deleted', async ({ page, open }) => {
  await open('/memory')
  await page.getByRole('button', { name: /Añadir un recuerdo/ }).click()
  await page.getByLabel('Texto', { exact: true }).fill('Mi presupuesto es de 8000 MXN')
  const save = page.getByRole('button', { name: 'Guardar recuerdo' })
  await expect(save).toBeDisabled()
  await page.getByLabel(/Confirmo que quiero/).check()
  await expect(save).toBeEnabled()
  await save.click()
  await expect(page.getByText('Recuerdo guardado.')).toBeVisible()
  const card = page.locator('.hist-item', { hasText: 'Mi presupuesto es de 8000 MXN' })
  await expect(card).toBeVisible()
  await expect(card).toContainText('project')
  await card.getByRole('button', { name: 'Corregir' }).click()
  await page.getByRole('dialog').getByLabel('Texto').fill('Mi presupuesto es de 9000 MXN')
  await page.getByRole('dialog').getByRole('button', { name: 'Corregir' }).click()
  await expect(page.getByText('Recuerdo corregido.')).toBeVisible()
  const corrected = page.locator('.hist-item', { hasText: '9000 MXN' })
  await expect(corrected).toBeVisible()
  await corrected.getByRole('button', { name: /Eliminar/ }).click()
  await page.getByRole('dialog').getByRole('button', { name: 'Eliminar' }).click()
  await expect(page.getByText('Recuerdo eliminado.')).toBeVisible()
  await expect(page.locator('.hist-item', { hasText: '9000 MXN' })).toHaveCount(0)
})
