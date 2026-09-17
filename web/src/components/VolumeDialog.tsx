import { useEffect, useRef, useState } from 'react'
import { api, type VolumeSummary } from '../api'
import { bytes, percent } from '../format'
import { capacitySlices, fileSlices } from '../volumeChart'
import { Pie3D } from './Pie3D'

export function VolumeDialog({ approximate, onDismiss }: { approximate: boolean; onDismiss: () => void }) {
  const dialog = useRef<HTMLDialogElement>(null)
  const [summary, setSummary] = useState<VolumeSummary | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [selected, setSelected] = useState<string | null>(null)
  const [mode, setMode] = useState<'capacity' | 'types'>('capacity')
  const [refresh, setRefresh] = useState(0)
  const [loading, setLoading] = useState(true)

  useEffect(() => {
    const element = dialog.current!
    element.showModal()
    return () => element.close()
  }, [])

  useEffect(() => {
    const controller = new AbortController()
    let timer: ReturnType<typeof setTimeout>
    const load = async () => {
      try {
        const data = await api.volumes(controller.signal)
        if (controller.signal.aborted) return
        setSummary(data)
        setError(null)
        // Poll only while open. A slow request cannot overlap the next one.
        timer = setTimeout(load, 5000)
      } catch (e) {
        if (!controller.signal.aborted) setError(String(e))
      } finally {
        if (!controller.signal.aborted) setLoading(false)
      }
    }
    setLoading(true)
    void load()
    return () => { controller.abort(); clearTimeout(timer) }
  }, [refresh])

  const volume = summary?.volumes.find(v => v.mount_point === selected) ?? summary?.volumes[0]
  const slices = volume ? (mode === 'capacity' ? capacitySlices(volume) : fileSlices(volume)) : []
  const close = () => dialog.current?.close()
  return (
    <dialog
      ref={dialog}
      className="volume-dialog"
      aria-labelledby="volume-title"
      onClose={() => { if (!dialog.current?.open) onDismiss() }}
      onKeyDown={e => e.stopPropagation()}
      onCancel={e => e.stopPropagation()}
      onClick={e => {
        if (e.target !== e.currentTarget) return
        const box = e.currentTarget.getBoundingClientRect()
        if (e.clientX < box.left || e.clientX > box.right || e.clientY < box.top || e.clientY > box.bottom) close()
      }}
    >
      <div className="volume-popup">
        <header className="volume-head">
          <div>
            <h2 id="volume-title">Disks & volumes</h2>
            <p>Storage capacity and the space accounted for by this scan.</p>
          </div>
          <div className="volume-head-actions">
            <button className="volume-refresh" disabled={loading} onClick={() => setRefresh(n => n + 1)}>{loading ? 'refreshing…' : 'refresh'}</button>
            <button className="volume-close" aria-label="Close disk summary" autoFocus onClick={close}>×</button>
          </div>
        </header>
        {error && <p className="volume-error" role="alert">Could not read disk details. {error}</p>}
        {!summary && !error && <p className="empty">Reading volume information…</p>}
        {summary && !summary.volumes.length && <p className="empty">No mounted volumes reported by this system.</p>}
        {summary && volume && (
          <div className="volume-body">
            <aside className="volume-list" aria-label="Mounted volumes">
              <div className="volume-list-label">Mounted volumes · {summary.volumes.length}</div>
              {summary.volumes.map(v => (
                <button key={v.mount_point} className={`volume-card${v.mount_point === volume.mount_point ? ' on' : ''}`} aria-pressed={v.mount_point === volume.mount_point} onClick={() => setSelected(v.mount_point)}>
                  <span className="volume-card-title"><span>{v.mount_point}</span>{v.contains_root && <small>scan root</small>}</span>
                  <span className="volume-card-name">{v.name || 'Volume'}</span>
                  <span className="volume-meter" aria-hidden="true"><i style={{ width: `${percent(v.used, v.total)}%` }} /></span>
                  <span className="volume-card-usage"><strong>{percent(v.used, v.total).toFixed(1)}% used</strong><span>{bytes(v.used)} / {bytes(v.total)}</span></span>
                </button>
              ))}
            </aside>
            <section className="volume-details" aria-label="Selected volume details">
              <div className="volume-identity">
                <h3>{volume.name || volume.mount_point}</h3>
                <span>{volume.kind}{volume.removable ? ' · removable' : ''}</span>
              </div>
              <dl className="volume-metadata">
                <div><dt>Mount point</dt><dd>{volume.mount_point}</dd></div>
                <div><dt>File system</dt><dd>{volume.file_system || 'Unknown'}</dd></div>
              </dl>
              <dl className="volume-stats">
                <div><dt>Total capacity</dt><dd>{bytes(volume.total)}</dd></div>
                <div><dt>Used space</dt><dd>{bytes(volume.used)}</dd></div>
                <div><dt>Available space</dt><dd>{bytes(volume.available)}</dd></div>
              </dl>
              <div className="volume-root-share">
                <div><span>{volume.contains_root ? 'Scan root on this volume' : 'Scanned data on this volume'}{approximate ? '*' : ''}</span><strong>{bytes(volume.scanned_alloc)} <small>· {percent(volume.scanned_alloc, volume.total).toFixed(2)}% of capacity</small></strong></div>
                <span className="volume-meter" aria-hidden="true"><i style={{ width: `${Math.min(100, percent(volume.scanned_alloc, volume.total))}%` }} /></span>
                <p title={summary.root}>{summary.root}</p>
              </div>
              <div className="volume-chart-head">
                <div className="toggle" role="group" aria-label="Disk chart view">
                  <button className={mode === 'capacity' ? 'on' : ''} aria-pressed={mode === 'capacity'} onClick={() => setMode('capacity')}>disk usage</button>
                  <button className={mode === 'types' ? 'on' : ''} aria-pressed={mode === 'types'} onClick={() => setMode('types')}>scanned file types</button>
                </div>
                <span>3D pie · {mode === 'capacity' ? '% of volume' : '% of scanned files'}</span>
              </div>
              <Pie3D key={`${volume.mount_point}-${mode}`} slices={slices} label={mode === 'capacity' ? 'Volume capacity breakdown' : 'Scanned file categories by on-disk size'} />
              <div className="volume-notes">
                <p>File categories cover only the scanned root, including its subfolders. Other used space includes everything outside this scan and filesystem overhead.</p>
                {summary.scanning && <p className="note">Scan in progress — category totals are partial and update automatically.</p>}
                {approximate && <p>* Scanned on-disk sizes are estimated from cluster size on Windows.</p>}
                {volume.scanned_alloc > volume.used && <p className="note">Recorded scan bytes exceed current used space. The capacity chart shows OS totals; scanned file types are available separately.</p>}
                {summary.unattributed_alloc > 0 && <p>{bytes(summary.unattributed_alloc)} of scanned data could not be matched to a reported volume.</p>}
              </div>
            </section>
          </div>
        )}
      </div>
    </dialog>
  )
}
