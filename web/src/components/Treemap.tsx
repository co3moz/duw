import { useMemo, useRef, useState } from 'react'
import { hierarchy, treemap, treemapSquarify, type HierarchyRectangularNode } from 'd3-hierarchy'
import { filterActive, type FilterSpec, type Metric, type SubtreeNode } from '../api'
import { CATEGORY_COLOR, ageColor, bytes, categoryOf, percent } from '../format'
import { useElementSize } from '../useElementSize'
import type { MenuHandler } from './Rows'

/** Leaf-only value carrier; `rest` stands for the entries the server trimmed. */
interface Cell {
  id: number
  name: string
  kind: SubtreeNode['kind'] | 'rest'
  value: number
  mtime: number
  children?: Cell[]
}

const HEADER = 15
const MIN_LABEL_W = 44
const MIN_LABEL_H = 16

export function Treemap({
  root,
  metric,
  onOpen,
  selected,
  onMenu,
  filter,
  heat,
}: {
  root: SubtreeNode
  metric: Metric
  onOpen: (id: number) => void
  selected: number | null
  onMenu?: MenuHandler
  filter?: FilterSpec
  heat?: boolean
}) {
  const [box, ref] = useElementSize<HTMLDivElement>()
  const [hover, setHover] = useState<HierarchyRectangularNode<Cell> | null>(null)
  const wrap = useRef<HTMLDivElement>(null)

  const cells = useMemo(() => {
    const tree = filter && filterActive(filter) ? prune(root, filter, metric) : root
    return tree ? toCells(tree, metric) : null
  }, [root, metric, filter])

  const layout = useMemo(() => {
    if (!cells) return null
    const w = Math.max(box.width, 1)
    const h = Math.max(box.height, 1)
    const h1 = hierarchy<Cell>(cells, (d) => d.children)
      .sum((d) => (d.children && d.children.length ? 0 : d.value))
      .sort((a, b) => (b.value ?? 0) - (a.value ?? 0))
    return treemap<Cell>()
      .tile(treemapSquarify)
      .size([w, h])
      .paddingInner(1)
      .paddingTop((d) => (d.depth > 0 && d.children ? HEADER : 0))
      .round(true)(h1)
  }, [cells, box.width, box.height])

  if (!layout) {
    return (
      <div className="treemap" ref={ref}>
        <div className="empty">No matches in this map.</div>
      </div>
    )
  }

  const total = layout.value ?? 0
  const nodes = layout.descendants().filter((d) => d.depth > 0 && d.x1 - d.x0 > 1.5 && d.y1 - d.y0 > 1.5)

  return (
    <div className="treemap" ref={ref}>
      <div ref={wrap} className="treemap-inner">
        <svg width={box.width} height={box.height} role="img" aria-label="Treemap of disk usage">
          {nodes.map((d) => {
            const w = d.x1 - d.x0
            const h = d.y1 - d.y0
            const isDir = !!d.data.children?.length
            const fill = fillOf(d.data, isDir, !!heat)
            const label = w > MIN_LABEL_W && h > MIN_LABEL_H
            return (
              <g
                key={`${d.data.id}-${d.depth}-${d.x0}-${d.y0}`}
                transform={`translate(${d.x0},${d.y0})`}
                className={
                  'cell' +
                  (isDir ? ' cell-dir' : '') +
                  (d.data.id === selected ? ' cell-selected' : '') +
                  (d.data.kind === 'rest' ? ' cell-rest' : '')
                }
                onMouseEnter={() => setHover(d)}
                onMouseLeave={() => setHover((cur) => (cur === d ? null : cur))}
                onClick={() => {
                  if (d.data.kind === 'dir') onOpen(d.data.id)
                }}
                onContextMenu={(ev) => {
                  if (onMenu && d.data.kind !== 'rest' && d.data.id >= 0) {
                    ev.preventDefault()
                    onMenu(d.data.id, d.data.name, ev)
                  }
                }}
              >
                <rect width={w} height={h} rx={2} fill={fill} />
                {label &&
                  (isDir ? (
                    <text className="cell-label cell-label-dir" x={4} y={11}>
                      {clip(d.data.name, w - 8)}
                    </text>
                  ) : (
                    <>
                      <text className="cell-label" x={4} y={12}>
                        {clip(d.data.name, w - 8)}
                      </text>
                      {h > 30 && (
                        <text className="cell-sub" x={4} y={24}>
                          {bytes(d.value ?? 0)}
                        </text>
                      )}
                    </>
                  ))}
              </g>
            )
          })}
        </svg>
      </div>
      {hover && (
        <div className="tooltip" style={tooltipStyle(hover, box)}>
          <div className="tooltip-name">{hover.ancestors().reverse().slice(1).map((a) => a.data.name).join(' / ')}</div>
          <div className="tooltip-meta">
            {bytes(hover.value ?? 0)} · {percent(hover.value ?? 0, total).toFixed(1)}%
          </div>
        </div>
      )}
    </div>
  )
}

