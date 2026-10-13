// Starts the real `ai-orchestra web` binary against fake agent CLIs (shell scripts) in a private HOME.
// No real model is ever contacted: the fakes only print canned answers (see fakes/body.sh).
import { spawn, type ChildProcess } from 'node:child_process'
import { copyFileSync, chmodSync, existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs'
import { createServer } from 'node:net'
import { tmpdir } from 'node:os'
import { dirname, join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

const here = dirname(fileURLToPath(import.meta.url))
const root = resolve(here, '..', '..')

export type Server = {
  url: string
  dir: string
  /** One line per call a fake agent received. */
  calls: () => string[]
  /** Every prompt the fake agents received, as text. */
  prompts: () => string
  stop: () => Promise<void>
  log: () => string
}

function freePort(): Promise<number> {
  return new Promise((ok, fail) => {
    const s = createServer()
    s.listen(0, '127.0.0.1', () => {
      const port = (s.address() as { port: number }).port
      s.close(() => ok(port))
    })
    s.on('error', fail)
  })
}

export async function startServer(options: { delay?: string } = {}): Promise<Server> {
  const bin = process.env.AIO_BIN ?? join(root, 'target', 'debug', 'ai-orchestra')
  if (!existsSync(bin)) throw new Error(`build the binary first (cargo build): ${bin} does not exist`)
  const dir = mkdtempSync(join(tmpdir(), 'aio-e2e-'))
  const bins = join(dir, 'bin')
  mkdirSync(bins)
  mkdirSync(join(dir, 'home'))
  const debates = join(dir, 'data', 'ai-orchestra', 'debates')
  mkdirSync(debates, { recursive: true })
  for (const f of ['claude', 'codex', 'agy', 'body.sh']) {
    copyFileSync(join(here, 'fakes', f), join(bins, f))
    chmodSync(join(bins, f), 0o755)
  }
  // A saved consultation with web research (its shape is checked by the Rust test that generates it).
  copyFileSync(join(here, 'fixtures', 'research-record.json'), join(debates, '20260801-120000-000001.json'))
  const port = await freePort()
  const callLog = join(dir, 'calls.log')
  const promptLog = join(dir, 'prompts.log')
  writeFileSync(callLog, '')
  writeFileSync(promptLog, '')
  writeFileSync(
    join(dir, 'config.toml'),
    `[general]\ntimeout_secs = 20\n[memory]\npath = "${join(dir, 'memory.db')}"\n[providers.claude]\nexecutable = "${join(bins, 'claude')}"\n[providers.codex]\nexecutable = "${join(bins, 'codex')}"\n[providers.google]\nexecutable = "${join(bins, 'agy')}"\n[web]\nport = ${port}\n`,
  )
  const env: NodeJS.ProcessEnv = { ...process.env, HOME: join(dir, 'home'), XDG_DATA_HOME: join(dir, 'data'), FAKE_LOG: callLog, FAKE_PROMPTS: promptLog, FAKE_DELAY: options.delay ?? '0.5' }
  for (const k of ['CLAUDECODE', 'AI_ORCHESTRA_ACTIVE', 'CODEX_THREAD_ID', 'CODEX_SESSION_ID', 'CODEX_SANDBOX', 'CODEX_SHELL', 'ANTHROPIC_API_KEY', 'OPENAI_API_KEY']) delete env[k]
  const child: ChildProcess = spawn(bin, ['--config', join(dir, 'config.toml'), 'web', '--port', String(port)], { env, stdio: ['ignore', 'pipe', 'pipe'] })
  let out = ''
  child.stdout?.on('data', (d) => (out += String(d)))
  child.stderr?.on('data', (d) => (out += String(d)))
  const url = `http://127.0.0.1:${port}`
  const deadline = Date.now() + 20_000
  for (;;) {
    try {
      const r = await fetch(`${url}/api/v1/health`)
      if (r.ok) break
    } catch {
      /* not up yet */
    }
    if (Date.now() > deadline) {
      child.kill('SIGKILL')
      throw new Error(`the server did not start:\n${out}`)
    }
    await new Promise((r) => setTimeout(r, 100))
  }
  return {
    url,
    dir,
    log: () => out,
    calls: () => readFileSync(callLog, 'utf8').split('\n').filter(Boolean),
    prompts: () => readFileSync(promptLog, 'utf8'),
    stop: async () => {
      child.kill('SIGINT')
      await new Promise<void>((done) => {
        const t = setTimeout(() => {
          child.kill('SIGKILL')
          done()
        }, 8000)
        child.on('exit', () => {
          clearTimeout(t)
          done()
        })
      })
      rmSync(dir, { recursive: true, force: true })
    },
  }
}
