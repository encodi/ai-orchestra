// The agents' characters: the name, the look and the profile the user gave each of them. The server stores them
// (and writes the name and profile into the prompts); the interface uses them to name and draw every agent.
import { createContext, useCallback, useContext, useEffect, useMemo, useState, type ReactNode } from 'react'
import { api } from '../api/client'
import type { Appearance, CharacterEntry, CharactersFile, CharactersResponse } from '../api/types'

export const DEFAULT_FILE: CharactersFile = { reply_language: 'auto', conversational: true, characters: {} }
const NO_APPEARANCE: Appearance = { palette: 0, accessory: 'none' }

type Ctx = {
  file: CharactersFile
  limits: CharactersResponse['limits'] | null
  note: string
  loaded: boolean
  /** The character's name, or `fallback` when the user gave none. */
  nameOf: (id: string, fallback: string) => string
  appearanceOf: (id: string) => Appearance
  entryOf: (id: string) => CharacterEntry | undefined
  save: (file: CharactersFile) => Promise<string | null>
}

const C = createContext<Ctx>({
  file: DEFAULT_FILE, limits: null, note: '', loaded: false,
  nameOf: (_id, fb) => fb, appearanceOf: () => NO_APPEARANCE, entryOf: () => undefined, save: async () => null,
})

export function CharactersProvider({ children }: { children: ReactNode }) {
  const [file, setFile] = useState<CharactersFile>(DEFAULT_FILE)
  const [limits, setLimits] = useState<CharactersResponse['limits'] | null>(null)
  const [note, setNote] = useState('')
  const [loaded, setLoaded] = useState(false)

  useEffect(() => {
    let alive = true
    api
      .characters()
      .then((r) => {
        if (!alive) return
        setFile({ ...DEFAULT_FILE, ...r.settings })
        setLimits(r.limits)
        setNote(r.note)
      })
      .catch(() => undefined)
      .finally(() => alive && setLoaded(true))
    return () => {
      alive = false
    }
  }, [])

  const save = useCallback(async (next: CharactersFile) => {
    try {
      const r = await api.putCharacters(next)
      setFile({ ...DEFAULT_FILE, ...r.settings })
      return null
    } catch (e) {
      return e instanceof Error ? e.message : String(e)
    }
  }, [])

  const value = useMemo<Ctx>(
    () => ({
      file, limits, note, loaded, save,
      nameOf: (id, fb) => file.characters[id]?.name?.trim() || fb,
      appearanceOf: (id) => file.characters[id]?.appearance ?? NO_APPEARANCE,
      entryOf: (id) => file.characters[id],
    }),
    [file, limits, note, loaded, save],
  )
  return <C.Provider value={value}>{children}</C.Provider>
}

export const useCharacters = () => useContext(C)
