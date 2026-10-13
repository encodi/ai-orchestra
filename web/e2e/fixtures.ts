import { test as base, expect, type Page } from '@playwright/test'
import { startServer, type Server } from './harness'

export const test = base.extend<{ app: Server; open: (hash?: string) => Promise<Page> }, { server: Server }>({
  server: [
    async ({}, use) => {
      const s = await startServer()
      await use(s)
      await s.stop()
    },
    { scope: 'worker' },
  ],
  app: async ({ server }, use) => use(server),
  open: async ({ page, server }, use) => {
    await use(async (hash = '') => {
      await page.goto(`${server.url}/${hash ? `#${hash}` : ''}`)
      await expect(page.locator('main')).toBeVisible()
      return page
    })
  },
})

export { expect }

/** Sends a question from the chat, optionally with per-question settings chosen in the advanced panel. */
export async function ask(page: Page, question: string, settings: Record<string, string> = {}): Promise<void> {
  if (Object.keys(settings).length) {
    await page.getByRole('button', { name: /Ajustes de esta consulta/ }).click()
    for (const [key, value] of Object.entries(settings)) {
      const field = page.locator(`#c-request-${key}`)
      if ((await field.count()) && (await field.evaluate((el) => el.tagName)) === 'SELECT') await field.selectOption(value)
      else if (await field.count()) await field.fill(value)
      else await page.locator('label.chip', { hasText: value }).click() // an agent chip
    }
  }
  await page.getByLabel('Tu pregunta').fill(question)
  await page.getByRole('button', { name: /^Enviar/ }).click()
}
