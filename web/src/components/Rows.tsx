import type { Entry, ExtStat, LargeFile, Metric, Rollup } from '../api'
import {
  CATEGORY_COLOR,
  CATEGORY_LABEL,
  bytes,
  categoryOf,
  count,
  mtime,
  percent,
  reclaimableHint,
  type Category,
} from '../format'

const value = (o: { size: number; alloc: number }, m: Metric) => (m === 'alloc' ? o.alloc : o.size)

function Bar({ pct, color }: { pct: number; color: string }) {
  return (
    <div className="bar" title={`${pct.toFixed(1)}%`}>
      <div className="bar-fill" style={{ width: `${Math.min(100, pct)}%`, background: color }} />
    </div>
  )
}

export function FolderRows({
  entries,
  other,
  total,
  metric,
  selected,
  onSelect,
  onOpen,
  onUp,
}: {
  entries: Entry[]
  other: Rollup
  total: number
  metric: Metric
  selected: number | null
  onSelect: (id: number) => void
  onOpen: (id: number) => void
  /// Absent at the scan root, where there is nowhere to go up to.
  onUp?: () => void
}) {
  return (
    <ul className="rows">
      {onUp && (
        <li className="row row-up" onClick={onUp} title="Go up one level">
          <span className="row-icon">↑</span>
          <span className="row-name">..</span>
          <span className="row-meta" />
          <span />
          <span />
          <span />
        </li>
      )}
      {!entries.length && !other.count && <li className="empty">Nothing here yet.</li>}
      {entries.map((e) => {
        const v = value(e, metric)
        const cat: Category = e.kind === 'dir' ? 'folder' : categoryOf(e.ext)
        const hint = e.kind === 'dir' ? reclaimableHint(e.name) : undefined
        return (
          <li
            key={e.id}
            className={'row' + (e.id === selected ? ' row-selected' : '')}
            onClick={() => onSelect(e.id)}
            onDoubleClick={() => e.kind === 'dir' && onOpen(e.id)}
          >
            <span className="row-icon" style={{ color: CATEGORY_COLOR[cat] }}>
              {e.kind === 'dir' ? '▣' : e.kind === 'link' ? '↗' : '▪'}
            </span>
            <span className="row-name" title={e.name}>
              {e.kind === 'dir' ? (
                <button className="link" onClick={(ev) => (ev.stopPropagation(), onOpen(e.id))}>
                  {e.name}
                </button>
              ) : (
                e.name
              )}
              {hint && <span className="badge" title={`${e.name} holds ${hint}`}>{hint}</span>}
              {e.err && <span className="badge badge-err">unreadable</span>}
              {e.kind === 'dir' && !e.read && <span className="badge badge-wait">scanning…</span>}
            </span>
            <span className="row-meta">
              {e.kind === 'dir' ? `${count(e.files)} files` : mtime(e.mtime)}
            </span>
            <Bar pct={percent(v, total)} color={CATEGORY_COLOR[cat]} />
            <span className="row-pct">{percent(v, total).toFixed(1)}%</span>
            <span className="row-size">{bytes(v)}</span>
          </li>
        )
      })}
      {other.count > 0 && (
        <li className="row row-other">
          <span className="row-icon">·</span>
          <span className="row-name">{count(other.count)} smaller entries</span>
          <span className="row-meta" />
          <Bar pct={percent(value(other, metric), total)} color="var(--map-rest)" />
          <span className="row-pct">{percent(value(other, metric), total).toFixed(1)}%</span>
          <span className="row-size">{bytes(value(other, metric))}</span>
        </li>
      )}
    </ul>
  )
}

interface Group {
  cat: Category
  size: number
  alloc: number
  count: number
  exts: ExtStat[]
}

export function TypeRows({
  types,
  metric,
  total,
}: {
  types: ExtStat[]
  metric: Metric
  total: number
}) {
  const groups = new Map<Category, Group>()
  for (const t of types) {
    const cat = categoryOf(t.ext)
    let g = groups.get(cat)
    if (!g) {
      g = { cat, size: 0, alloc: 0, count: 0, exts: [] }
      groups.set(cat, g)
    }
    g.size += t.size
    g.alloc += t.alloc
    g.count += t.count
    g.exts.push(t)
  }
  const ordered = [...groups.values()].sort((a, b) => value(b, metric) - value(a, metric))
  if (!ordered.length) return <p className="empty">No files scanned here yet.</p>

  return (
    <ul className="rows">
      {ordered.map((g) => (
        <li key={g.cat} className="group">
          <div className="row row-group">
            <span className="row-icon" style={{ color: CATEGORY_COLOR[g.cat] }}>
              ●
            </span>
            <span className="row-name">{CATEGORY_LABEL[g.cat]}</span>
            <span className="row-meta">{count(g.count)} files</span>
            <Bar pct={percent(value(g, metric), total)} color={CATEGORY_COLOR[g.cat]} />
            <span className="row-pct">{percent(value(g, metric), total).toFixed(1)}%</span>
            <span className="row-size">{bytes(value(g, metric))}</span>
          </div>
          <ul className="rows rows-nested">
            {g.exts
              .sort((a, b) => value(b, metric) - value(a, metric))
              .slice(0, 8)
              .map((t) => (
                <li key={t.ext || '(none)'} className="row row-sub">
                  <span className="row-icon" />
                  <span className="row-name">{t.ext ? `.${t.ext}` : 'no extension'}</span>
                  <span className="row-meta">{count(t.count)}</span>
                  <Bar pct={percent(value(t, metric), total)} color={CATEGORY_COLOR[g.cat]} />
                  <span className="row-pct">{percent(value(t, metric), total).toFixed(1)}%</span>
                  <span className="row-size">{bytes(value(t, metric))}</span>
                </li>
              ))}
          </ul>
        </li>
      ))}
    </ul>
  )
}

export function LargestRows({
  files,
  metric,
  total,
}: {
  files: LargeFile[]
  metric: Metric
  total: number
}) {
  if (!files.length) return <p className="empty">No files scanned here yet.</p>
  return (
    <ul className="rows">
      {files.map((f) => {
        const v = value(f, metric)
        const slash = f.path.lastIndexOf('/')
        const name = f.path.slice(slash + 1)
        const dir = slash >= 0 ? f.path.slice(0, slash) : ''
        const dot = name.lastIndexOf('.')
        const cat = categoryOf(dot > 0 ? name.slice(dot + 1) : null)
        return (
          <li key={f.id} className="row">
            <span className="row-icon" style={{ color: CATEGORY_COLOR[cat] }}>
              ▪
            </span>
            {/* The file name is what identifies the row, so it never gets
                clipped; the directory takes whatever width is left. */}
            <span className="row-name row-path" title={f.path}>
              <span className="path-base">{name}</span>
              {dir && <span className="path-dir">{dir}</span>}
            </span>
            <span className="row-meta">{mtime(f.mtime)}</span>
            <Bar pct={percent(v, total)} color={CATEGORY_COLOR[cat]} />
            <span className="row-pct">{percent(v, total).toFixed(1)}%</span>
            <span className="row-size">{bytes(v)}</span>
          </li>
        )
      })}
    </ul>
  )
}
