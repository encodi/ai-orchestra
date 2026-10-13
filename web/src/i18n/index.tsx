import { createContext, useCallback, useContext, useMemo, type ReactNode } from 'react'
import { es } from './es'
import { en } from './en'

export type Lang = 'es' | 'en'
export type TKey = keyof typeof es
export type TFn = (key: TKey, vars?: Record<string, string | number>) => string

const dicts: Record<Lang, Record<TKey, string>> = { es, en }

export function translate(lang: Lang, key: TKey, vars?: Record<string, string | number>): string {
  const raw = dicts[lang][key] ?? es[key] ?? key
  return vars ? raw.replace(/\{(\w+)\}/g, (_, k: string) => String(vars[k] ?? `{${k}}`)) : raw
}

const Ctx = createContext<{ lang: Lang; t: TFn }>({ lang: 'es', t: (k, v) => translate('es', k, v) })

export function I18nProvider({ lang, children }: { lang: Lang; children: ReactNode }) {
  const t = useCallback<TFn>((key, vars) => translate(lang, key, vars), [lang])
  const value = useMemo(() => ({ lang, t }), [lang, t])
  return <Ctx.Provider value={value}>{children}</Ctx.Provider>
}

export const useI18n = () => useContext(Ctx)
