import { useCallback, useEffect, useState } from 'react'
import { api, type Metric } from './api'
import { bytes, count, duration } from './format'
import { useLive, useResource, useThrottled } from './useLive'
import { Treemap } from './components/Treemap'
import { FolderRows, LargestRows, TypeRows } from './components/Rows'
import { Duplicates } from './components/Duplicates'

type Tab = 'folders' | 'types' | 'largest' | 'duplicates'

const LIST_LIMIT = 500
const MAP_DEPTH = 3
const MAP_PER_LEVEL = 60
/** Refetch at most this often while a scan is streaming in. */
const REFRESH_MS = 600

export default function App() {
  const { state, progress, error } = useLive()
  const [nodeId, setNodeId] = useState(0)
  const [metric, setMetric] = useState<Metric>('size')
  const [tab, setTab] = useState<Tab>('folders')
  const [selected, setSelected] = useState<number | null>(null)

  const version = useThrottled(progress?.version ?? 0, REFRESH_MS)

  const node = useResource(
    (s) => api.node(nodeId, metric, LIST_LIMIT, s),
    [nodeId, metric, version],
  )
  const map = useResource(
    (s) => api.tree(nodeId, metric, MAP_DEPTH, MAP_PER_LEVEL, s),
    [nodeId, metric, version],
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
    (min: number) => {
      setMinSize(min)
      api.startDuplicates(nodeId, min)
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

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Backspace' || e.key === 'Escape') {
        e.preventDefault()
        up()
      }
    }
    window.addEventListener('keydown', onKey)
    return () => window.removeEventListener('keydown', onKey)
  }, [up])

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

      <main className="split">
        <section className="panel panel-list">
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
          </div>
          <div className="panel-body">
            {tab === 'folders' && (
              <FolderRows
                entries={view.children}
                other={view.other}
                total={total}
                metric={metric}
                selected={selected}
                onSelect={setSelected}
                onOpen={open}
                onUp={view.breadcrumb.length > 1 ? up : undefined}
              />
            )}
            {tab === 'types' &&
              (types.data ? (
                <TypeRows types={types.data.types} metric={metric} total={total} />
              ) : (
                <p className="empty">aggregating…</p>
              ))}
            {tab === 'largest' &&
              (largest.data ? (
                <LargestRows files={largest.data.files} metric={metric} total={total} />
              ) : (
                <p className="empty">aggregating…</p>
              ))}
            {tab === 'duplicates' && dupes && (
              <Duplicates
                progress={dupes}
                groups={dupeResults.data?.groups ?? []}
                truncated={dupeResults.data?.truncated ?? false}
                totalGroups={dupeResults.data?.total_groups ?? 0}
                minSize={effectiveMin}
                scopeName={view.name}
                matchesScope={dupes.scope === nodeId && dupes.phase !== 'idle'}
                onMinSize={runDupes}
                onScan={() => runDupes(effectiveMin)}
                onCancel={() => api.cancelDuplicates()}
              />
            )}
          </div>
        </section>

        <section className="panel panel-map">
          {map.data ? (
            <Treemap root={map.data.root} metric={metric} onOpen={open} selected={selected} />
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
