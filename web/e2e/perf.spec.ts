// Measures the cost of the interface itself (not of any provider): page load, bytes transferred, the lag
// between the server recording an event and the browser receiving it, and the time to show a result.
// The numbers are printed; the assertions only guard against a gross regression.
import { ask, expect, test } from './fixtures'

test('interface overhead: load, transfer, event delivery lag, render', async ({ page, open, server }) => {
  const sizes: number[] = []
  page.on('response', async (r) => {
    if (r.url().startsWith(server.url) && !r.url().includes('/api/')) sizes.push((await r.body().catch(() => Buffer.alloc(0))).length)
  })
  await open()
  await expect(page.getByText('Pregunta una vez')).toBeVisible()
  const nav = await page.evaluate(() => {
    const n = performance.getEntriesByType('navigation')[0] as PerformanceNavigationTiming
    return { domContentLoadedMs: Math.round(n.domContentLoadedEventEnd), loadMs: Math.round(n.loadEventEnd), heapMB: Math.round(((performance as unknown as { memory?: { usedJSHeapSize: number } }).memory?.usedJSHeapSize ?? 0) / 1e5) / 10 }
  })

  await ask(page, 'Mide la latencia de la interfaz', { strategy: 'thorough' })
  await expect(page.locator('.progress')).toBeVisible()
  await expect(page.locator('.result')).toBeVisible({ timeout: 60_000 })
  const visibleAt = Date.now()

  // Delivery lag: replay the job's events over SSE and compare each event's server timestamp with its arrival.
  const jobs = await (await fetch(`${server.url}/api/v1/consultations`)).json()
  const id = jobs.items[0].id as string
  const finishedMs = (await (await fetch(`${server.url}/api/v1/consultations/${id}`)).json()).job?.finished_ms as number | undefined
  const renderAfterFinishMs = finishedMs ? visibleAt - finishedMs : null
  const lag = await page.evaluate(
    (cid) =>
      new Promise<{ n: number; maxMs: number }>((done) => {
        const es = new EventSource(`/api/v1/consultations/${cid}/events`)
        let n = 0
        es.addEventListener('consultation_completed', () => {
          es.close()
          done({ n, maxMs: 0 })
        })
        for (const k of ['provider_started', 'provider_completed', 'agent_message_recorded']) es.addEventListener(k, () => void n++)
      }),
    id,
  )
  const live = await page.evaluate(async () => {
    // A fresh consultation observed from the page itself, to measure live delivery.
    const token = document.querySelector('meta[name="aio-token"]')?.getAttribute('content') ?? ''
    const res = await fetch('/api/v1/consultations', { method: 'POST', headers: { 'content-type': 'application/json', 'x-aio-token': token }, body: JSON.stringify({ question: 'Latencia de eventos en vivo', settings: { strategy: 'thorough' } }) })
    const job = (await res.json()) as { job_id: string }
    return new Promise<{ n: number; maxLagMs: number; meanLagMs: number }>((done) => {
      const es = new EventSource(`/api/v1/consultations/${job.job_id}/events`)
      const lags: number[] = []
      const on = (e: MessageEvent) => {
        const rec = JSON.parse(e.data) as { ts_ms: number; kind: string }
        lags.push(Date.now() - rec.ts_ms)
        if (rec.kind === 'consultation_completed') {
          es.close()
          done({ n: lags.length, maxLagMs: Math.max(...lags), meanLagMs: Math.round(lags.reduce((a, b) => a + b, 0) / lags.length) })
        }
      }
      for (const k of ['planning_started', 'strategy_selected', 'consultation_created', 'provider_started', 'provider_completed', 'agent_message_recorded', 'research_note', 'memory_note', 'consultation_completed']) es.addEventListener(k, on as EventListener)
    })
  })
  const totalKB = Math.round(sizes.reduce((a, b) => a + b, 0) / 102.4) / 10
  console.log('PERF ' + JSON.stringify({ ...nav, transferredKB: totalKB, resultVisibleAfterServerFinishedMs: renderAfterFinishMs, replayedEvents: lag.n, live }))
  expect(nav.loadMs).toBeLessThan(2000)
  expect(live.maxLagMs).toBeLessThan(500)
})
