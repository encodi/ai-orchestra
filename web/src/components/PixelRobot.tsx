// Three original pixel-art robots drawn as SVG rectangles (no images, no external assets).
// They are decorative: the agent's name is always written next to them, and the accessible label says
// the agent and its state. States only change the face and a small motion; they carry no information
// that the text does not (and motion stops with reduced motion).
import { memo } from 'react'

export type RobotState = 'idle' | 'thinking' | 'speaking' | 'reviewing' | 'waiting' | 'finished' | 'error'
export type RobotKind = 'claude' | 'codex' | 'google' | 'other'

type Rect = { x: number; y: number; w: number; h: number }
type Face = {
  eyeL: Rect
  eyeR: Rect
  /** The mouth baseline: x of its first pixel, y, width. */
  mouth: { x: number; y: number; w: number }
}

// Legend: o outline, b body, w secondary body colour, v visor, p prism / antenna accent, c circuit, . empty.
const GRIDS: Record<RobotKind, string[]> = {
  claude: [
    '......pp......',
    '.......o......',
    '...oooooooo...',
    '..obbbbbbbbo..',
    '.obbbbbbbbbbo.',
    '.obbbbbbbbbbo.',
    '.obbbbbbbbbbo.',
    '.obbbbbbbbbbo.',
    '.obbbbbbbbbbo.',
    '..obbbbbbbbo..',
    '...oooooooo...',
    '...obbbbbbo...',
    '...obo..obo...',
    '...ooo..ooo...',
  ],
  codex: [
    '...oooooooo...',
    '..obbbbbbbbo..',
    '.obbbbbbbbbbo.',
    '.ovvvvvvvvvvo.',
    '.ovvvvvvvvvvo.',
    '.ovvvvvvvvvvo.',
    '.obbbbbbbbbbo.',
    '.obcbbbbbbcbo.',
    '..obcbbbbcbo..',
    '...oooooooo...',
    '....obbbbo....',
    '..oobbbbbboo..',
    '..obbo..obbo..',
    '..oooo..oooo..',
  ],
  google: [
    '......pp......',
    '.....pppp.....',
    '..oooooooooo..',
    '.obbbbbbbbbbo.',
    '.obbbbbbbbbbo.',
    '.obbbbbbbbbbo.',
    '.obbbbbbbbbbo.',
    '.obbbbbbbbbbo.',
    '.owwwwwwwwwwo.',
    '.owwwwwwwwwwo.',
    '..oooooooooo..',
    '...owwwwwwo...',
    '...owo..owo...',
    '...ooo..ooo...',
  ],
  other: [
    '..............',
    '...oooooooo...',
    '..obbbbbbbbo..',
    '.obbbbbbbbbbo.',
    '.obbbbbbbbbbo.',
    '.obbbbbbbbbbo.',
    '.obbbbbbbbbbo.',
    '.obbbbbbbbbbo.',
    '..obbbbbbbbo..',
    '...oooooooo...',
    '...obbbbbbo...',
    '...obo..obo...',
    '...ooo..ooo...',
    '..............',
  ],
}

const FACES: Record<RobotKind, Face> = {
  claude: { eyeL: { x: 4, y: 5, w: 2, h: 2 }, eyeR: { x: 8, y: 5, w: 2, h: 2 }, mouth: { x: 5, y: 8, w: 4 } },
  codex: { eyeL: { x: 3, y: 3, w: 2, h: 2 }, eyeR: { x: 9, y: 3, w: 2, h: 2 }, mouth: { x: 5, y: 7, w: 4 } },
  google: { eyeL: { x: 4, y: 4, w: 2, h: 3 }, eyeR: { x: 8, y: 4, w: 2, h: 3 }, mouth: { x: 6, y: 8, w: 2 } },
  other: { eyeL: { x: 4, y: 4, w: 2, h: 2 }, eyeR: { x: 8, y: 4, w: 2, h: 2 }, mouth: { x: 5, y: 7, w: 4 } },
}

// The second pose of the legs, used while walking (rows 12 and 13 of the grid).
const LEGS_B: Record<RobotKind, string[]> = {
  claude: ['..obo....obo..', '..ooo....ooo..'],
  codex: ['.obbo....obbo.', '.oooo....oooo.'],
  google: ['..owo....owo..', '..ooo....ooo..'],
  other: ['..obo....obo..', '..ooo....ooo..'],
}

/** The first row of each head, where a cap or headphones go. */
const HEAD_TOP: Record<RobotKind, number> = { claude: 2, codex: 0, google: 2, other: 1 }

