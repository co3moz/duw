import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import { api, HttpError, filterActive, type FilterSpec, type Kind, type Metric, type SortKey } from './api'
import { bytes, count, duration, parseSize, AGE_STOPS } from './format'
import { useElementSize } from './useElementSize'
import { useDebounced, useLive, useResource, useThrottled } from './useLive'
import { Treemap } from './components/Treemap'
import { FolderRows, LargestRows, ListHeader, SearchRows, TypeRows } from './components/Rows'
import { Duplicates } from './components/Duplicates'
import { Snapshots } from './components/Snapshots'

type Tab = 'folders' | 'types' | 'largest' | 'duplicates' | 'snapshots'

/** Folder encoded in the URL, so links and the back button work. */
function nodeFromHash(): number {
  const m = /^#\/node\/(\d+)$/.exec(window.location.hash)
  return m ? Number(m[1]) : 0
}

const LIST_LIMIT = 500
const MAP_DEPTH = 3
const MAP_PER_LEVEL = 60
/** Refetch at most this often while a scan is streaming in. */
const REFRESH_MS = 600
/** Bounds for the draggable list/map divider, in pixels. */
const MIN_LIST = 280
const MIN_MAP = 320

export default function App() {
  const { state, progress, error } = useLive()
  const [nodeId, setNodeId] = useState(nodeFromHash)
  const [metric, setMetric] = useState<Metric>('size')
  const [tab, setTab] = useState<Tab>('folders')
  const [selected, setSelected] = useState<number | null>(null)

  // Width of the list panel in pixels. `null` keeps the stylesheet default
  // until the divider is dragged; `useElementSize` lets us clamp to the
  // available space when the window is resized.
  const [splitBox, splitRef] = useElementSize<HTMLElement>()
  const [listWidth, setListWidth] = useState<number | null>(null)
  const dragging = useRef(false)
  const [menu, setMenu] = useState<{ id: number; name: string; x: number; y: number } | null>(null)
  const [filters, setFilters] = useState({ q: '', ext: '', min: '', max: '', age: '' })
  const [sort, setSort] = useState<{ key: SortKey; asc: boolean }>({ key: 'size', asc: false })
  const [heat, setHeat] = useState(false)

  const onSort = useCallback((key: SortKey) => {
    setSort((cur) => (cur.key === key ? { key, asc: !cur.asc } : { key, asc: key === 'name' }))
  }, [])

  const dragTo = useCallback((clientX: number, parent: HTMLElement) => {
    const rect = parent.getBoundingClientRect()
    const max = Math.max(MIN_LIST, rect.width - MIN_MAP)
    setListWidth(Math.min(max, Math.max(MIN_LIST, clientX - rect.left)))
  }, [])

  useEffect(() => {
    if (listWidth == null || !splitBox.width) return
    const max = Math.max(MIN_LIST, splitBox.width - MIN_MAP)
    if (listWidth > max) setListWidth(max)
  }, [splitBox.width, listWidth])

  const version = useThrottled(progress?.version ?? 0, REFRESH_MS)

  const dq = useDebounced(filters.q, 250)
  const dext = useDebounced(filters.ext, 250)
  const dmin = useDebounced(filters.min, 250)
  const dmax = useDebounced(filters.max, 250)
  const dage = useDebounced(filters.age, 250)
  const minBytes = dmin.trim() ? parseSize(dmin) : null
  const maxBytes = dmax.trim() ? parseSize(dmax) : null
  const parsedAge = dage.trim() ? Number(dage) : NaN
  const ageDays = Number.isSafeInteger(parsedAge) && parsedAge >= 0 ? parsedAge : null
  const filter = useMemo<FilterSpec>(
    () => ({
      q: dq.trim().toLowerCase(),
      exts: dext
        .split(',')
        .map((e) => e.trim().replace(/^\./, '').toLowerCase())
        .filter(Boolean),
      min: minBytes ?? 0,
      max: maxBytes ?? Number.MAX_SAFE_INTEGER,
      maxMtime:
        ageDays != null && Number.isFinite(ageDays)
          ? Math.floor(Date.now() / 1000) - ageDays * 86_400
          : 0,
    }),
    [dq, dext, minBytes, maxBytes, ageDays],
  )
  const filtering = filterActive(filter)
  const filterParams = useMemo(() => ({
    q: filter.q,
    ext: filter.exts.join(','),
    min: filter.min > 0 ? filter.min : undefined,
    max: filter.max < Number.MAX_SAFE_INTEGER ? filter.max : undefined,
    age: ageDays ?? undefined,
  }), [filter, ageDays])

  const node = useResource(
    (s) => api.node(nodeId, metric, LIST_LIMIT, sort.key, sort.asc, s),
    [nodeId, metric, version, sort],
  )
  const map = useResource(
    (s) => api.tree(nodeId, metric, MAP_DEPTH, MAP_PER_LEVEL, s, filtering ? filterParams : undefined),
    [nodeId, metric, version, filtering, filterParams],
  )
  const search = useResource(
    (s) =>
      filtering
        ? api.search(
            nodeId,
            {
              ...filterParams,
              metric,
              limit: LIST_LIMIT,
            },
            s,
          )
        : Promise.resolve(null),
    [nodeId, metric, version, filtering, filterParams],
  )
  const types = useResource(
    (s) => (tab === 'types' ? api.types(nodeId, s) : Promise.resolve(null)),
    [nodeId, tab, version],
  )
  const largest = useResource(
    (s) => (tab === 'largest' ? api.largest(nodeId, metric, 100, s) : Promise.resolve(null)),
    [nodeId, tab, metric, version],
  )

  const dupes = progress?.dupes
  const [minSize, setMinSize] = useState<number | null>(null)
  // Results are only fetched once a run settles; while it runs the SSE stream
  // already carries everything the progress display needs.
  const dupeResults = useResource(
    (s) =>
      tab === 'duplicates' && dupes && dupes.phase !== 'idle'
        ? api.duplicates(nodeId, 200, s)
        : Promise.resolve(null),
    [nodeId, tab, dupes?.generation, dupes?.phase],
  )

  const effectiveMin = minSize ?? state?.dupes_min ?? 524288
  const runDupes = useCallback(
    async (min: number) => {
      setMinSize(min)
      try {
        const res = await api.startDuplicates(nodeId, min)
        if (!res.ok) window.alert(await res.text())
      } catch (e) {
        window.alert(String(e))
      }
    },
    [nodeId],
  )

  const open = useCallback((id: number) => {
    setNodeId(id)
    setSelected(null)
  }, [])

  const up = useCallback(() => {
    const crumbs = node.data?.breadcrumb
    if (crumbs && crumbs.length > 1) open(crumbs[crumbs.length - 2].id)
  }, [node.data, open])

  // Keep the URL in step with the folder in view. pushState (rather than
  // assigning location.hash) does not fire hashchange, so this cannot loop.
  const initialHash = useRef(true)
  useEffect(() => {
    const hash = `#/node/${nodeId}`
    const replace = initialHash.current
    initialHash.current = false
    if (window.location.hash !== hash) {
      if (replace) window.history.replaceState(null, '', hash)
      else window.history.pushState(null, '', hash)
    }
  }, [nodeId])

  useEffect(() => {
    const sync = () => {
      const id = nodeFromHash()
      setNodeId((cur) => (cur === id ? cur : id))
      setSelected(null)
    }
    window.addEventListener('popstate', sync)
    window.addEventListener('hashchange', sync)
    return () => {
      window.removeEventListener('popstate', sync)
      window.removeEventListener('hashchange', sync)
    }
  }, [])

  // A link to a node that is not part of this scan falls back to the root
  // instead of leaving the view stuck.
  useEffect(() => {
    if (node.error instanceof HttpError && node.error.status === 404 && nodeId !== 0) setNodeId(0)
  }, [node.error, nodeId])

  const onDividerDown = (e: React.PointerEvent<HTMLDivElement>) => {
    e.preventDefault()
    dragging.current = true
    e.currentTarget.setPointerCapture(e.pointerId)
  }
  const onDividerMove = (e: React.PointerEvent<HTMLDivElement>) => {
    if (!dragging.current) return
    const parent = e.currentTarget.parentElement
    if (parent) dragTo(e.clientX, parent)
  }
  const onDividerUp = (e: React.PointerEvent<HTMLDivElement>) => {
    dragging.current = false
    if (e.currentTarget.hasPointerCapture(e.pointerId)) {
      e.currentTarget.releasePointerCapture(e.pointerId)
    }
  }

  const openMenu = useCallback((id: number, name: string, e: React.MouseEvent) => {
    e.preventDefault()
    e.stopPropagation()
    setMenu({ id, name, x: e.clientX, y: e.clientY })
  }, [])

  const revealEntry = useCallback(async (id: number) => {
    const res = await api.reveal(id)
    if (!res.ok) window.alert(`could not open the file manager: ${await res.text()}`)
  }, [])

  const copyPath = useCallback(async (id: number) => {
    try {
      const { path } = await api.abs(id)
      await navigator.clipboard.writeText(path)
    } catch (e) {
      window.alert(String(e))
    }
  }, [])

  const rescanEntry = useCallback(async (id: number) => {
    const res = await api.rescan(id)
    if (!res.ok) window.alert(await res.text())
  }, [])

  const moveToTrash = useCallback(async (id: number, name: string) => {
    if (!window.confirm(`Move "${name}" to the trash?`)) return
    const res = await api.trash(id)
    if (!res.ok) {
      window.alert(await res.text())
      return
    }
    setSelected((cur) => (cur === id ? null : cur))
  }, [])

  // The rows the arrow keys can walk through, in display order.
  const visible = useMemo<{ id: number; kind: Kind }[]>(() => {
    if (tab === 'folders') {
      return filtering
        ? (search.data?.hits ?? []).map((h) => ({ id: h.id, kind: h.kind }))
        : (node.data?.children ?? []).map((e) => ({ id: e.id, kind: e.kind }))
    }
    if (tab === 'largest') {
      return (largest.data?.files ?? []).map((f) => ({ id: f.id, kind: 'file' as const }))
    }
    return []
  }, [tab, filtering, search.data, node.data, largest.data])

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const el = e.target as HTMLElement | null
      const typing =
        !!el && (el.tagName === 'INPUT' || el.tagName === 'TEXTAREA' || el.isContentEditable)
      if (e.key === 'Escape' && menu) {
        e.preventDefault()
        setMenu(null)
        return
      }
      if (typing) return

      if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
        if (!visible.length) return
        e.preventDefault()
        const idx = visible.findIndex((v) => v.id === selected)
        const next =
          e.key === 'ArrowDown'
            ? idx < 0
              ? 0
              : Math.min(visible.length - 1, idx + 1)
            : idx < 0
              ? visible.length - 1
              : Math.max(0, idx - 1)
        setSelected(visible[next].id)
        return
      }
      if (e.key === 'Enter') {
        const row = visible.find((v) => v.id === selected)
        if (row?.kind === 'dir') {
          e.preventDefault()
          open(row.id)
        }
        return
      }
      if (e.key === 'Backspace' || e.key === 'Escape') {
        e.preventDefault()
        up()
      }
    }
    window.addEventListener('keydown', onKey)
    return () => window.removeEventListener('keydown', onKey)
  }, [up, menu, visible, selected, open])

  // Keep the keyboard selection on screen when it moves past the fold.
  useEffect(() => {
    if (selected == null) return
    document.querySelector(`[data-id="${selected}"]`)?.scrollIntoView({ block: 'nearest' })
  }, [selected])

  if (error && !state) {
    return <div className="fatal">{error}</div>
  }
  if (!state || !node.data) {
    return <div className="loading">scanning…</div>
  }

  const view = node.data
  const total = metric === 'alloc' ? view.alloc : view.size
  const scanning = progress?.scanning ?? false
  const stats = progress?.stats
  const splitStyle =
    listWidth != null
      ? ({ '--list-w': `${Math.round(listWidth)}px` } as React.CSSProperties)
      : undefined

  return (
    <div className="app">
      <header className="head">
        <div className="head-top">
          <div className="brand" title={state.root}>
            duw
          </div>
          <nav className="crumbs">
            {view.breadcrumb.map((c, i) => (
              <span key={c.id}>
                {i > 0 && <span className="sep">/</span>}
                <button
                  className={'crumb' + (i === view.breadcrumb.length - 1 ? ' crumb-current' : '')}
                  onClick={() => open(c.id)}
                >
                  {c.name}
                </button>
              </span>
            ))}
          </nav>
          <div className="head-actions">
            <div className="toggle" role="group" aria-label="Size metric">
              <button
                className={metric === 'size' ? 'on' : ''}
                onClick={() => setMetric('size')}
                title="Sum of file sizes, like du --apparent-size"
              >
                apparent
              </button>
              <button
                className={metric === 'alloc' ? 'on' : ''}
                onClick={() => setMetric('alloc')}
                title={
                  state.platform.approximate_alloc
                    ? 'Space used on disk, estimated from the volume cluster size'
                    : 'Space actually used on disk'
                }
              >
                on&nbsp;disk{state.platform.approximate_alloc ? '*' : ''}
              </button>
            </div>
            <div className="toggle" role="group" aria-label="View mode">
              <button
                className={heat ? 'on' : ''}
                onClick={() => setHeat((h) => !h)}
                title="Colour by how long since each entry was modified"
              >
                age map
              </button>
            </div>
            {scanning && (
              <button className="stop" onClick={() => api.cancel()}>
                stop scan
              </button>
            )}
          </div>
        </div>

        <div className="stats">
          {/* Sizes and counts describe the folder in view; time and problem
              counters describe the scan as a whole. */}
          <Stat label="total" value={bytes(total)} strong />
          <Stat label="files" value={count(view.files)} />
          <Stat label="folders" value={count(view.dirs)} />
          <Stat label="elapsed" value={duration(progress?.elapsed_ms ?? 0)} />
          {!!stats?.errors && <Stat label="unreadable" value={count(stats.errors)} warn />}
          {!!stats?.skipped && <Stat label="skipped" value={count(stats.skipped)} />}
          <div className="spacer" />
          <div className={'status' + (scanning ? ' status-live' : '')}>
            {progress?.cancelled
              ? 'scan stopped'
              : scanning
                ? 'scanning'
                : `scan complete in ${duration(progress?.elapsed_ms ?? 0)}`}
          </div>
        </div>
        {scanning && <div className="scanline" />}
      </header>

      <main className="split" ref={splitRef} style={splitStyle}>
        <section className="panel panel-list">
          <div className="filters">
            <input
              className="filter-q"
              placeholder="search name…"
              value={filters.q}
              onChange={(e) => setFilters((f) => ({ ...f, q: e.target.value }))}
            />
            <input
              className="filter-ext"
              placeholder="ext"
              title="Comma-separated extensions, e.g. png,jpg"
              value={filters.ext}
              onChange={(e) => setFilters((f) => ({ ...f, ext: e.target.value }))}
            />
            <input
              className="filter-min"
              placeholder="min"
              title="Smallest size, e.g. 10M"
              value={filters.min}
              onChange={(e) => setFilters((f) => ({ ...f, min: e.target.value }))}
            />
            <input
              className="filter-max"
              placeholder="max"
              title="Largest size, e.g. 1G"
              value={filters.max}
              onChange={(e) => setFilters((f) => ({ ...f, max: e.target.value }))}
            />
            <input
              className="filter-age"
              placeholder="older (d)"
              title="Modified at least this many days ago"
              value={filters.age}
              onChange={(e) => setFilters((f) => ({ ...f, age: e.target.value }))}
            />
            {filtering && (
              <button
                className="filter-clear"
                title="Clear the filter"
                onClick={() => setFilters({ q: '', ext: '', min: '', max: '', age: '' })}
              >
                clear
              </button>
            )}
          </div>
          <div className="tabs">
            <button className={tab === 'folders' ? 'on' : ''} onClick={() => setTab('folders')}>
              folders
            </button>
            <button className={tab === 'types' ? 'on' : ''} onClick={() => setTab('types')}>
              file types
            </button>
            <button className={tab === 'largest' ? 'on' : ''} onClick={() => setTab('largest')}>
              largest files
            </button>
            <button
              className={tab === 'duplicates' ? 'on' : ''}
              onClick={() => setTab('duplicates')}
            >
              duplicates
            </button>
            <button className={tab === 'snapshots' ? 'on' : ''} onClick={() => setTab('snapshots')}>
              snapshots
            </button>
          </div>
          {tab === 'folders' && !filtering && (
            <ListHeader sort={sort.key} asc={sort.asc} onSort={onSort} />
          )}
          <div className="panel-body">
            {tab === 'folders' &&
              (filtering ? (
                search.data ? (
                  <>
                    <div className="filter-note">
                      {count(search.data.hits.length)} match
                      {search.data.hits.length === 1 ? '' : 'es'}
                      {search.data.truncated ? ' (showing the biggest)' : ''}
                    </div>
                    <SearchRows
                      hits={search.data.hits}
                      metric={metric}
                      total={total}
                      onOpen={(hit) => open(hit.kind === 'dir' ? hit.id : hit.parent)}
                      onMenu={openMenu}
                      age={heat}
                    />
                  </>
                ) : (
                  <p className="empty">searching…</p>
                )
              ) : (
                <FolderRows
                  entries={view.children}
                  other={view.other}
                  total={total}
                  metric={metric}
                  selected={selected}
                  onSelect={setSelected}
                  onOpen={open}
                  onUp={view.breadcrumb.length > 1 ? up : undefined}
                  onMenu={openMenu}
                  age={heat}
                />
              ))}
            {tab === 'types' &&
              (types.data ? (
                <TypeRows types={types.data.types} metric={metric} total={total} />
              ) : (
                <p className="empty">aggregating…</p>
              ))}
            {tab === 'largest' &&
              (largest.data ? (
                <LargestRows
                  files={largest.data.files}
                  metric={metric}
                  total={total}
                  onMenu={openMenu}
                  age={heat}
                />
              ) : (
                <p className="empty">aggregating…</p>
              ))}
            {tab === 'duplicates' && dupes && (
              <Duplicates
                progress={dupes}
                scanReady={!scanning}
                groups={dupeResults.data?.groups ?? []}
                truncated={dupeResults.data?.truncated ?? false}
                totalGroups={dupeResults.data?.total_groups ?? 0}
                minSize={effectiveMin}
                scopeName={view.name}
                matchesScope={dupes.scope === nodeId && dupes.phase !== 'idle'}
                onMinSize={(value) => setMinSize(value)}
                onScan={() => runDupes(effectiveMin)}
                onCancel={() => api.cancelDuplicates()}
                onMenu={openMenu}
              />
            )}
            {tab === 'snapshots' && <Snapshots root={state.root} />}
          </div>
        </section>

        <div
          className="divider"
          role="separator"
          aria-orientation="vertical"
          aria-label="Resize the list and map panels"
          title="Drag to resize, double-click to reset"
          onPointerDown={onDividerDown}
          onPointerMove={onDividerMove}
          onPointerUp={onDividerUp}
          onPointerCancel={onDividerUp}
          onDoubleClick={() => setListWidth(null)}
        />

        <section className="panel panel-map">
          {map.data ? (
            <>
              <Treemap
                root={map.data.root}
                metric={metric}
                onOpen={open}
                selected={selected}
                onMenu={openMenu}
                heat={heat}
              />
              {heat && (
                <div className="age-legend">
                  {AGE_STOPS.map((s) => (
                    <span key={s.label}>
                      <i style={{ background: s.color }} />
                      {s.label}
                    </span>
                  ))}
                </div>
              )}
            </>
          ) : (
            <div className="empty">building map…</div>
          )}
        </section>
      </main>

      <footer className="foot">
        <span className="foot-path" title={progress?.current}>
          {scanning ? progress?.current || state.root : state.root}
        </span>
        {state.platform.approximate_alloc && metric === 'alloc' && (
          <span className="note">* on-disk sizes are rounded to the cluster size</span>
        )}
        {!state.platform.hardlink_dedup && <span className="note">hard links counted once per link</span>}
      </footer>

      {menu && (
        <>
          <div
            className="menu-backdrop"
            onClick={() => setMenu(null)}
            onContextMenu={(e) => {
              e.preventDefault()
              setMenu(null)
            }}
          />
          <div
            className="menu"
            role="menu"
            style={{
              left: Math.min(menu.x, window.innerWidth - 200),
              top: Math.min(menu.y, window.innerHeight - 150),
            }}
          >
            <button
              role="menuitem"
              disabled={scanning}
              title={scanning ? 'Wait for the current scan to finish' : undefined}
              onClick={() => {
                setMenu(null)
                void rescanEntry(menu.id)
              }}
            >
              Rescan
            </button>
            <button
              role="menuitem"
              onClick={() => {
                setMenu(null)
                void revealEntry(menu.id)
              }}
            >
              Show in file manager
            </button>
            <button
              role="menuitem"
              onClick={() => {
                setMenu(null)
                void copyPath(menu.id)
              }}
            >
              Copy path
            </button>
            <button
              role="menuitem"
              className="menu-danger"
              onClick={() => {
                setMenu(null)
                void moveToTrash(menu.id, menu.name)
              }}
            >
              Move to trash
            </button>
          </div>
        </>
      )}
    </div>
  )
}

function Stat({
  label,
  value,
  strong,
  warn,
}: {
  label: string
  value: string
  strong?: boolean
  warn?: boolean
}) {
  return (
    <div className={'stat' + (strong ? ' stat-strong' : '') + (warn ? ' stat-warn' : '')}>
      <span className="stat-value">{value}</span>
      <span className="stat-label">{label}</span>
    </div>
  )
}
