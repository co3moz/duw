import type {
  Entry,
  ExtStat,
  LargeFile,
  Metric,
  Rollup,
  SearchHit,
  SortKey,
} from '../api'
import {
  CATEGORY_COLOR,
  CATEGORY_LABEL,
  ageColor,
  bytes,
  categoryOf,
  count,
  mtime,
  percent,
  reclaimableHint,
  type Category,
} from '../format'

const value = (o: { size: number; alloc: number }, m: Metric) => (m === 'alloc' ? o.alloc : o.size)

/** Opens the per-entry action menu at the pointer. */
export type MenuHandler = (id: number, name: string, e: React.MouseEvent) => void

function MenuButton({ id, name, onMenu }: { id: number; name: string; onMenu: MenuHandler }) {
  return (
    <button
      className="row-menu"
      title="Actions"
      aria-label={`Actions for ${name}`}
      onClick={(ev) => {
        ev.stopPropagation()
        onMenu(id, name, ev)
      }}
      onContextMenu={(ev) => {
        ev.preventDefault()
        ev.stopPropagation()
        onMenu(id, name, ev)
      }}
    >
      ⋯
    </button>
  )
}

function Bar({ pct, color }: { pct: number; color: string }) {
  return (
    <div className="bar" title={`${pct.toFixed(1)}%`}>
      <div className="bar-fill" style={{ width: `${Math.min(100, pct)}%`, background: color }} />
    </div>
  )
}

/** Clickable column headers for the folder list. */
export function ListHeader({
  sort,
  asc,
  onSort,
}: {
  sort: SortKey
  asc: boolean
  onSort: (key: SortKey) => void
}) {
  const mark = (key: SortKey) => (sort === key ? (asc ? ' ↑' : ' ↓') : '')
  return (
    <div className="list-head">
      <div className="row row-head">
        <span />
        <button className="head-sort" onClick={() => onSort('name')}>
          name{mark('name')}
        </button>
        <span className="head-meta">
          <button className="head-sort" onClick={() => onSort('mtime')}>
            modified{mark('mtime')}
          </button>
          <button className="head-sort" onClick={() => onSort('count')}>
            files{mark('count')}
          </button>
        </span>
        <span />
        <span />
        <button className="head-sort head-size" onClick={() => onSort('size')}>
          size{mark('size')}
        </button>
        <span />
      </div>
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
  onMenu,
  age,
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
  onMenu?: MenuHandler
  /** Colour bars by age instead of file type. */
  age?: boolean
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
            data-id={e.id}
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
              {e.cloud && (
                <span className="badge badge-cloud" title="Stored in the cloud, not on this disk">
                  cloud
                </span>
              )}
              {e.kind === 'dir' && !e.read && <span className="badge badge-wait">scanning…</span>}
            </span>
            <span className="row-meta">
              {e.kind === 'dir' ? `${count(e.files)} files` : mtime(e.mtime)}
            </span>
            <Bar pct={percent(v, total)} color={age ? ageColor(e.mtime) : CATEGORY_COLOR[cat]} />
            <span className="row-pct">{percent(v, total).toFixed(1)}%</span>
            <span className="row-size">{bytes(v)}</span>
            {onMenu && <MenuButton id={e.id} name={e.name} onMenu={onMenu} />}
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
  onMenu,
  age,
}: {
  files: LargeFile[]
  metric: Metric
  total: number
  onMenu?: MenuHandler
  age?: boolean
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
          <li key={f.id} data-id={f.id} className="row">
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
            <Bar pct={percent(v, total)} color={age ? ageColor(f.mtime) : CATEGORY_COLOR[cat]} />
            <span className="row-pct">{percent(v, total).toFixed(1)}%</span>
            <span className="row-size">{bytes(v)}</span>
            {onMenu && <MenuButton id={f.id} name={name} onMenu={onMenu} />}
          </li>
        )
      })}
    </ul>
  )
}

/** Results of a filtered subtree search. */
export function SearchRows({
  hits,
  metric,
  total,
  onOpen,
  onMenu,
  age,
}: {
  hits: SearchHit[]
  metric: Metric
  total: number
  /** Double-clicking a file opens its folder; a directory opens itself. */
  onOpen: (hit: SearchHit) => void
  onMenu?: MenuHandler
  age?: boolean
}) {
  if (!hits.length) return <p className="empty">No matches.</p>
  return (
    <ul className="rows">
      {hits.map((h) => {
        const v = value(h, metric)
        const slash = h.path.lastIndexOf('/')
        const dir = slash >= 0 ? h.path.slice(0, slash) : ''
        const cat: Category = h.kind === 'dir' ? 'folder' : categoryOf(h.ext)
        return (
          <li key={h.id} data-id={h.id} className="row" onDoubleClick={() => onOpen(h)}>
            <span className="row-icon" style={{ color: CATEGORY_COLOR[cat] }}>
              {h.kind === 'dir' ? '▣' : h.kind === 'link' ? '↗' : '▪'}
            </span>
            <span className="row-name row-path" title={h.path}>
              <span className="path-base">{h.name}</span>
              {dir && <span className="path-dir">{dir}</span>}
            </span>
            <span className="row-meta">{mtime(h.mtime)}</span>
            <Bar pct={percent(v, total)} color={age ? ageColor(h.mtime) : CATEGORY_COLOR[cat]} />
            <span className="row-pct">{percent(v, total).toFixed(1)}%</span>
            <span className="row-size">{bytes(v)}</span>
            {onMenu && <MenuButton id={h.id} name={h.name} onMenu={onMenu} />}
          </li>
        )
      })}
    </ul>
  )
}
