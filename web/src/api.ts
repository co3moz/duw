export type Kind = 'dir' | 'file' | 'link' | 'other'
export type Metric = 'size' | 'alloc'
export type SortKey = 'size' | 'name' | 'mtime' | 'count'

export interface Stats {
  files: number
  dirs: number
  size: number
  alloc: number
  errors: number
  skipped: number
  hardlinks: number
}

export type DupePhase = 'idle' | 'grouping' | 'windowing' | 'hashing' | 'done' | 'cancelled'

export interface DupeProgress {
  phase: DupePhase
  scope: number
  min_size: number
  candidates: number
  /** Files whose metadata has been refreshed so far while grouping. */
  checked: number
  /** Files grouping will refresh in total. */
  files_total: number
  read: number
  bytes_read: number
  bytes_total: number
  groups: number
  wasted: number
  elapsed_ms: number
  generation: number
}

export interface DupeFile {
  id: number
  path: string
}

export interface DupeGroup {
  size: number
  wasted: number
  files: DupeFile[]
}

export interface Progress {
  version: number
  scanning: boolean
  cancelled: boolean
  elapsed_ms: number
  current: string
  stats: Stats
  root_size: number
  root_alloc: number
  dupes: DupeProgress
}

export interface Platform {
  os: string
  one_file_system: boolean
  hardlink_dedup: boolean
  approximate_alloc: boolean
}

export interface FullState extends Progress {
  root: string
  root_id: number
  platform: Platform
  local_only: boolean
  dupes_min: number
}

export interface Entry {
  id: number
  name: string
  kind: Kind
  size: number
  alloc: number
  files: number
  dirs: number
  mtime: number
  read: boolean
  err: boolean
  /// Hidden because the bytes live in the cloud, not on this disk.
  cloud: boolean
  ext: string | null
}

export interface Crumb {
  id: number
  name: string
}

export interface Rollup {
  count: number
  size: number
  alloc: number
}

export interface NodeView {
  id: number
  name: string
  path: string
  breadcrumb: Crumb[]
  size: number
  alloc: number
  files: number
  dirs: number
  mtime: number
  read: boolean
  children: Entry[]
  other: Rollup
  version: number
  scanning: boolean
}

export interface SubtreeNode {
  id: number
  name: string
  kind: Kind
  size: number
  alloc: number
  mtime: number
  children?: SubtreeNode[]
}

export interface SearchHit {
  id: number
  parent: number
  name: string
  kind: Kind
  path: string
  size: number
  alloc: number
  mtime: number
  ext: string | null
}

/** Filter used by the search endpoint and the treemap. */
export interface FilterSpec {
  /** Lowercased name substring. */
  q: string
  /** Lowercased extensions, without the dot. */
  exts: string[]
  min: number
  max: number
  /** Unix seconds; `0` means no age bound. */
  maxMtime: number
}

export function filterActive(f: FilterSpec): boolean {
  return f.q !== '' || f.exts.length > 0 || f.min > 0 || f.max < Number.MAX_SAFE_INTEGER || f.maxMtime > 0
}

export interface ExtStat {
  ext: string
  size: number
  alloc: number
  count: number
}

export interface LargeFile {
  id: number
  path: string
  size: number
  alloc: number
  mtime: number
}

export interface ScanError {
  path: string
  message: string
}

export interface SnapshotMeta {
  name: string
  root: string
  created: number
  entries: number
  /** Sum of the scanned file sizes the snapshot describes. */
  bytes: number
  /** Size of the snapshot file itself on disk. */
  file_bytes: number
}

export interface SnapshotChange {
  path: string
  kind: Kind
  old: number
  new: number
  delta: number
  added: boolean
  removed: boolean
}

export interface SnapshotDiff {
  from: SnapshotMeta
  to_root: string
  to_created: number
  added_bytes: number
  removed_bytes: number
  net: number
  total_changes: number
  changes: SnapshotChange[]
}

export class HttpError extends Error {
  readonly status: number

  constructor(status: number, message: string) {
    super(message)
    this.status = status
  }
}