export type Accessory = 'none' | 'glasses' | 'cap' | 'headphones'
export const ACCESSORIES: Accessory[] = ['none', 'glasses', 'cap', 'headphones']

export const ROBOT_NAMES: Record<RobotKind, string> = { claude: 'Amber Bot', codex: 'Circuit Bot', google: 'Prism Bot', other: 'Agent Bot' }

function runs(grid: string[]): { x: number; y: number; w: number; c: string }[] {
  const out: { x: number; y: number; w: number; c: string }[] = []
  grid.forEach((row, y) => {
    let x = 0
    while (x < row.length) {
      const c = row[x] ?? '.'
      if (c === '.') {
        x++
        continue
      }
      let w = 1
      while (row[x + w] === c) w++
      out.push({ x, y, w, c })
      x += w
    }
  })
  return out
}

function eyeRects(face: Face, state: RobotState): { r: Rect; side: 'l' | 'r' }[] {
  const shrink = (r: Rect): Rect => ({ x: r.x, y: r.y + r.h - 1, w: r.w, h: 1 })
  const narrow = (r: Rect): Rect => ({ x: r.x, y: r.y + Math.floor(r.h / 2), w: r.w, h: Math.max(1, r.h - 1) })
  switch (state) {
    case 'waiting':
      return [{ r: shrink(face.eyeL), side: 'l' }, { r: shrink(face.eyeR), side: 'r' }]
    case 'reviewing':
      return [{ r: narrow(face.eyeL), side: 'l' }, { r: narrow(face.eyeR), side: 'r' }]
    case 'error':
      return []
    default:
      return [{ r: face.eyeL, side: 'l' }, { r: face.eyeR, side: 'r' }]
  }
}

function crossPixels(r: Rect): Rect[] {
  const cx = r.x
  const cy = r.y
  const out: Rect[] = [{ x: cx, y: cy, w: 1, h: 1 }, { x: cx + 1, y: cy + 1, w: 1, h: 1 }, { x: cx + 1, y: cy, w: 1, h: 1 }, { x: cx, y: cy + 1, w: 1, h: 1 }]
  return r.h >= 3 ? [...out, { x: cx, y: cy + 2, w: 1, h: 1 }, { x: cx + 1, y: cy + 2, w: 1, h: 1 }] : out
}

function mouthRects(face: Face, state: RobotState): Rect[] {
  const { x, y, w } = face.mouth
  switch (state) {
    case 'speaking':
      return [{ x, y, w, h: 2 }]
    case 'finished':
      return [{ x: x - 1, y, w: 1, h: 1 }, { x: x + w, y, w: 1, h: 1 }, { x, y: y + 1, w, h: 1 }]
    case 'error':
      return [{ x: x - 1, y: y + 1, w: 1, h: 1 }, { x: x + w, y: y + 1, w: 1, h: 1 }, { x, y, w, h: 1 }]
    case 'thinking':
      return [{ x: x + 1, y, w: Math.max(1, w - 2), h: 1 }]
    case 'waiting':
      return [{ x: x + 1, y, w: Math.max(1, w - 2), h: 1 }]
    default:
      return [{ x, y, w, h: 1 }]
  }
}

const STATE_LABEL: Record<RobotState, string> = {
  idle: 'idle', thinking: 'thinking', speaking: 'speaking', reviewing: 'reviewing', waiting: 'waiting', finished: 'finished', error: 'error',
}

type Props = {
  kind: RobotKind
  state?: RobotState
  size?: number
  /** 0 is the agent's own colours; 1 and 2 are alternatives chosen by the user. */
  palette?: number
  accessory?: Accessory | string
  /** Alternates the legs (a walk cycle). Only the pose changes; where it goes is up to the caller. */
  walking?: boolean
  /** Looks to the left instead of the right. */
  flip?: boolean
  /** Decorative when the agent's name is written beside it (the usual case). */
  decorative?: boolean
  label?: string
}

