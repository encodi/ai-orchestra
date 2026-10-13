import { expect, test } from './fixtures'

const ID = '20260801-120000-000001'

test('the research screen separates retrieved sources from candidates and shows claim evidence', async ({ page, open }) => {
  await open(`/research/${ID}`)
  await expect(page.getByText('2 fuente(s) descargada(s)')).toBeVisible()
  await expect(page.getByText('1 candidata(s) no descargada(s)')).toBeVisible()
  await expect(page.getByText('Solo una página que AI Orchestra descargó cuenta como fuente')).toBeVisible()
  await expect(page.getByText('Contradicha', { exact: true })).toBeVisible()
  await expect(page.getByText('Apoyada', { exact: true })).toBeVisible()
  const vendor = page.getByRole('link', { name: /vendor\.example/ })
  await expect(vendor).toHaveAttribute('href', 'https://vendor.example/model-x/datasheet')
  await expect(vendor).toHaveAttribute('rel', 'noopener noreferrer')
  await expect(vendor).toHaveAttribute('target', '_blank')
  await page.getByRole('button', { name: /Candidatas sin descargar/ }).click()
  await expect(page.getByText('No son evidencia')).toBeVisible()
  await page.getByRole('button', { name: /Evidencia \(2\)/ }).first().click()
  await expect(page.getByText('Esta lectura la hace un modelo', { exact: false }).first()).toBeVisible()
  await page.getByRole('button', { name: 'Ir a la fuente 2' }).click()
  await expect(page.locator('#source-2')).toHaveClass(/focused/)
})

test('a verified citation in a saved answer opens its source', async ({ page, open }) => {
  await open(`/history/${ID}`)
  await expect(page.getByText(/Esta consulta tiene \d+ días/)).toBeVisible()
  const cite = page.getByRole('button', { name: 'Source 1' }).first()
  await expect(cite).toBeVisible()
  await cite.click()
  await expect(page.locator('#source-1')).toHaveClass(/focused/)
  await expect(page.getByText('1 candidata(s) no descargada(s)')).toBeVisible()
})

test('the theater shows a recorded timeout as a failure', async ({ page, open }) => {
  await open(`/theater/${ID}`)
  await expect(page.locator('.bubble-fail')).toHaveCount(1)
  await expect(page.locator('.bubble-fail')).toContainText('timed out after 1s')
  await expect(page.locator('.bubble-fail')).toContainText('no se inventa ninguna')
  await expect(page.getByText('El coordinador dejó desacuerdos sin resolver.')).toBeVisible()
  await expect(page.locator('.bubble')).toHaveCount(4)
})