export interface SearchOptions {
  q: string
  ext: string
  min?: number
  max?: number
  age?: number
}

async function get<T>(url: string, signal?: AbortSignal): Promise<T> {
  const res = await fetch(url, { signal })
  if (!res.ok) throw new HttpError(res.status, `${res.status} ${res.statusText}`)
  return (await res.json()) as T
}

export const api = {
  state: (signal?: AbortSignal) => get<FullState>('/api/state', signal),

  node: (id: number, metric: Metric, limit: number, sort: SortKey, asc: boolean, signal?: AbortSignal) =>
    get<NodeView>(
      `/api/node/${id}?metric=${metric}&limit=${limit}&sort=${sort}&asc=${asc}`,
      signal,
    ),

  tree: (id: number, metric: Metric, depth: number, limit: number, signal?: AbortSignal, filter?: SearchOptions) =>
    get<{ root: SubtreeNode; version: number; truncated: boolean }>(
      `/api/tree/${id}?metric=${metric}&depth=${depth}&limit=${limit}${filter ? '&' + searchQuery(filter) : ''}`,
      signal,
    ),

  types: (id: number, signal?: AbortSignal) =>
    get<{ types: ExtStat[]; version: number }>(`/api/types/${id}`, signal),

  largest: (id: number, metric: Metric, limit: number, signal?: AbortSignal) =>
    get<{ files: LargeFile[]; version: number }>(
      `/api/largest/${id}?metric=${metric}&limit=${limit}`,
      signal,
    ),

  search: (
    id: number,
    params: SearchOptions & {
      metric: Metric
      limit: number
    },
    signal?: AbortSignal,
  ) => {
    const qs = searchQuery(params)
    qs.set('metric', params.metric)
    qs.set('limit', String(params.limit))
    return get<{ hits: SearchHit[]; version: number; truncated: boolean }>(
      `/api/search/${id}?${qs.toString()}`,
      signal,
    )
  },

  errors: (signal?: AbortSignal) => get<ScanError[]>('/api/errors', signal),

  snapshots: (signal?: AbortSignal) =>
    get<{ snapshots: SnapshotMeta[]; dir: string }>('/api/snapshots', signal),

  saveSnapshot: (name: string) =>
    fetch(`/api/snapshots?name=${encodeURIComponent(name)}`, { method: 'POST' }),

  deleteSnapshot: (name: string) =>
    fetch(`/api/snapshots/${encodeURIComponent(name)}`, { method: 'DELETE' }),

  snapshotDiff: (name: string, limit: number, signal?: AbortSignal) =>
    get<SnapshotDiff>(`/api/snapshots/${encodeURIComponent(name)}/diff?limit=${limit}`, signal),

  cancel: () => fetch('/api/cancel', { method: 'POST' }),

  abs: (id: number, signal?: AbortSignal) =>
    get<{ path: string }>(`/api/abs/${id}`, signal),

  reveal: (id: number) => fetch(`/api/reveal/${id}`, { method: 'POST' }),

  trash: (id: number) => fetch(`/api/trash/${id}`, { method: 'POST' }),

  rescan: (id: number) => fetch(`/api/rescan/${id}`, { method: 'POST' }),

  duplicates: (id: number, limit: number, signal?: AbortSignal) =>
    get<{
      progress: DupeProgress
      groups: DupeGroup[]
      total_groups: number
      truncated: boolean
    }>(`/api/duplicates/${id}?limit=${limit}`, signal),

  startDuplicates: (id: number, min: number) =>
    fetch(`/api/duplicates/${id}?min=${min}`, { method: 'POST' }),

  cancelDuplicates: () => fetch('/api/duplicates/cancel', { method: 'POST' }),
}

function searchQuery(params: SearchOptions): URLSearchParams {
  const qs = new URLSearchParams()
  if (params.q) qs.set('q', params.q)
  if (params.ext) qs.set('ext', params.ext)
  if (params.min != null) qs.set('min', String(params.min))
  if (params.max != null) qs.set('max', String(params.max))
  if (params.age != null) qs.set('age', String(params.age))
  return qs
}
