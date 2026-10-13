import { useEffect, useId, useRef, useState, type CSSProperties, type ReactNode } from 'react'
import { Icon, type IconName } from './Icons'

export function Pill({ tone = 'neutral', children, title }: { tone?: 'neutral' | 'ok' | 'warn' | 'danger' | 'info' | 'accent'; children: ReactNode; title?: string }) {
  return (
    <span className={`pill${tone === 'neutral' ? '' : ` ${tone}`}`} title={title}>
      {children}
    </span>
  )
}

const NOTICE_ICON: Record<string, IconName> = { info: 'info', warn: 'alert', danger: 'alert', ok: 'check' }
export function Notice({ tone = 'info', children, role }: { tone?: 'info' | 'warn' | 'danger' | 'ok'; children: ReactNode; role?: 'alert' | 'status' }) {
  return (
    <div className={`notice ${tone}`} role={role ?? (tone === 'danger' ? 'alert' : undefined)}>
      <Icon name={NOTICE_ICON[tone] ?? 'info'} />
      <div style={{ minWidth: 0, flex: 1 }}>{children}</div>
    </div>
  )
}

export function statusTone(status: string): 'ok' | 'warn' | 'danger' | 'info' | 'neutral' {
  switch (status) {
    case 'complete':
    case 'completed':
      return 'ok'
    case 'partial':
      return 'warn'
    case 'failed':
      return 'danger'
    case 'cancelled':
      return 'neutral'
    case 'running':
      return 'info'
    default:
      return 'neutral'
  }
}

export function Spinner({ label }: { label: string }) {
  return (
    <span role="status" className="muted row" style={{ gap: 8 }}>
      <span className="spin" aria-hidden="true" />
      {label}
    </span>
  )
}

export type TabDef<T extends string> = { id: T; label: string; badge?: string | number }
export function Tabs<T extends string>({ tabs, value, onChange, label }: { tabs: TabDef<T>[]; value: T; onChange: (t: T) => void; label: string }) {
  const refs = useRef<Record<string, HTMLButtonElement | null>>({})
  const move = (dir: number) => {
    const i = tabs.findIndex((t) => t.id === value)
    const next = tabs[(i + dir + tabs.length) % tabs.length]
    if (next) {
      onChange(next.id)
      refs.current[next.id]?.focus()
    }
  }
  return (
    <div className="tabs" role="tablist" aria-label={label}>
      {tabs.map((t) => (
        <button
          key={t.id}
          ref={(el) => {
            refs.current[t.id] = el
          }}
          role="tab"
          type="button"
          id={`tab-${t.id}`}
          aria-selected={value === t.id}
          aria-controls={`panel-${t.id}`}
          tabIndex={value === t.id ? 0 : -1}
          onClick={() => onChange(t.id)}
          onKeyDown={(e) => {
            if (e.key === 'ArrowRight') {
              e.preventDefault()
              move(1)
            } else if (e.key === 'ArrowLeft') {
              e.preventDefault()
              move(-1)
            }
          }}
        >
          {t.label}
          {t.badge !== undefined && <span className="faint"> · {t.badge}</span>}
        </button>
      ))}
    </div>
  )
}

export function TabPanel({ id, children }: { id: string; children: ReactNode }) {
  return (
    <div role="tabpanel" id={`panel-${id}`} aria-labelledby={`tab-${id}`}>
      {children}
    </div>
  )
}

export function Collapsible({ title, children, defaultOpen = false, meta }: { title: ReactNode; children: ReactNode; defaultOpen?: boolean; meta?: ReactNode }) {
  const [open, setOpen] = useState(defaultOpen)
  const id = useId()
  return (
    <div className="collapsible">
      <button type="button" className="collapsible-head" aria-expanded={open} aria-controls={id} onClick={() => setOpen(!open)}>
        <span className={`chev${open ? ' open' : ''}`}>
          <Icon name="chevron" size={16} />
        </span>
        <span className="collapsible-title">{title}</span>
        {meta && <span className="collapsible-meta">{meta}</span>}
      </button>
      {open && (
        <div id={id} className="collapsible-body">
          {children}
        </div>
      )}
    </div>
  )
}

export function Dialog({
  title, children, confirmLabel, cancelLabel, onConfirm, onCancel, tone = 'primary', busy,
}: {
  title: string
  children: ReactNode
  confirmLabel: string
  cancelLabel: string
  onConfirm: () => void
  onCancel: () => void
  tone?: 'primary' | 'danger'
  busy?: boolean
}) {
  const ref = useRef<HTMLDivElement>(null)
  const titleId = useId()
  useEffect(() => {
    const previous = document.activeElement as HTMLElement | null
    ref.current?.querySelector<HTMLElement>('[data-autofocus]')?.focus()
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') onCancel()
      if (e.key === 'Tab' && ref.current) {
        const f = Array.from(ref.current.querySelectorAll<HTMLElement>('button, input, select, textarea, [href]')).filter((x) => !x.hasAttribute('disabled'))
        const first = f[0]
        const last = f[f.length - 1]
        if (first && last) {
          if (e.shiftKey && document.activeElement === first) {
            e.preventDefault()
            last.focus()
          } else if (!e.shiftKey && document.activeElement === last) {
            e.preventDefault()
            first.focus()
          }
        }
      }
    }
    document.addEventListener('keydown', onKey)
    return () => {
      document.removeEventListener('keydown', onKey)
      previous?.focus()
    }
  }, [onCancel])
  return (
    <div className="dialog-backdrop" onMouseDown={(e) => e.target === e.currentTarget && onCancel()}>
      <div className="dialog" role="dialog" aria-modal="true" aria-labelledby={titleId} ref={ref}>
        <h2 id={titleId}>{title}</h2>
        <div>{children}</div>
        <div className="actions">
          <button type="button" className="btn" onClick={onCancel} data-autofocus>
            {cancelLabel}
          </button>
          <button type="button" className={`btn ${tone === 'danger' ? 'danger' : 'primary'}`} onClick={onConfirm} disabled={busy}>
            {confirmLabel}
          </button>
        </div>
      </div>
    </div>
  )
}

export function Empty({ title, children }: { title: string; children?: ReactNode }) {
  return (
    <div className="empty">
      <p style={{ fontWeight: 600, color: 'var(--text)' }}>{title}</p>
      {children}
    </div>
  )
}

/** A form field whose accessible name is its label text only (the control is not nested inside the label). */
export function Field({ label, children, style }: { label: ReactNode; children: (id: string) => ReactNode; style?: CSSProperties }) {
  const id = useId()
  return (
    <div className="field" style={style}>
      <label htmlFor={id}>{label}</label>
      {children(id)}
    </div>
  )
}
