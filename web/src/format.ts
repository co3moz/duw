const UNITS = ['B', 'KB', 'MB', 'GB', 'TB', 'PB']

/** Binary-scaled, `du -h` style. */
export function bytes(n: number): string {
  if (!Number.isFinite(n) || n <= 0) return '0 B'
  let i = 0
  let v = n
  while (v >= 1024 && i < UNITS.length - 1) {
    v /= 1024
    i++
  }
  const digits = i === 0 ? 0 : v < 10 ? 2 : v < 100 ? 1 : 0
  return `${v.toFixed(digits)} ${UNITS[i]}`
}

export function count(n: number): string {
  return n.toLocaleString()
}

export function duration(ms: number): string {
  if (ms < 1000) return `${ms} ms`
  const s = ms / 1000
  if (s < 60) return `${s.toFixed(1)} s`
  const m = Math.floor(s / 60)
  return `${m}m ${Math.round(s - m * 60)}s`
}

export function percent(part: number, whole: number): number {
  return whole > 0 ? (part / whole) * 100 : 0
}

export function mtime(unixSeconds: number): string {
  if (!unixSeconds) return '-'
  return new Date(unixSeconds * 1000).toLocaleDateString()
}

export type Category =
  | 'folder'
  | 'video'
  | 'audio'
  | 'image'
  | 'archive'
  | 'code'
  | 'document'
  | 'data'
  | 'binary'
  | 'other'

const BY_EXT: Record<string, Category> = {}
const register = (cat: Category, exts: string) => {
  for (const e of exts.split(' ')) BY_EXT[e] = cat
}

// `.ts` is deliberately absent: in a developer's tree it is TypeScript far
// more often than an MPEG transport stream.
register('video', 'mp4 mkv avi mov wmv flv webm m4v mpg mpeg vob ogv m2ts mts')
register('audio', 'mp3 wav flac aac ogg oga m4a wma opus aiff mid')
register('image', 'jpg jpeg png gif bmp svg webp tif tiff ico heic raw psd ai avif')
register('archive', 'zip rar 7z tar gz bz2 xz zst lz4 iso dmg cab pkg deb rpm jar war whl tgz txz tbz crate nupkg vsix xz')
register(
  'code',
  'js jsx ts tsx rs go py rb java kt swift c h cpp hpp cc cs php sh ps1 bat lua pl r m sql css scss less html htm vue svelte scala clj ex exs dart zig nim hs ml',
)
register('document', 'pdf doc docx xls xlsx ppt pptx odt ods odp txt md rtf tex epub mobi pages')
register(
  'data',
  'json yaml yml toml xml csv tsv db sqlite sqlite3 parquet avro proto ini cfg conf log ndjson pack idx wt turtle bson mdb frm ibd dat index lock map',
)
register(
  'binary',
  'exe dll so dylib bin o obj a lib pdb wasm apk msi node pyc pyd class rlib rmeta bc d elf ko sys dSYM',
)
// Virtual disks and VM images are large and worth their own bucket, but they
// behave like archives from a "what can I delete" point of view.
register('archive', 'qcow2 vmdk vdi vhd vhdx img ova ovf')

export function categoryOf(ext: string | null | undefined): Category {
  if (!ext) return 'other'
  return BY_EXT[ext.toLowerCase()] ?? 'other'
}

export const CATEGORY_LABEL: Record<Category, string> = {
  folder: 'Folders',
  video: 'Video',
  audio: 'Audio',
  image: 'Images',
  archive: 'Archives',
  code: 'Code',
  document: 'Documents',
  data: 'Data',
  binary: 'Binaries',
  other: 'Other',
}

export const CATEGORY_COLOR: Record<Category, string> = {
  folder: '#7f8ea3',
  video: '#e0658c',
  audio: '#c47ae0',
  image: '#4fa8d8',
  archive: '#e09a52',
  code: '#5cc8a0',
  document: '#d8c65a',
  data: '#6f8fe8',
  binary: '#a0a6b0',
  other: '#69737f',
}

/** Directories that usually hold regenerable data worth calling out. */
const RECLAIMABLE: Record<string, string> = {
  node_modules: 'npm packages',
  target: 'Rust build output',
  '.git': 'Git history',
  __pycache__: 'Python bytecode',
  '.venv': 'Python virtualenv',
  venv: 'Python virtualenv',
  dist: 'build output',
  build: 'build output',
  '.next': 'Next.js cache',
  '.cache': 'cache',
  '.gradle': 'Gradle cache',
  vendor: 'vendored deps',
  obj: 'build output',
  bin: 'build output',
}

export function reclaimableHint(name: string): string | undefined {
  return RECLAIMABLE[name]
}
