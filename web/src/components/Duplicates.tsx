import type { DupeGroup, DupeProgress } from '../api'
import { bytes, count, duration, percent } from '../format'

/** Thresholds offered in the UI; the CLI accepts any value. */
const THRESHOLDS = [
  { label: '64 KB', value: 65536 },
  { label: '512 KB', value: 524288 },
  { label: '1 MB', value: 1048576 },
  { label: '10 MB', value: 10485760 },
  { label: '100 MB', value: 104857600 },
]

const RUNNING: DupeProgress['phase'][] = ['grouping', 'windowing', 'hashing']

const PHASE_LABEL: Record<DupeProgress['phase'], string> = {
  idle: 'not started',
  grouping: 'grouping by size',
  windowing: 'comparing file edges',
  hashing: 'hashing candidates',
  done: 'done',
  cancelled: 'stopped',
}

export function Duplicates({
  progress,
  groups,
  truncated,
  totalGroups,
  minSize,
  scopeName,
  matchesScope,
  onMinSize,
  onScan,
  onCancel,
}: {
  progress: DupeProgress
  groups: DupeGroup[]
  truncated: boolean
  totalGroups: number
  minSize: number
  scopeName: string
  /** False when the last run was for a different folder than the one in view. */
  matchesScope: boolean
  onMinSize: (value: number) => void
  onScan: () => void
  onCancel: () => void
}) {
  const running = RUNNING.includes(progress.phase)
  const done = progress.phase === 'done' && matchesScope

  return (
    <div className="dupes">
      <div className="dupes-bar">
        <label className="dupes-min">
          smallest file
          <select
            value={minSize}
            onChange={(e) => onMinSize(Number(e.target.value))}
            disabled={running}
          >
            {THRESHOLDS.map((t) => (
              <option key={t.value} value={t.value}>
                {t.label}
              </option>
            ))}
          </select>
        </label>

        {running ? (
          <button className="stop" onClick={onCancel}>
            stop
          </button>
        ) : (
          <button className="dupes-run" onClick={onScan}>
            {done ? 'rescan' : `scan ${scopeName}`}
          </button>
        )}

        {done && (
          <span className="dupes-summary">
            <strong>{bytes(progress.wasted)}</strong> reclaimable in{' '}
            {count(progress.groups)} groups
          </span>
        )}
      </div>

      {running && (
        <div className="dupes-progress">
          <div className="dupes-phase">
            {PHASE_LABEL[progress.phase]} · {count(progress.candidates)} candidates ·{' '}
            {count(progress.read)} files read
          </div>
          <div className="bar">
            <div
              className="bar-fill"
              style={{
                width: `${Math.min(100, percent(progress.bytes_read, progress.bytes_total))}%`,
                background: 'var(--accent)',
              }}
            />
          </div>
          <div className="dupes-phase">
            {bytes(progress.bytes_read)} of {bytes(progress.bytes_total)}
          </div>
        </div>
      )}

      {!running && !matchesScope && (
        <p className="empty">
          Duplicate scanning reads file contents, so it is not run automatically.
        </p>
      )}

      {done && groups.length === 0 && (
        <p className="empty">
          No duplicates at or above this size. Try a smaller threshold.
        </p>
      )}

      {progress.phase === 'cancelled' && matchesScope && (
        <p className="empty">Stopped after {duration(progress.elapsed_ms)}.</p>
      )}

      {groups.length > 0 && (
        <ul className="rows dupe-groups">
          {groups.map((g, i) => (
            <li key={`${g.size}-${g.files[0]?.id ?? i}`} className="dupe-group">
              <div className="dupe-head">
                <span className="dupe-count">{g.files.length} copies</span>
                <span className="dupe-each">{bytes(g.size)} each</span>
                <span className="dupe-wasted">{bytes(g.wasted)} wasted</span>
              </div>
              <ul className="dupe-files">
                {g.files.map((f) => (
                  <li key={f.id} title={f.path}>
                    {f.path}
                  </li>
                ))}
              </ul>
            </li>
          ))}
        </ul>
      )}

      {truncated && (
        <p className="empty">
          Showing the {count(groups.length)} largest of {count(totalGroups)} groups.
        </p>
      )}
    </div>
  )
}
