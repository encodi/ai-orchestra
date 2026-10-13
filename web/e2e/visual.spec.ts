// Screenshots (desktop and mobile, dark and light) and automated checks: no horizontal scroll and no serious
// accessibility violations (axe, including colour contrast) on every screen. The images go to docs/screenshots.
import AxeBuilder from '@axe-core/playwright'
import { mkdirSync } from 'node:fs'
import { dirname, join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'
import type { Page } from '@playwright/test'
import { ask, expect, test } from './fixtures'

const out = resolve(dirname(fileURLToPath(import.meta.url)), '..', '..', 'docs', 'screenshots')
mkdirSync(out, { recursive: true })
const ID = '20260801-120000-000001'

const PAGES: { name: string; hash: string; ready: (p: Page) => Promise<void> }[] = [
  { name: 'chat-empty', hash: '/', ready: async (p) => void (await expect(p.getByText('Pregunta una vez')).toBeVisible()) },
  { name: 'history', hash: '/history', ready: async (p) => void (await expect(p.locator('.hist-item').first()).toBeVisible()) },
  { name: 'research', hash: `/research/${ID}`, ready: async (p) => void (await expect(p.getByText('Contradicha', { exact: true })).toBeVisible()) },
  { name: 'theater-record', hash: `/theater/${ID}`, ready: async (p) => void (await expect(p.locator('.bubble').first()).toBeVisible()) },
  { name: 'reports', hash: '/reports', ready: async (p) => void (await expect(p.locator('main h1')).toBeVisible()) },
  { name: 'memory', hash: '/memory', ready: async (p) => void (await expect(p.locator('main h1')).toBeVisible()) },
  { name: 'settings', hash: '/settings', ready: async (p) => void (await expect(p.getByLabel('Estrategia', { exact: true })).toBeVisible()) },
]

const serious = async (page: Page) => {
  const results = await new AxeBuilder({ page }).withTags(['wcag2a', 'wcag2aa', 'wcag21aa']).analyze()
  return results.violations.filter((v) => v.impact === 'serious' || v.impact === 'critical').map((v) => `${v.id}: ${v.nodes.map((n) => n.target.join(' ')).slice(0, 3).join(' | ')}`)
}

const setTheme = (page: Page, theme: 'dark' | 'light') =>
  page.evaluate(async (t) => {
    const token = document.querySelector('meta[name="aio-token"]')?.getAttribute('content') ?? ''
    await fetch('/api/v1/ui-preferences', { method: 'PUT', headers: { 'content-type': 'application/json', 'x-aio-token': token }, body: JSON.stringify({ theme: t, language: 'es', reduced_motion: 'on', theater_default: false }) })
  }, theme)

for (const theme of ['dark', 'light'] as const) {
  test(`every screen in ${theme}: screenshot, no horizontal scroll, no serious accessibility violations`, async ({ page, open, server }, info) => {
    await page.emulateMedia({ reducedMotion: 'reduce' })
    await open()
    await setTheme(page, theme)
    for (const p of PAGES) {
      await page.goto(`${server.url}/#${p.hash}`)
      await page.reload()
      await p.ready(page)
      await page.waitForTimeout(250)
      const overflow = await page.evaluate(() => document.documentElement.scrollWidth - window.innerWidth)
      expect(overflow, `${p.name}: horizontal overflow of ${overflow}px`).toBeLessThanOrEqual(1)
      await page.screenshot({ path: join(out, `${info.project.name}-${theme}-${p.name}.png`) })
      expect(await serious(page), `${p.name} (${theme})`).toEqual([])
    }
  })
}

test('a consultation in progress, its result and its theater at this size', async ({ page, open }, info) => {
  await page.emulateMedia({ reducedMotion: 'reduce' })
  await open()
  await setTheme(page, 'dark')
  await page.reload()
  await ask(page, 'Compara tres osciloscopios para aprender electrónica', { strategy: 'thorough' })
  await expect(page.locator('.progress')).toBeVisible()
  await page.waitForTimeout(900)
  await page.screenshot({ path: join(out, `${info.project.name}-dark-chat-progress.png`) })
  await expect(page.locator('.result')).toBeVisible({ timeout: 60_000 })
  await page.waitForTimeout(300)
  await page.screenshot({ path: join(out, `${info.project.name}-dark-chat-result.png`) })
  expect(await page.evaluate(() => document.documentElement.scrollWidth - window.innerWidth)).toBeLessThanOrEqual(1)
  expect(await serious(page)).toEqual([])
  await page.getByRole('link', { name: 'Ver en el Teatro' }).click()
  await expect(page.locator('.bubble').first()).toBeVisible()
  await page.waitForTimeout(300)
  await page.screenshot({ path: join(out, `${info.project.name}-dark-theater.png`) })
})
