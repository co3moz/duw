import { useCallback, useEffect, useRef, useState } from 'react'

export interface Size {
  width: number
  height: number
}

/** Tracks an element's content box; returns `[size, ref]`. */
export function useElementSize<T extends HTMLElement>(): [Size, (node: T | null) => void] {
  const [size, setSize] = useState<Size>({ width: 0, height: 0 })
  const observer = useRef<ResizeObserver | null>(null)

  const ref = useCallback((node: T | null) => {
    observer.current?.disconnect()
    if (!node) return
    const ro = new ResizeObserver(([entry]) => {
      const box = entry.contentRect
      setSize({ width: Math.round(box.width), height: Math.round(box.height) })
    })
    ro.observe(node)
    observer.current = ro
    setSize({ width: node.clientWidth, height: node.clientHeight })
  }, [])

  useEffect(() => () => observer.current?.disconnect(), [])

  return [size, ref]
}
