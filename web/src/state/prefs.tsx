import { createContext, useCallback, useContext, useEffect, useMemo, useState, type ReactNode } from 'react'
import { api } from '../api/client'
import type { UiPrefs } from '../api/types'
import { I18nProvider } from '../i18n'

export const DEFAULT_PREFS: UiPrefs = { theme: 'dark', language: 'es', reduced_motion: 'system', theater_default: false, show_stage: true }

type PrefsCtx = { prefs: UiPrefs; update: (patch: Partial<UiPrefs>) => Promise<string | null>; loaded: boolean; resolvedTheme: 'dark' | 'light' }
const Ctx = createContext<PrefsCtx>({ prefs: DEFAULT_PREFS, update: async () => null, loaded: false, resolvedTheme: 'dark' })

function systemTheme(): 'dark' | 'light' {
  try {
    return window.matchMedia('(prefers-color-scheme: light)').matches ? 'light' : 'dark'
  } catch {
    return 'dark'
  }
}

export function PrefsProvider({ children }: { children: ReactNode }) {
  const [prefs, setPrefs] = useState<UiPrefs>(DEFAULT_PREFS)
  const [loaded, setLoaded] = useState(false)
  const [sys, setSys] = useState<'dark' | 'light'>(systemTheme())

  useEffect(() => {
    let alive = true
    api
      .getUi()
      .then((r) => alive && setPrefs({ ...DEFAULT_PREFS, ...r.preferences }))
      .catch(() => undefined)
      .finally(() => alive && setLoaded(true))
    return () => {
      alive = false
    }
  }, [])

  useEffect(() => {
    let mq: MediaQueryList | null = null
    try {
      mq = window.matchMedia('(prefers-color-scheme: light)')
    } catch {
      return
    }
    const on = () => setSys(mq?.matches ? 'light' : 'dark')
    mq.addEventListener('change', on)
    return () => mq?.removeEventListener('change', on)
  }, [])

  const resolvedTheme = prefs.theme === 'system' ? sys : prefs.theme
  useEffect(() => {
    const root = document.documentElement
    root.dataset.theme = resolvedTheme
    root.lang = prefs.language
    if (prefs.reduced_motion === 'on') root.dataset.motion = 'reduced'
    else if (prefs.reduced_motion === 'off') root.dataset.motion = 'full'
    else delete root.dataset.motion
  }, [resolvedTheme, prefs.language, prefs.reduced_motion])

  const update = useCallback(
    async (patch: Partial<UiPrefs>) => {
      const next = { ...prefs, ...patch }
      setPrefs(next) // applied at once; the server confirms (or we report that it could not be saved)
      try {
        await api.putUi(next)
        return null
      } catch (e) {
        return e instanceof Error ? e.message : String(e)
      }
    },
    [prefs],
  )

  const value = useMemo(() => ({ prefs, update, loaded, resolvedTheme }), [prefs, update, loaded, resolvedTheme])
  return (
    <Ctx.Provider value={value}>
      <I18nProvider lang={prefs.language}>{children}</I18nProvider>
    </Ctx.Provider>
  )
}

export const usePrefs = () => useContext(Ctx)
