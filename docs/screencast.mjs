#!/usr/bin/env node
// Records docs/demo.gif from the hidden `--demo` mode.
//
// The demo flag streams a synthetic tree into the UI with a pause between
// directories, so the recording shows a scan in progress without preparing
// any real files. This script builds the binary, drives headless Chrome over
// the DevTools protocol while the scan streams, and encodes the captured
// frames with ffmpeg.
//
// Usage: node docs/screencast.mjs [--out <gif>] [--port <n>] [--debug-port <n>]
//                                 [--keep-frames]
//
// Needs cargo, node 22.4+ (built-in WebSocket) and Chrome/Chromium. ffmpeg is
// looked up on PATH, or installed once with npm (`ffmpeg-static`) when missing.

import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import { spawn, spawnSync } from 'node:child_process'
import { createRequire } from 'node:module'
import { setTimeout as sleep } from 'node:timers/promises'
import { fileURLToPath } from 'node:url'

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..')

/** Viewport and encoding settings for the recorded animation. */
const VIEW = { width: 1560, height: 900 }
const GIF = { fps: 15, height: 600, colors: 128 }

/** Frames smaller than this are the blank page before the app paints. */
const BLANK_FRAME_BYTES = 25_000

const args = process.argv.slice(2)
const arg = (name, fallback) => {
  const i = args.indexOf(name)
  return i >= 0 ? args[i + 1] : fallback
}
const out = path.resolve(arg('--out', path.join(ROOT, 'docs', 'demo.gif')))
const port = Number(arg('--port', '8132'))
const debugPort = Number(arg('--debug-port', '9228'))
const keepFrames = args.includes('--keep-frames')
const framesDir = fs.mkdtempSync(path.join(os.tmpdir(), 'duw-frames-'))
const profileDir = fs.mkdtempSync(path.join(os.tmpdir(), 'duw-chrome-'))

let chrome
let server

try {
  if (typeof WebSocket === 'undefined') {
    throw new Error('node 22.4+ is required (built-in WebSocket)')
  }

  const binary = buildBinary()
  const chromePath = findChrome()
  const ffmpeg = findFfmpeg()
  console.log(`duw-demo: ${binary}`)
  console.log(`duw-demo: ${chromePath}`)
  console.log(`duw-demo: ${ffmpeg}`)

  chrome = spawn(
    chromePath,
    [
      '--headless=new',
      '--disable-gpu',
      '--no-sandbox',
      `--user-data-dir=${profileDir}`,
      `--remote-debugging-port=${debugPort}`,
      `--window-size=${VIEW.width},${VIEW.height}`,
      'about:blank',
    ],
    { detached: true, stdio: 'ignore' },
  )
  chrome.unref()

  const target = await waitForChrome(debugPort)
  const ws = await connect(target.webSocketDebuggerUrl)

  let id = 0
  const pending = new Map()
  let frameCount = 0
  const times = []
  ws.onmessage = (ev) => {
    const message = JSON.parse(ev.data)
    if (message.id && pending.has(message.id)) {
      pending.get(message.id)(message)
      pending.delete(message.id)
    }
    if (message.method !== 'Page.screencastFrame') return
    ws.send(
      JSON.stringify({
        id: ++id,
        method: 'Page.screencastFrameAck',
        params: { sessionId: message.params.sessionId },
      }),
    )
    const name = `f_${String(++frameCount).padStart(5, '0')}.jpg`
    fs.writeFileSync(path.join(framesDir, name), Buffer.from(message.params.data, 'base64'))
    times.push(message.params.metadata.timestamp)
  }
  const send = (method, params = {}) => {
    const messageId = ++id
    ws.send(JSON.stringify({ id: messageId, method, params }))
    return new Promise((resolve) => pending.set(messageId, resolve))
  }
  const evaluate = async (expression) => {
    const r = await send('Runtime.evaluate', { expression, awaitPromise: true, returnByValue: true })
    if (r.result?.exceptionDetails) throw new Error(JSON.stringify(r.result.exceptionDetails))
    return r.result?.result?.value
  }

  await send('Runtime.enable')
  await send('Page.enable')
  await send('Emulation.setDeviceMetricsOverride', {
    width: VIEW.width,
    height: VIEW.height,
    deviceScaleFactor: 1,
    mobile: false,
  })
  await send('Page.startScreencast', {
    format: 'jpeg',
    quality: 85,
    maxWidth: VIEW.width,
    maxHeight: VIEW.height,
    everyNthFrame: 1,
  })

  // The footer path comes from the root argument; the demo only uses its name.
  server = spawn(binary, ['--demo', '--no-open', '--port', String(port)], {
    detached: true,
    stdio: 'ignore',
  })
  server.unref()
  await sleep(250)
  await send('Page.navigate', { url: `http://127.0.0.1:${port}/` })

  console.log('duw-demo: recording the scan')
  const deadline = Date.now() + 120_000
  for (;;) {
    const status = await evaluate(`document.querySelector('.status')?.innerText ?? ''`)
    if (status.includes('scan complete') && !status.includes('checking')) break
    if (Date.now() > deadline) throw new Error('the scan never finished')
    await sleep(100)
  }
  await sleep(900)
  await send('Page.stopScreencast')
  ws.close()
  fs.writeFileSync(path.join(framesDir, 'times.json'), JSON.stringify(times))
  console.log(`duw-demo: ${frameCount} frames`)

  encode(ffmpeg, times, frameCount)
  console.log(`duw-demo: wrote ${out}`)
} finally {
  killTree(chrome?.pid)
  killTree(server?.pid)
  fs.rmSync(profileDir, { recursive: true, force: true })
  if (!keepFrames) fs.rmSync(framesDir, { recursive: true, force: true })
  else console.log(`duw-demo: frames kept in ${framesDir}`)
}

