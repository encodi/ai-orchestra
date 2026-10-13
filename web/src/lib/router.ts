import { useEffect, useState } from 'react'

export type Route = { path: string; parts: string[] }

function read(): Route {
  const raw = window.location.hash.replace(/^#/, '') || '/'
  const path = raw.split('?')[0] || '/'
  return { path, parts: path.split('/').filter(Boolean).map((p) => decodeURIComponent(p)) }
}

export function navigate(path: string): void {
  window.location.hash = path
}

export function useRoute(): Route {
  const [route, setRoute] = useState(read)
  useEffect(() => {
    const on = () => setRoute(read())
    window.addEventListener('hashchange', on)
    return () => window.removeEventListener('hashchange', on)
  }, [])
  return route
}
