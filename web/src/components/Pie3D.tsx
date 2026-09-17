import { useId, useMemo, useState } from 'react'
import { bytes, percent } from '../format'
import { pieArcs, shade, sidePaths, topPath, type PieSlice } from '../volumeChart'

/** SVG extrusion preserves exact slice angles; values remain in the legend. */
export function Pie3D({ slices, label }: { slices: PieSlice[]; label: string }) {
  const id = useId().replace(/:/g, '')
  const arcs = useMemo(() => pieArcs(slices), [slices])
  const [active, setActive] = useState<string | null>(null)
  const total = arcs.reduce((n, s) => n + s.value, 0)
  const highlighted = arcs.find(s => s.id === active)
  const share = (value: number) => {
    const pct = percent(value, total)
    return pct > 0 && pct < 0.1 ? '<0.1%' : `${pct.toFixed(1)}%`
  }
  const transform = (sliceId: string) => sliceId === active ? 'translate(0,-7)' : undefined
  if (!total) return <div className="volume-chart-empty">No scanned file data on this volume yet.</div>
  return (
    <div className="pie3d">
      <div className="pie3d-graphic">
        <svg viewBox="0 0 380 265" role="img" aria-label={label}>
          <title>{label}</title>
          <defs>
            <filter id={`${id}-shadow`} x="-50%" y="-50%" width="200%" height="200%">
              <feGaussianBlur stdDeviation="7" />
            </filter>
            {arcs.map(s => (
              <linearGradient key={s.id} id={`${id}-${s.id}`} x1="0" y1="0" x2="0" y2="1">
                <stop offset="0%" stopColor={s.color} />
                <stop offset="100%" stopColor={shade(s.color, 0.85)} />
              </linearGradient>
            ))}
          </defs>
          <ellipse cx="190" cy="159" rx="150" ry="77" fill="#000" opacity="0.22" filter={`url(#${id}-shadow)`} />
          <g aria-hidden="true">
            {arcs.map(s => (
              <g key={s.id} className="pie3d-slice" transform={transform(s.id)}>
                {sidePaths(s).map((path, index) => <path key={index} d={path} fill={shade(s.color, 0.56)} />)}
              </g>
            ))}
          </g>
          {arcs.map(s => (
            <g
              key={s.id}
              className={`pie3d-slice${s.id === active ? ' pie3d-active' : ''}`}
              transform={transform(s.id)}
              tabIndex={0}
              role="img"
              aria-label={`${s.label}: ${bytes(s.value)}, ${share(s.value)}`}
              onMouseEnter={() => setActive(s.id)}
              onMouseLeave={() => setActive(null)}
              onFocus={() => setActive(s.id)}
              onBlur={() => setActive(null)}
            >
              <title>{s.label}: {bytes(s.value)} · {share(s.value)}</title>
              <path d={topPath(s)} fill={`url(#${id}-${s.id})`} stroke="var(--bg-raised)" strokeWidth="0.8" />
            </g>
          ))}
        </svg>
        <div className="pie3d-caption" aria-live="polite">
          <span>{highlighted?.label ?? 'Total represented'}</span>
          <strong>{bytes(highlighted?.value ?? total)}</strong>
          {highlighted && <span>{share(highlighted.value)}</span>}
        </div>
      </div>
      <ul className="pie3d-legend" aria-label={`${label} values`}>
        {arcs.map(s => (
          <li key={s.id} className={active === s.id ? 'on' : ''}>
            <button
              onMouseEnter={() => setActive(s.id)} onMouseLeave={() => setActive(null)}
              onFocus={() => setActive(s.id)} onBlur={() => setActive(null)}
              onClick={() => setActive(s.id)}
            >
              <i style={{ background: s.color }} />
              <span>{s.label}</span>
              <strong>{bytes(s.value)}</strong>
              <small>{share(s.value)}</small>
            </button>
          </li>
        ))}
      </ul>
    </div>
  )
}
