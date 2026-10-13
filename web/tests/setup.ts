import '@testing-library/jest-dom/vitest'
import { afterEach, vi } from 'vitest'
import { cleanup } from '@testing-library/react'

// jsdom has no layout: scrolling is a no-op there.
Element.prototype.scrollIntoView = vi.fn()
Element.prototype.scrollTo = vi.fn() as unknown as typeof Element.prototype.scrollTo

afterEach(() => cleanup())
