import { useCallback, useState } from 'react'
import { api } from '../api'
import { bytes, count, mtime } from '../format'
import { useResource } from '../useLive'

/**
 * Saved scans and the diff between one of them and the tree currently loaded.
 * The server keeps the files, so they survive across runs.
 */
export function Snapshots({ root }: { root: string }) {
  const [refresh, setRefresh] = useState(0)
  const [selected, setSelected] = useState<string | null>(null)

  const list = useResource((s) => api.snapshots(s), [refresh])
  const diff = useResource(
    (s) => (selected ? api.snapshotDiff(selected, 200, s) : Promise.resolve(null)),
    [selected, refresh],
  )

  const save = useCallback(async () => {
    const stamp = new Date().toISOString().slice(0, 19).replace(/[:T]/g, '-')
    const name = window.prompt('Snapshot name', `snapshot-${stamp}`)
    if (!name) return
    const res = await api.saveSnapshot(name)
    if (!res.ok) {
      window.alert(await res.text())
      return
    }
    setRefresh((n) => n + 1)
  }, [])

  const remove = useCallback(
    async (name: string) => {
      if (!window.confirm(`Delete snapshot "${name}"?`)) return
      const res = await api.deleteSnapshot(name)
      if (!res.ok) {
        window.alert(await res.text())
        return
      }
      if (selected === name) setSelected(null)
      setRefresh((n) => n + 1)
    },
    [selected],
  )

  return (
    <div className="snapshots">
      <div className="snap-bar">
        <button className="snap-save" onClick={save}>
          save current scan
        </button>
        <span className="snap-root" title={root}>
          {root}
        </span>
      </div>

      {!list.data ? (
        <p className="empty">loading…</p>
      ) : list.data.snapshots.length === 0 ? (
        <p className="empty">
          No snapshots yet. Save one now, then scan again later and compare to see what grew.
        </p>
      ) : (
        <ul className="rows snap-list">
          {list.data.snapshots.map((s) => (
            <li key={s.name} className={'snap-row' + (selected === s.name ? ' snap-active' : '')}>
              <div className="snap-info" title={s.root}>
                <span className="snap-name">{s.name}</span>
                <span className="snap-meta">
                  {mtime(s.created)} · {count(s.entries)} files · {bytes(s.bytes)}
                </span>
              </div>
              <button onClick={() => setSelected(selected === s.name ? null : s.name)}>
                {selected === s.name ? 'hide' : 'compare'}
              </button>
              <button className="menu-danger" onClick={() => void remove(s.name)}>
                delete
              </button>
            </li>
          ))}
        </ul>
      )}

      {selected && diff.data && (
        <div className="snap-diff">
          <div className="snap-summary">
            <span className="delta-up">+{bytes(diff.data.added_bytes)}</span>
            <span className="delta-down">−{bytes(diff.data.removed_bytes)}</span>
            <span className="snap-net">
              net {diff.data.net >= 0 ? '+' : '−'}
              {bytes(Math.abs(diff.data.net))}
            </span>
          </div>
          <div className="snap-meta">
            {count(diff.data.total_changes)} changes since {mtime(diff.data.from.created)}
          </div>
          {diff.data.changes.length === 0 ? (
            <p className="empty">Nothing changed.</p>
          ) : (
            <ul className="rows">
              {diff.data.changes.map((c) => (
                <li key={c.path} className="row snap-change" title={c.path}>
                  <span className="row-icon">{c.added ? '+' : c.removed ? '−' : '±'}</span>
                  <span className="row-name row-path">
                    <span className="path-base">{c.path}</span>
                  </span>
                  <span className="row-meta">
                    {c.added ? 'added' : c.removed ? 'removed' : 'changed'}
                  </span>
                  <span />
                  <span />
                  <span className={'row-size ' + (c.delta >= 0 ? 'delta-up' : 'delta-down')}>
                    {c.delta >= 0 ? '+' : '−'}
                    {bytes(Math.abs(c.delta))}
                  </span>
                  <span />
                </li>
              ))}
            </ul>
          )}
        </div>
      )}
    </div>
  )
}
