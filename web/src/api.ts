export type Kind = 'dir' | 'file' | 'link' | 'other'
export type Metric = 'size' | 'alloc'

export interface Stats {
  files: number
  dirs: number
  size: number
  alloc: number
  errors: number
  skipped: number
  hardlinks: number
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
  children?: SubtreeNode[]
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

async function get<T>(url: string, signal?: AbortSignal): Promise<T> {
  const res = await fetch(url, { signal })
  if (!res.ok) throw new Error(`${res.status} ${res.statusText}`)
  return (await res.json()) as T
}

export const api = {
  state: (signal?: AbortSignal) => get<FullState>('/api/state', signal),

  node: (id: number, metric: Metric, limit: number, signal?: AbortSignal) =>
    get<NodeView>(`/api/node/${id}?metric=${metric}&limit=${limit}`, signal),

  tree: (id: number, metric: Metric, depth: number, limit: number, signal?: AbortSignal) =>
    get<{ root: SubtreeNode; version: number; truncated: boolean }>(
      `/api/tree/${id}?metric=${metric}&depth=${depth}&limit=${limit}`,
      signal,
    ),

  types: (id: number, signal?: AbortSignal) =>
    get<{ types: ExtStat[]; version: number }>(`/api/types/${id}`, signal),

  largest: (id: number, metric: Metric, limit: number, signal?: AbortSignal) =>
    get<{ files: LargeFile[]; version: number }>(
      `/api/largest/${id}?metric=${metric}&limit=${limit}`,
      signal,
    ),

  errors: (signal?: AbortSignal) => get<ScanError[]>('/api/errors', signal),

  cancel: () => fetch('/api/cancel', { method: 'POST' }),
}