function colorOf(cell: Cell): string {
  if (cell.kind === 'rest') return 'var(--map-rest)'
  if (cell.kind === 'dir') return CATEGORY_COLOR.folder
  const dot = cell.name.lastIndexOf('.')
  const ext = dot > 0 ? cell.name.slice(dot + 1) : null
  return CATEGORY_COLOR[categoryOf(ext)]
}

function fillOf(cell: Cell, isDir: boolean, heat: boolean): string {
  if (cell.kind === 'rest') return 'var(--map-rest)'
  if (heat) return ageColor(cell.mtime)
  return isDir ? 'var(--map-dir)' : colorOf(cell)
}

/**
 * The API returns only the biggest children per level, so a directory's own
 * total is usually larger than the sum of what came back. The difference
 * becomes an explicit "rest" cell instead of silently inflating its siblings.
 */
function toCells(node: SubtreeNode, metric: Metric): Cell {
  const value = metric === 'alloc' ? node.alloc : node.size
  const kids = node.children ?? []
  if (!kids.length) {
    return { id: node.id, name: node.name, kind: node.kind, value, mtime: node.mtime }
  }
  const children = kids.map((k) => toCells(k, metric))
  const covered = children.reduce((a, c) => a + c.value, 0)
  const rest = value - covered
  if (rest > 0 && rest > value * 0.005) {
    children.push({
      id: -1,
      name: `… ${bytes(rest)} more`,
      kind: 'rest',
      value: rest,
      mtime: 0,
    })
  }
  return { id: node.id, name: node.name, kind: node.kind, value, mtime: node.mtime, children }
}

/**
 * Keeps only the cells that pass the filter. A directory survives when it
 * matches itself or when any descendant does; a surviving directory's value
 * becomes the sum of what is left, so the map shows matching bytes only.
 */
function prune(node: SubtreeNode, f: FilterSpec, metric: Metric): SubtreeNode | null {
  const kids = (node.children ?? [])
    .map((c) => prune(c, f, metric))
    .filter((c): c is SubtreeNode => c !== null)
  if (kids.length) {
    return {
      ...node,
      size: kids.reduce((a, c) => a + c.size, 0),
      alloc: kids.reduce((a, c) => a + c.alloc, 0),
      children: kids,
    }
  }
  return matches(node, f, metric) ? { ...node, children: [] } : null
}

function matches(node: SubtreeNode, f: FilterSpec, metric: Metric): boolean {
  if (f.q && !node.name.toLowerCase().includes(f.q)) return false
  if (f.exts.length > 0) {
    const dot = node.name.lastIndexOf('.')
    const ext = dot > 0 ? node.name.slice(dot + 1).toLowerCase() : ''
    if (!f.exts.includes(ext)) return false
  }
  const v = metric === 'alloc' ? node.alloc : node.size
  if (v < f.min || v > f.max) return false
  if (f.maxMtime > 0 && node.mtime > f.maxMtime) return false
  return true
}

function clip(text: string, width: number): string {
  const max = Math.max(0, Math.floor(width / 6.1))
  if (text.length <= max) return text
  if (max <= 1) return ''
  return text.slice(0, max - 1) + '…'
}

function tooltipStyle(
  d: HierarchyRectangularNode<Cell>,
  box: { width: number; height: number },
): React.CSSProperties {
  const x = Math.min(d.x0 + 6, Math.max(0, box.width - 240))
  const flipUp = d.y0 > box.height - 60
  return flipUp
    ? { left: x, bottom: box.height - d.y0 + 6 }
    : { left: x, top: Math.min(d.y1 + 6, box.height - 46) }
}
