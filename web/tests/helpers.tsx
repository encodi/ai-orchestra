import { render } from '@testing-library/react'
import type { ReactElement } from 'react'
import { I18nProvider, type Lang } from '../src/i18n'
import { PrefsProvider } from '../src/state/prefs'
import { CharactersProvider } from '../src/state/characters'
import { StoreProvider } from '../src/state/store'

/** The providers the application mounts, with the API client mocked by each test. */
export function Providers({ children }: { children: ReactElement }) {
  return (
    <PrefsProvider>
      <StoreProvider>
        <CharactersProvider>{children}</CharactersProvider>
      </StoreProvider>
    </PrefsProvider>
  )
}

export function renderWithApp(ui: ReactElement) {
  return render(<Providers>{ui}</Providers>)
}

export function renderI18n(ui: ReactElement, lang: Lang = 'es') {
  return render(<I18nProvider lang={lang}>{ui}</I18nProvider>)
}

/** A stand-in for the browser's EventSource that tests drive by hand. */
export class FakeEventSource {
  static instances: FakeEventSource[] = []
  url: string
  readyState = 0
  onopen: (() => void) | null = null
  onerror: (() => void) | null = null
  listeners = new Map<string, ((e: MessageEvent) => void)[]>()
  closed = false
  static CLOSED = 2
  constructor(url: string) {
    this.url = url
    FakeEventSource.instances.push(this)
  }
  addEventListener(kind: string, fn: (e: MessageEvent) => void) {
    this.listeners.set(kind, [...(this.listeners.get(kind) ?? []), fn])
  }
  emit(kind: string, payload: unknown) {
    for (const fn of this.listeners.get(kind) ?? []) fn({ data: JSON.stringify(payload) } as MessageEvent)
  }
  close() {
    this.closed = true
    this.readyState = 2
  }
}
