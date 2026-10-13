// App-wide state: the settings schema, this browser session's overrides and the conversation's turns.
// The overrides and turns live in sessionStorage so a refresh keeps them; nothing here ever starts a
// consultation (only an explicit "send" does).
import { createContext, useCallback, useContext, useEffect, useMemo, useState, type ReactNode } from 'react'
import { api } from '../api/client'
import type { Capabilities, SettingsObj } from '../api/types'

export type Turn = {
  id: string
  question: string
  jobId: string
  consultationId?: string
  startedMs: number
  planText?: string
  /** What was sent for this question only (shown in its details). */
  requestSettings: SettingsObj
}

type Stored = { sessionSettings: SettingsObj; sessionId: string | null; turns: Turn[]; sessionPrefsOpen?: boolean }
const KEY = 'aio-state-v1'

function load(): Stored {
  try {
    const raw = sessionStorage.getItem(KEY)
    if (raw) {
      const p = JSON.parse(raw) as Stored
      return { sessionSettings: p.sessionSettings ?? {}, sessionId: p.sessionId ?? null, turns: Array.isArray(p.turns) ? p.turns.slice(-30) : [] }
    }
  } catch {
    /* storage blocked: start empty */
  }
  return { sessionSettings: {}, sessionId: null, turns: [] }
}

type Store = {
  caps: Capabilities | null
  capsError: string | null
  sessionSettings: SettingsObj
  setSessionSettings: (s: SettingsObj) => void
  sessionId: string | null
  setSessionId: (id: string | null) => void
  turns: Turn[]
  addTurn: (t: Turn) => void
  patchTurn: (id: string, patch: Partial<Turn>) => void
  newConversation: () => void
  reloadCaps: () => void
}

const Ctx = createContext<Store | null>(null)

export function StoreProvider({ children }: { children: ReactNode }) {
  const [initial] = useState(load)
  const [caps, setCaps] = useState<Capabilities | null>(null)
  const [capsError, setCapsError] = useState<string | null>(null)
  const [sessionSettings, setSessionSettings] = useState<SettingsObj>(initial.sessionSettings)
  const [sessionId, setSessionId] = useState<string | null>(initial.sessionId)
  const [turns, setTurns] = useState<Turn[]>(initial.turns)
  const [nonce, setNonce] = useState(0)

  useEffect(() => {
    let alive = true
    api
      .capabilities()
      .then((c) => alive && (setCaps(c), setCapsError(null)))
      .catch((e: unknown) => alive && setCapsError(e instanceof Error ? e.message : String(e)))
    return () => {
      alive = false
    }
  }, [nonce])

  useEffect(() => {
    try {
      sessionStorage.setItem(KEY, JSON.stringify({ sessionSettings, sessionId, turns } satisfies Stored))
    } catch {
      /* ignore */
    }
  }, [sessionSettings, sessionId, turns])

  const addTurn = useCallback((t: Turn) => setTurns((x) => [...x, t].slice(-30)), [])
  const patchTurn = useCallback((id: string, patch: Partial<Turn>) => setTurns((x) => x.map((t) => (t.id === id ? { ...t, ...patch } : t))), [])
  const newConversation = useCallback(() => {
    setTurns([])
    setSessionId(null)
  }, [])
  const reloadCaps = useCallback(() => setNonce((n) => n + 1), [])

  const value = useMemo<Store>(
    () => ({ caps, capsError, sessionSettings, setSessionSettings, sessionId, setSessionId, turns, addTurn, patchTurn, newConversation, reloadCaps }),
    [caps, capsError, sessionSettings, sessionId, turns, addTurn, patchTurn, newConversation, reloadCaps],
  )
  return <Ctx.Provider value={value}>{children}</Ctx.Provider>
}

export function useStore(): Store {
  const v = useContext(Ctx)
  if (!v) throw new Error('StoreProvider is missing')
  return v
}