process.exit(0)

/** Builds the debug binary (it reads web/dist from disk) and returns its path. */
function buildBinary() {
  const manifest = path.join(ROOT, 'Cargo.toml')
  const build = spawnSync('cargo', ['build', '--manifest-path', manifest], { stdio: 'inherit' })
  if (build.status !== 0) throw new Error('cargo build failed')

  const meta = spawnSync(
    'cargo',
    ['metadata', '--no-deps', '--format-version', '1', '--manifest-path', manifest],
    { encoding: 'utf8' },
  )
  if (meta.status !== 0) throw new Error('cargo metadata failed')
  const exe = process.platform === 'win32' ? 'duw.exe' : 'duw'
  const binary = path.join(JSON.parse(meta.stdout).target_directory, 'debug', exe)
  if (!fs.existsSync(binary)) throw new Error(`missing ${binary}`)
  return binary
}

function findChrome() {
  const fromEnv = process.env.CHROME || process.env.CHROME_BIN
  if (fromEnv && fs.existsSync(fromEnv)) return fromEnv

  const candidates = {
    win32: [
      'C:/Program Files/Google/Chrome/Application/chrome.exe',
      'C:/Program Files (x86)/Google/Chrome/Application/chrome.exe',
      'C:/Program Files/Microsoft/Edge/Application/msedge.exe',
      'C:/Program Files (x86)/Microsoft/Edge/Application/msedge.exe',
    ],
    darwin: [
      '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome',
      '/Applications/Chromium.app/Contents/MacOS/Chromium',
    ],
    linux: [],
  }[process.platform] ?? []

  for (const candidate of candidates) {
    if (fs.existsSync(candidate)) return candidate
  }
  for (const name of ['google-chrome', 'google-chrome-stable', 'chromium', 'chromium-browser']) {
    const found = which(name)
    if (found) return found
  }
  throw new Error('no Chrome found; set CHROME=/path/to/chrome')
}

