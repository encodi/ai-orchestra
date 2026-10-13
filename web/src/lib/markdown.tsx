// A small Markdown renderer that produces React elements, never HTML strings. Agent text is untrusted:
// raw HTML is shown as literal text, links are limited to http(s), and nothing is evaluated.
import type { ReactNode } from 'react'
import { translateHeading, ES_HEADINGS } from './headings'
import { useI18n } from '../i18n'
import { safeHttpUrl } from './format'

export type CiteHandler = (id: number) => void
export type MarkdownOptions = {
  /** Source ids that exist (retrieved). `[n]` becomes a clickable chip only for these. */
  knownSources?: Set<number>
  onCite?: CiteHandler
  /** Known English section headings mapped to the interface language (display only). */
  headings?: Record<string, string>
}

type Ctx = MarkdownOptions & { key: number }

function inline(text: string, ctx: Ctx): ReactNode[] {
  const out: ReactNode[] = []
  let rest = text
  const push = (n: ReactNode) => out.push(n)
  const k = () => `i${ctx.key++}`
  const re = /(`[^`\n]+`)|(\*\*[^*\n]+\*\*)|(__[^_\n]+__)|(\*[^*\s][^*\n]*\*)|(\[([^\]\n]+)\]\(([^)\s]+)\))|(\[(\d{1,3})\])/
  while (rest.length > 0) {
    const m = re.exec(rest)
    if (!m) {
      push(rest)
      break
    }
    if (m.index > 0) push(rest.slice(0, m.index))
    const tok = m[0]
    if (m[1]) push(<code key={k()}>{tok.slice(1, -1)}</code>)
    else if (m[2] || m[3]) push(<strong key={k()}>{inline(tok.slice(2, -2), ctx)}</strong>)
    else if (m[4]) push(<em key={k()}>{inline(tok.slice(1, -1), ctx)}</em>)
    else if (m[5]) {
      const href = safeHttpUrl(m[7] ?? '')
      if (href) {
        push(
          <a key={k()} href={href} target="_blank" rel="noopener noreferrer">
            {inline(m[6] ?? '', ctx)}
          </a>,
        )
      } else push(tok)
    } else if (m[8]) {
      const id = Number(m[9])
      if (ctx.knownSources?.has(id) && ctx.onCite) {
        push(
          <button key={k()} type="button" className="cite" onClick={() => ctx.onCite?.(id)} aria-label={`Source ${id}`}>
            {id}
          </button>,
        )
      } else push(<span key={k()} className="cite plain" title="Not a retrieved source">{id}</span>)
    }
    rest = rest.slice(m.index + tok.length)
  }
  return out
}

function splitRow(line: string): string[] {
  const t = line.trim().replace(/^\|/, '').replace(/\|$/, '')
  return t.split('|').map((c) => c.trim())
}

const isTableSep = (l: string) => /^\s*\|?\s*:?-{2,}:?\s*(\|\s*:?-{2,}:?\s*)*\|?\s*$/.test(l) && l.includes('-')

export function renderMarkdown(src: string, opts: MarkdownOptions = {}): ReactNode[] {
  const ctx: Ctx = { ...opts, key: 0 }
  const lines = src.replace(/\r\n?/g, '\n').split('\n')
  const out: ReactNode[] = []
  let i = 0
  const key = () => `b${ctx.key++}`
  while (i < lines.length) {
    const line = lines[i] ?? ''
    if (/^\s*$/.test(line)) {
      i++
      continue
    }
    const fence = /^\s*(```|~~~)\s*([\w+-]*)/.exec(line)
    if (fence) {
      const buf: string[] = []
      i++
      while (i < lines.length && !(lines[i] ?? '').trim().startsWith(fence[1] ?? '```')) buf.push(lines[i++] ?? '')
      i++
      out.push(
        <pre key={key()} tabIndex={0}>
          <code>{buf.join('\n')}</code>
        </pre>,
      )
      continue
    }
    const h = /^(#{1,4})\s+(.*?)\s*#*\s*$/.exec(line)
    if (h) {
      const level = (h[1] ?? '#').length
      const content = inline(translateHeading(h[2] ?? '', opts.headings), ctx)
      const Tag = (`h${level}`) as 'h1' | 'h2' | 'h3' | 'h4'
      out.push(<Tag key={key()}>{content}</Tag>)
      i++
      continue
    }
    if (/^\s*([-*_])(\s*\1){2,}\s*$/.test(line)) {
      out.push(<hr key={key()} />)
      i++
      continue
    }
    if (line.includes('|') && isTableSep(lines[i + 1] ?? '')) {
      const head = splitRow(line)
      i += 2
      const rows: string[][] = []
      while (i < lines.length && (lines[i] ?? '').includes('|') && !/^\s*$/.test(lines[i] ?? '')) rows.push(splitRow(lines[i++] ?? ''))
      out.push(
        <table key={key()}>
          <thead>
            <tr>
              {head.map((c, ci) => (
                <th key={ci}>{inline(c, ctx)}</th>
              ))}
            </tr>
          </thead>
          <tbody>
            {rows.map((r, ri) => (
              <tr key={ri}>
                {head.map((_, ci) => (
                  <td key={ci}>{inline(r[ci] ?? '', ctx)}</td>
                ))}
              </tr>
            ))}
          </tbody>
        </table>,
      )
      continue
    }
    if (/^\s*>/.test(line)) {
      const buf: string[] = []
      while (i < lines.length && /^\s*>/.test(lines[i] ?? '')) buf.push((lines[i++] ?? '').replace(/^\s*>\s?/, ''))
      out.push(<blockquote key={key()}>{renderMarkdown(buf.join('\n'), opts)}</blockquote>)
      continue
    }
    const li = /^(\s*)([-*+]|\d+[.)])\s+(.*)$/.exec(line)
    if (li) {
      const ordered = /\d/.test(li[2] ?? '')
      const baseIndent = (li[1] ?? '').length
      const items: ReactNode[] = []
      while (i < lines.length) {
        const m = /^(\s*)([-*+]|\d+[.)])\s+(.*)$/.exec(lines[i] ?? '')
        if (!m || (m[1] ?? '').length < baseIndent) break
        if ((m[1] ?? '').length > baseIndent) {
          // A nested list: render the deeper lines recursively and attach them to the previous item.
          const nested: string[] = []
          while (i < lines.length) {
            const mm = /^(\s*)([-*+]|\d+[.)])\s+/.exec(lines[i] ?? '')
            if (!mm || (mm[1] ?? '').length <= baseIndent) break
            nested.push((lines[i++] ?? '').slice(baseIndent + 1))
          }
          items.push(<li key={key()} style={{ listStyle: 'none' }}>{renderMarkdown(nested.join('\n'), opts)}</li>)
          continue
        }
        if ((/\d/.test(m[2] ?? '')) !== ordered) break
        items.push(<li key={key()}>{inline(m[3] ?? '', ctx)}</li>)
        i++
      }
      out.push(ordered ? <ol key={key()}>{items}</ol> : <ul key={key()}>{items}</ul>)
      continue
    }
    const buf: string[] = [line]
    i++
    while (
      i < lines.length &&
      !/^\s*$/.test(lines[i] ?? '') &&
      !/^\s*(```|~~~|#{1,4}\s|>|([-*+]|\d+[.)])\s)/.test(lines[i] ?? '') &&
      !((lines[i] ?? '').includes('|') && isTableSep(lines[i + 1] ?? ''))
    )
      buf.push(lines[i++] ?? '')
    out.push(<p key={key()}>{inline(buf.join(' '), ctx)}</p>)
  }
  return out
}

export function Markdown({ text, ...opts }: { text: string } & MarkdownOptions) {
  const { lang } = useI18n()
  return <div className="md">{renderMarkdown(text, { headings: lang === 'es' ? ES_HEADINGS : undefined, ...opts })}</div>
}
