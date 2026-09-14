import { test } from 'node:test'
import assert from 'node:assert/strict'
import { api, HttpError } from '../src/api.ts'

test('404 keeps its status for folder fallback', async () => {
  const original = globalThis.fetch
  globalThis.fetch = async () => new Response('', { status: 404, statusText: 'Not Found' })
  try {
    await assert.rejects(api.node(999999, 'size', 500, 'size', false),
      error => error instanceof HttpError && error.status === 404)
  } finally { globalThis.fetch = original }
})

test('rescan posts to the node', async () => {
  const original = globalThis.fetch
  const calls = []
  globalThis.fetch = async (url, init) => {
    calls.push({ url: String(url), method: init?.method })
    return new Response('', { status: 202 })
  }
  try {
    await api.rescan(42)
    assert.deepEqual(calls, [{ url: '/api/rescan/42', method: 'POST' }])
  } finally { globalThis.fetch = original }
})

test('map and list send identical filters including encoded names', async () => {
  const original = globalThis.fetch
  const urls = []
  globalThis.fetch = async url => {
    urls.push(new URL(url, 'http://localhost'))
    return new Response('{}', { status: 200 })
  }
  try {
    const filter = { q: 'a & b', ext: 'txt,png', min: 10, max: 5000, age: 30 }
    await api.search(0, { ...filter, metric: 'size', limit: 500 })
    await api.tree(0, 'size', 3, 60, undefined, filter)
    for (const key of ['q', 'ext', 'min', 'max', 'age', 'metric']) {
      assert.equal(urls[0].searchParams.get(key), urls[1].searchParams.get(key), key)
    }
    assert.equal(urls[1].searchParams.get('q'), filter.q)
  } finally { globalThis.fetch = original }
})
