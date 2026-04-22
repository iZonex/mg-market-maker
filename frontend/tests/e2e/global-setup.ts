/**
 * Playwright globalSetup — ensures a dev stand is alive.
 *
 * If `.stand-run/stand.env` exists, reuse it (dev loop: operator
 * ran `scripts/stand-up.sh` once and now iterates Playwright).
 * Else, spawn the stand synchronously and block until the env
 * file appears. The stand is NOT torn down here — that's the
 * caller's responsibility (CI post-step, operator via
 * `scripts/tear-down.sh`).
 */

import { spawnSync, spawn } from 'node:child_process'
import { existsSync, readFileSync } from 'node:fs'
import { resolve, dirname } from 'node:path'
import { fileURLToPath } from 'node:url'

const __dirname = dirname(fileURLToPath(import.meta.url))
const REPO_ROOT = resolve(__dirname, '../../..')
const STAND_ENV = resolve(REPO_ROOT, '.stand-run/stand.env')

function parseEnv(path: string): Record<string, string> {
  const out: Record<string, string> = {}
  for (const line of readFileSync(path, 'utf8').split('\n')) {
    const eq = line.indexOf('=')
    if (eq <= 0) continue
    out[line.slice(0, eq).trim()] = line.slice(eq + 1).trim()
  }
  return out
}

async function waitForEnvFile(path: string, timeoutMs: number): Promise<void> {
  const start = Date.now()
  while (Date.now() - start < timeoutMs) {
    if (existsSync(path)) return
    await new Promise((r) => setTimeout(r, 500))
  }
  throw new Error(
    `stand-up did not produce ${path} within ${timeoutMs}ms`,
  )
}

export default async function globalSetup() {
  if (!existsSync(STAND_ENV)) {
    console.log('[playwright] no stand.env — booting stand via stand-up.sh')
    const child = spawn('bash', ['scripts/stand-up.sh'], {
      cwd: REPO_ROOT,
      detached: true,
      stdio: ['ignore', 'pipe', 'pipe'],
    })
    child.stdout?.on('data', (b) => process.stdout.write(`[stand-up] ${b}`))
    child.stderr?.on('data', (b) => process.stderr.write(`[stand-up] ${b}`))
    child.unref()
    await waitForEnvFile(STAND_ENV, 5 * 60_000)
  } else {
    console.log('[playwright] reusing existing stand.env')
  }

  const env = parseEnv(STAND_ENV)
  for (const k of ['HTTP_URL', 'ADMIN_TOKEN', 'AGENT_ID', 'DEPLOYMENT_ID']) {
    if (!env[k]) throw new Error(`stand.env missing ${k}`)
    process.env[`STAND_${k}`] = env[k]
  }

  // Quick sanity poke — server responds + graph trace topic has
  // at least one tick. Fail fast here with a readable message
  // rather than inside a test.
  const httpBase = env.HTTP_URL
  const token = env.ADMIN_TOKEN
  const agent = env.AGENT_ID
  const dep = env.DEPLOYMENT_ID
  const resp = await fetch(
    `${httpBase}/api/v1/agents/${agent}/deployments/${dep}/details/graph_trace_recent?limit=1`,
    { headers: { Authorization: `Bearer ${token}` } },
  )
  if (!resp.ok) {
    throw new Error(
      `graph_trace_recent returned ${resp.status} — stand not healthy`,
    )
  }
  const body = await resp.json()
  const nTraces = body?.payload?.traces?.length ?? 0
  console.log(`[playwright] stand alive · ${nTraces} trace(s) ready`)
}
