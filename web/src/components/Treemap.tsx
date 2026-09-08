import { useMemo, useRef, useState } from 'react'
import { hierarchy, treemap, treemapSquarify, type HierarchyRectangularNode } from 'd3-hierarchy'
import type { Metric, SubtreeNode } from '../api'
import { CATEGORY_COLOR, bytes, categoryOf, percent } from '../format'
import { useElementSize } from '../useElementSize'

/** Leaf-only value carrier; `rest` stands for the entries the server trimmed. */
interface Cell {
  id: number
  name: string
  kind: SubtreeNode['kind'] | 'rest'
  value: number
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
}: {
  root: SubtreeNode
  metric: Metric
  onOpen: (id: number) => void
  selected: number | null
}) {
  const [box, ref] = useElementSize<HTMLDivElement>()
  const [hover, setHover] = useState<HierarchyRectangularNode<Cell> | null>(null)
  const wrap = useRef<HTMLDivElement>(null)

  const cells = useMemo(() => toCells(root, metric), [root, metric])

  const layout = useMemo(() => {
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
            const fill = isDir ? 'var(--map-dir)' : colorOf(d.data)
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

/**
 * The API returns only the biggest children per level, so a directory's own
 * total is usually larger than the sum of what came back. The difference
 * becomes an explicit "rest" cell instead of silently inflating its siblings.
 */
function toCells(node: SubtreeNode, metric: Metric): Cell {
  const value = metric === 'alloc' ? node.alloc : node.size
  const kids = node.children ?? []
  if (!kids.length) {
    return { id: node.id, name: node.name, kind: node.kind, value }
  }
  const children = kids.map((k) => toCells(k, metric))
  const covered = children.reduce((a, c) => a + c.value, 0)
  const rest = value - covered
  if (rest > 0 && rest > value * 0.005) {
    children.push({ id: -1, name: `… ${bytes(rest)} more`, kind: 'rest', value: rest })
  }
  return { id: node.id, name: node.name, kind: node.kind, value, children }
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