function findFfmpeg() {
  if (process.env.FFMPEG) return process.env.FFMPEG
  const onPath = which('ffmpeg')
  if (onPath) return onPath

  const cache = path.join(os.tmpdir(), 'duw-demo-tools')
  const pkg = path.join(cache, 'node_modules', 'ffmpeg-static')
  const ffmpegExe = path.join(pkg, process.platform === 'win32' ? 'ffmpeg.exe' : 'ffmpeg')
  if (!fs.existsSync(ffmpegExe)) {
    console.log('duw-demo: installing ffmpeg-static (one time)')
    // npm is a shell shim on Windows, so it needs a shell to run.
    const install = spawnSync(
      'npm',
      ['install', '--prefix', `"${cache}"`, 'ffmpeg-static', '--no-audit', '--no-fund'],
      { stdio: 'inherit', shell: true },
    )
    if (install.status !== 0) throw new Error('npm install failed; install ffmpeg or set FFMPEG')
  }
  return createRequire(import.meta.url)(pkg)
}

function which(name) {
  const probe = spawnSync(process.platform === 'win32' ? 'where' : 'which', [name], {
    encoding: 'utf8',
  })
  return probe.status === 0 ? probe.stdout.split(/\r?\n/)[0].trim() : null
}

async function waitForChrome(debugPort) {
  for (let i = 0; i < 100; i++) {
    try {
      const targets = await (await fetch(`http://127.0.0.1:${debugPort}/json`)).json()
      const page = targets.find((t) => t.type === 'page')
      if (page?.webSocketDebuggerUrl) return page
    } catch {
      // Chrome is not listening yet.
    }
    await sleep(100)
  }
  throw new Error('Chrome did not open a debugging port')
}

async function connect(url) {
  const ws = new WebSocket(url)
  await new Promise((resolve, reject) => {
    ws.onopen = resolve
    ws.onerror = () => reject(new Error('cannot connect to Chrome'))
    setTimeout(() => reject(new Error('Chrome connection timed out')), 5000)
  })
  return ws
}

/** Encodes the captured frames, keeping their original timing. */
function encode(ffmpeg, times, frameCount) {
  const files = []
  for (let i = 1; i <= frameCount; i++) {
    files.push(`f_${String(i).padStart(5, '0')}.jpg`)
  }
  // The recording starts before the page paints; drop the blank lead-in.
  let first = 0
  while (first < files.length) {
    if (fs.statSync(path.join(framesDir, files[first])).size > BLANK_FRAME_BYTES) break
    first++
  }

  let list = ''
  for (let i = first; i < files.length; i++) {
    const now = times[i]
    const next = times[i + 1]
    const raw = next && now ? next - now : 0.4
    const duration = Math.min(0.4, Math.max(0.02, raw))
    list += `file '${files[i]}'\nduration ${duration.toFixed(4)}\n`
  }
  list += `file '${files[files.length - 1]}'\n`
  fs.writeFileSync(path.join(framesDir, 'list.txt'), list)

  const filter =
    `fps=${GIF.fps},scale=-2:${GIF.height}:flags=lanczos,` +
    `split[s0][s1];[s0]palettegen=max_colors=${GIF.colors}:stats_mode=diff[p];` +
    `[s1][p]paletteuse=dither=bayer:bayer_scale=3:diff_mode=rectangle`
  const result = spawnSync(
    ffmpeg,
    ['-y', '-f', 'concat', '-safe', '0', '-i', 'list.txt', '-vf', filter, '-loop', '0', out],
    { cwd: framesDir, stdio: 'inherit' },
  )
  if (result.status !== 0) throw new Error('ffmpeg failed')
}

/** Kills a detached child and its helpers on both Windows and Unix. */
function killTree(pid) {
  if (!pid) return
  if (process.platform === 'win32') {
    spawnSync('taskkill', ['/PID', String(pid), '/T', '/F'], { stdio: 'ignore' })
    return
  }
  try {
    process.kill(-pid, 'SIGKILL')
  } catch {
    try {
      process.kill(pid, 'SIGKILL')
    } catch {
      // Already gone.
    }
  }
}
