import type { Volume } from './api.ts'
import { CATEGORY_COLOR, CATEGORY_LABEL, categoryOf } from './format.ts'

export interface PieSlice {
  id: string
  label: string
  color: string
  value: number
}

/** Extension categories describe only files present in the existing scan. */
export function fileSlices(volume: Volume): PieSlice[] {
  const categories = new Map<string, PieSlice>()
  for (const stat of volume.types) {
    if (stat.alloc <= 0) continue
    const category = categoryOf(stat.ext)
    const slice = categories.get(category) ?? {
      id: category, label: CATEGORY_LABEL[category], color: CATEGORY_COLOR[category], value: 0,
    }
    slice.value += stat.alloc
    categories.set(category, slice)
  }
  return [...categories.values()].sort((a, b) => b.value - a.value)
}

/** Never imply that unscanned disk contents have been categorized. */
export function capacitySlices(volume: Volume): PieSlice[] {
  const files = fileSlices(volume)
  const overhead = Math.max(0, volume.scanned_alloc - files.reduce((n, s) => n + s.value, 0))
  // Recorded allocations can exceed actual usage (e.g. shared blocks, links,
  // or an external change). In that case keep OS capacity figures intact.
  const slices: PieSlice[] = volume.scanned_alloc <= volume.used
    ? [
        ...files,
        { id: 'overhead', label: 'Folders & links', color: '#7f8ea3', value: overhead },
        { id: 'unscanned', label: 'Other used space', color: '#596477', value: volume.used - volume.scanned_alloc },
      ]
    : [{ id: 'used', label: 'Used space', color: '#7f8ea3', value: volume.used }]
  slices.push({ id: 'free', label: 'Available space', color: '#c3d9d0', value: volume.available })
  return slices.filter(s => s.value > 0)
}

export interface PieArc extends PieSlice {
  start: number
  end: number
}

export function pieArcs(slices: PieSlice[]): PieArc[] {
  const positive = slices.filter(s => s.value > 0 && Number.isFinite(s.value))
  const total = positive.reduce((n, s) => n + s.value, 0)
  let angle = -Math.PI / 2
  return positive.map((slice, index) => {
    const start = angle
    angle = index === positive.length - 1 ? Math.PI * 1.5 : angle + slice.value / total * Math.PI * 2
    return { ...slice, start, end: angle }
  })
}

const CX = 190
const CY = 118
const RX = 156
const RY = 88
const DEPTH = 26
function point(angle: number, offset = 0): string {
  return `${(CX + RX * Math.cos(angle)).toFixed(4)},${(CY + RY * Math.sin(angle) + offset).toFixed(4)}`
}

export function topPath(arc: PieArc): string {
  const start = point(arc.start)
  const end = point(arc.end)
  if (arc.end - arc.start >= Math.PI * 2 - 1e-9) {
    return `M ${start} A ${RX},${RY} 0 1 1 ${point(arc.start + Math.PI)} A ${RX},${RY} 0 1 1 ${end} Z`
  }
  return `M ${CX},${CY} L ${start} A ${RX},${RY} 0 ${arc.end - arc.start > Math.PI ? 1 : 0} 1 ${end} Z`
}

/** Only the front half of the extruded rim is visible from above. */
export function sidePaths(arc: PieArc): string[] {
  const paths: string[] = []
  for (let turn = -1; turn <= 1; turn++) {
    const start = Math.max(arc.start, turn * Math.PI * 2)
    const end = Math.min(arc.end, turn * Math.PI * 2 + Math.PI)
    if (end - start <= 1e-9) continue
    paths.push(`M ${point(start)} A ${RX},${RY} 0 0 1 ${point(end)} L ${point(end, DEPTH)} A ${RX},${RY} 0 0 0 ${point(start, DEPTH)} Z`)
  }
  return paths
}

export function shade(hex: string, factor: number): string {
  const values = [1, 3, 5].map(index => Math.round(parseInt(hex.slice(index, index + 2), 16) * factor))
  return `rgb(${values.join(',')})`
}