function accessoryRects(kind: RobotKind, face: Face, accessory: string): { r: Rect; cls: string }[] {
  const ht = HEAD_TOP[kind]
  switch (accessory) {
    case 'glasses':
      return [face.eyeL, face.eyeR].flatMap((e) => [
        { r: { x: e.x - 1, y: e.y - 1, w: e.w + 2, h: 1 }, cls: 'px-acc' },
        { r: { x: e.x - 1, y: e.y + e.h, w: e.w + 2, h: 1 }, cls: 'px-acc' },
        { r: { x: e.x - 1, y: e.y, w: 1, h: e.h }, cls: 'px-acc' },
        { r: { x: e.x + e.w, y: e.y, w: 1, h: e.h }, cls: 'px-acc' },
      ]).concat([{ r: { x: face.eyeL.x + face.eyeL.w + 1, y: face.eyeL.y, w: Math.max(0, face.eyeR.x - face.eyeL.x - face.eyeL.w - 2), h: 1 }, cls: 'px-acc' }]).filter((a) => a.r.w > 0)
    case 'cap':
      return [
        { r: { x: 3, y: ht - 1 < 0 ? 0 : ht - 1, w: 8, h: 1 }, cls: 'px-cap' },
        { r: { x: 2, y: ht, w: 10, h: 1 }, cls: 'px-cap' },
        { r: { x: 10, y: ht + 1, w: 3, h: 1 }, cls: 'px-cap' },
      ]
    case 'headphones':
      return [
        { r: { x: 2, y: ht, w: 10, h: 1 }, cls: 'px-acc' },
        { r: { x: 0, y: ht + 3, w: 2, h: 4 }, cls: 'px-acc' },
        { r: { x: 12, y: ht + 3, w: 2, h: 4 }, cls: 'px-acc' },
      ]
    default:
      return []
  }
}

export const PixelRobot = memo(function PixelRobot({ kind, state = 'idle', size = 64, palette = 0, accessory = 'none', walking = false, flip = false, decorative = false, label }: Props) {
  const grid = GRIDS[kind]
  const face = FACES[kind]
  const body = runs(grid.slice(0, 12))
  const legsA = runs(grid.slice(12)).map((r) => ({ ...r, y: r.y + 12 }))
  const legsB = runs(LEGS_B[kind]).map((r) => ({ ...r, y: r.y + 12 }))
  const extras = accessoryRects(kind, face, accessory)
  const eyes = eyeRects(face, state)
  const mouth = mouthRects(face, state)
  const name = label ?? ROBOT_NAMES[kind]
  const a11y = decorative ? { 'aria-hidden': true as const } : { role: 'img' as const, 'aria-label': `${name}: ${STATE_LABEL[state]}` }
  return (
    <svg
      className={`robot robot-${kind} robot-st-${state} pal-${palette}${walking ? ' walking' : ''}${flip ? ' flip' : ''}`}
      viewBox="0 0 14 14"
      width={size}
      height={size}
      shapeRendering="crispEdges"
      focusable="false"
      {...a11y}
    >
      <g className="robot-body">
        {body.map((r, i) => (
          <rect key={i} className={`px-${r.c}`} x={r.x} y={r.y} width={r.w} height={1} />
        ))}
        <g className="legs legs-a">
          {legsA.map((r, i) => (
            <rect key={i} className={`px-${r.c}`} x={r.x} y={r.y} width={r.w} height={1} />
          ))}
        </g>
        <g className="legs legs-b">
          {legsB.map((r, i) => (
            <rect key={i} className={`px-${r.c}`} x={r.x} y={r.y} width={r.w} height={1} />
          ))}
        </g>
        {state === 'error'
          ? [face.eyeL, face.eyeR].flatMap((e, ei) =>
              crossPixels(e).map((p, pi) => <rect key={`x${ei}-${pi}`} className="px-x" x={p.x} y={p.y} width={p.w} height={p.h} />),
            )
          : eyes.map((e, i) => <rect key={`e${i}`} className={`px-e px-e-${e.side}`} x={e.r.x} y={e.r.y} width={e.r.w} height={e.r.h} />)}
        {mouth.map((m, i) => (
          <rect key={`m${i}`} className={`px-m${state === 'speaking' ? ' px-talk' : ''}`} x={m.x} y={m.y} width={m.w} height={m.h} />
        ))}
        {extras.map((a, i) => (
          <rect key={`a${i}`} className={a.cls} x={a.r.x} y={a.r.y} width={a.r.w} height={a.r.h} />
        ))}
        {state === 'thinking' && (
          <g className="px-dots">
            <rect x={11} y={0} width={1} height={1} />
            <rect x={12} y={0} width={1} height={1} />
            <rect x={13} y={0} width={1} height={1} />
          </g>
        )}
        {state === 'reviewing' && (
          <g className="px-lens">
            <rect x={11} y={9} width={2} height={1} />
            <rect x={10} y={10} width={1} height={2} />
            <rect x={13} y={10} width={1} height={2} />
            <rect x={11} y={12} width={2} height={1} />
          </g>
        )}
        {state === 'finished' && (
          <g className="px-spark">
            <rect x={12} y={1} width={1} height={3} />
            <rect x={11} y={2} width={3} height={1} />
          </g>
        )}
      </g>
    </svg>
  )
})
