import { test } from 'node:test'
import assert from 'node:assert/strict'
import { capacitySlices, fileSlices, pieArcs, sidePaths, topPath } from '../src/volumeChart.ts'

const volume = {
  scanned_alloc: 320, used: 700, available: 300, total: 1000,
  types: [
    { ext: 'mp4', alloc: 200 },
    { ext: 'mkv', alloc: 50 },
    { ext: 'txt', alloc: 50 },
    { ext: 'bin', alloc: 0 },
  ],
}

test('capacity chart balances categories, scan overhead, unscanned usage and free space', () => {
  const slices = capacitySlices(volume)
  assert.equal(slices.reduce((n, s) => n + s.value, 0), volume.total)
  assert.equal(slices.find(s => s.id === 'video').value, 250)
  assert.equal(slices.find(s => s.id === 'overhead').value, 20)
  assert.equal(slices.find(s => s.id === 'unscanned').value, 380)
  assert.equal(slices.find(s => s.id === 'free').value, 300)
  assert.equal(fileSlices(volume).reduce((n, s) => n + s.value, 0), 300)
})

test('unscanned volumes and allocations exceeding OS usage never invent categories or distort capacity', () => {
  assert.deepEqual(capacitySlices({ ...volume, scanned_alloc: 0, types: [] }).map(s => s.value), [700, 300])
  assert.deepEqual(capacitySlices({ ...volume, scanned_alloc: 800 }).map(s => s.value), [700, 300])
  assert.deepEqual(capacitySlices({ ...volume, scanned_alloc: 0, used: 0, available: 1000, types: [] }).map(s => s.value), [1000])
})

test('3D pie handles empty data, full circles, tiny slices and front rim clipping', () => {
  assert.deepEqual(pieArcs([]), [])
  const [whole] = pieArcs([{ id: 'one', value: 1 }])
  assert.equal((topPath(whole).match(/ A /g) ?? []).length, 2)
  assert.equal(sidePaths(whole).length, 1)
  const arcs = pieArcs([{ id: 'tiny', value: 1 }, { id: 'rest', value: 1e9 }])
  assert.equal(arcs.at(-1).end, Math.PI * 1.5)
  for (const arc of arcs) assert.doesNotMatch(topPath(arc), /NaN|Infinity/)
  assert.deepEqual(sidePaths({ start: -Math.PI / 2, end: 0 }), [])
})
