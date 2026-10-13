import { lazy, Suspense, useEffect, useRef, useState } from 'react'
import { Icon, type IconName } from './components/Icons'
import { Dialog, Spinner } from './components/ui'
import { useI18n, type TKey } from './i18n'
import { navigate, useRoute } from './lib/router'
import { ChatPage } from './pages/Chat'

// Everything but the chat loads on demand, so the first screen stays small.
const HistoryPage = lazy(() => import('./pages/History').then((m) => ({ default: m.HistoryPage })))
const HistoryDetailPage = lazy(() => import('./pages/History').then((m) => ({ default: m.HistoryDetailPage })))
const MemoryPage = lazy(() => import('./pages/Memory').then((m) => ({ default: m.MemoryPage })))
const ReportsPage = lazy(() => import('./pages/Reports').then((m) => ({ default: m.ReportsPage })))
const ResearchPage = lazy(() => import('./pages/Research').then((m) => ({ default: m.ResearchPage })))
const SettingsPage = lazy(() => import('./pages/Settings').then((m) => ({ default: m.SettingsPage })))
const TheaterPage = lazy(() => import('./pages/Theater').then((m) => ({ default: m.TheaterPage })))
import { usePrefs } from './state/prefs'
import { useStore } from './state/store'

const NAV: { path: string; key: TKey; icon: IconName; root: string }[] = [
  { path: '/', key: 'nav.chat', icon: 'chat', root: '' },
  { path: '/theater', key: 'nav.theater', icon: 'theater', root: 'theater' },
  { path: '/history', key: 'nav.history', icon: 'history', root: 'history' },
  { path: '/research', key: 'nav.research', icon: 'research', root: 'research' },
  { path: '/reports', key: 'nav.reports', icon: 'reports', root: 'reports' },
  { path: '/memory', key: 'nav.memory', icon: 'memory', root: 'memory' },
  { path: '/settings', key: 'nav.settings', icon: 'settings', root: 'settings' },
]

const SHORTCUTS: [string, TKey][] = [
  ['Ctrl/⌘ + Enter', 'kb.send'], ['/', 'kb.focus'], ['Alt + 1…7', 'kb.nav'], ['Alt + N', 'kb.new'], ['?', 'kb.help'], ['Esc', 'kb.close'],
]

export default function App() {
  const { t, lang } = useI18n()
  const route = useRoute()
  const { prefs, update, resolvedTheme } = usePrefs()
  const { caps, newConversation } = useStore()
  const [help, setHelp] = useState(false)
  const main = useRef<HTMLElement>(null)
  const root = route.parts[0] ?? ''

  useEffect(() => {
    main.current?.focus({ preventScroll: true })
  }, [route.path])

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const tag = (e.target as HTMLElement | null)?.tagName
      const typing = tag === 'INPUT' || tag === 'TEXTAREA' || tag === 'SELECT'
      if (e.altKey && /^Digit[1-7]$/.test(e.code)) {
        e.preventDefault()
        navigate(NAV[Number(e.code.slice(5)) - 1]?.path ?? '/')
      } else if (e.altKey && e.code === 'KeyN') {
        e.preventDefault()
        newConversation()
        navigate('/')
      } else if (e.key === '?' && !typing) {
        e.preventDefault()
        setHelp(true)
      }
    }
    document.addEventListener('keydown', onKey)
    return () => document.removeEventListener('keydown', onKey)
  }, [newConversation])

  let page
  switch (root) {
    case 'theater': page = <TheaterPage id={route.parts[1]} />; break
    case 'history': page = route.parts[1] ? <HistoryDetailPage id={route.parts[1]} /> : <HistoryPage />; break
    case 'research': page = <ResearchPage id={route.parts[1]} />; break
    case 'reports': page = <ReportsPage />; break
    case 'memory': page = <MemoryPage />; break
    case 'settings': page = <SettingsPage />; break
    default: page = <ChatPage />
  }

  return (
    <div className="app">
      <a href="#main" className="skip-link" onClick={(e) => { e.preventDefault(); main.current?.focus() }}>{t('skip')}</a>
      <aside className="sidebar">
        <div className="brand">
          <span className="brand-mark" aria-hidden="true">
            <svg width="22" height="22" viewBox="0 0 14 14" shapeRendering="crispEdges" aria-hidden="true">
              <rect x="1" y="9" width="3" height="4" fill="var(--claude)" /><rect x="5.5" y="5" width="3" height="8" fill="var(--codex)" /><rect x="10" y="1" width="3" height="12" fill="var(--google)" />
            </svg>
          </span>
          <span className="brand-name">AI Orchestra</span>
          <span className="badge-local" title={t('local_title')}>{t('local')}</span>
        </div>
        <nav className="nav" aria-label={t('nav.label')}>
          {NAV.map((n, i) => (
            <a key={n.path} href={`#${n.path}`} aria-current={root === n.root ? 'page' : undefined} title={`Alt+${i + 1}`}>
              <Icon name={n.icon} />
              <span>{t(n.key)}</span>
            </a>
          ))}
        </nav>
        <div className="sidebar-foot">
          <div className="row" style={{ gap: 6 }}>
            <button type="button" className="btn ghost sm icon-btn" onClick={() => update({ theme: resolvedTheme === 'dark' ? 'light' : 'dark' })} aria-label={t('toggle_theme')} title={t('toggle_theme')}>
              <Icon name={resolvedTheme === 'dark' ? 'sun' : 'moon'} />
            </button>
            <button type="button" className="btn ghost sm" onClick={() => update({ language: prefs.language === 'es' ? 'en' : 'es' })} aria-label={t('toggle_lang')}>
              {lang === 'es' ? 'EN' : 'ES'}
            </button>
            <button type="button" className="btn ghost sm" onClick={() => setHelp(true)} aria-label={t('kb.title')}>?</button>
          </div>
          <span>v{caps?.version ?? '…'} · 127.0.0.1</span>
        </div>
      </aside>
      <main id="main" className="main" ref={main} tabIndex={-1}>
        <Suspense fallback={<div className="page"><Spinner label={t('loading')} /></div>}>{page}</Suspense>
      </main>
      {help && (
        <Dialog title={t('kb.title')} confirmLabel={t('close')} cancelLabel={t('close')} onConfirm={() => setHelp(false)} onCancel={() => setHelp(false)}>
          <table className="table"><tbody>{SHORTCUTS.map(([k, d]) => <tr key={k}><td><kbd>{k}</kbd></td><td>{t(d)}</td></tr>)}</tbody></table>
        </Dialog>
      )}
    </div>
  )
}
