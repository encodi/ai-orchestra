import { StrictMode } from 'react'
import { createRoot } from 'react-dom/client'
import './styles/tokens.css'
import './styles/base.css'
import './styles/app.css'
import './styles/robots.css'
import App from './App'
import { PrefsProvider } from './state/prefs'
import { CharactersProvider } from './state/characters'
import { StoreProvider } from './state/store'

const el = document.getElementById('root')
if (el) {
  createRoot(el).render(
    <StrictMode>
      <PrefsProvider>
        <StoreProvider>
          <CharactersProvider>
            <App />
          </CharactersProvider>
        </StoreProvider>
      </PrefsProvider>
    </StrictMode>,
  )
}
