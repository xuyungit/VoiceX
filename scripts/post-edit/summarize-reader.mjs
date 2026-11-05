// Analyze only metadata emitted by the controlled native reader test.
import { readFileSync } from 'node:fs'

const lines = readFileSync(process.argv[2] || 0, 'utf8').split('\n')
const collect = prefix => lines.filter(line => line.startsWith(prefix)).map(line => JSON.parse(line.slice(prefix.length)))
const queries = collect('native_read_metric=')
const resources = collect('native_target_metric=')
if (!queries.length) throw new Error('No native reader measurements found')
const percentile = (values, p) => [...values].sort((a, b) => a - b)[Math.max(0, Math.ceil(values.length * p) - 1)]
const operations = Object.fromEntries([...new Set(queries.map(row => row.operation))].map(operation => {
  const rows = queries.filter(row => row.operation === operation)
  return [operation, {
    count: rows.length,
    p50Ms: percentile(rows.map(row => row.elapsedUs / 1000), 0.5),
    p95Ms: percentile(rows.map(row => row.elapsedUs / 1000), 0.95),
    maxMs: Math.max(...rows.map(row => row.elapsedUs / 1000)),
    medianAttributeQueries: percentile(rows.map(row => row.attributeQueries), 0.5),
    frameworkChecks: rows.reduce((sum, row) => sum + row.frameworkChecks, 0),
    capabilityChecks: rows.reduce((sum, row) => sum + row.capabilityChecks, 0),
  }]
}))
const cpuSeconds = value => value.split(':').reduce((seconds, part) => seconds * 60 + Number(part), 0)
const phases = Object.fromEntries(resources.map(row => [row.phase, row]))
const intervals = {}
for (const [name, from, to] of [['idle', 'idle_start', 'idle_end'], ['twentyReads', 'idle_end', 'read_end']]) {
  if (!phases[from] || !phases[to]) continue
  intervals[name] = {
    elapsedSeconds: (phases[to].elapsedUs - phases[from].elapsedUs) / 1e6,
    targetCpuSeconds: cpuSeconds(phases[to].cpuTime) - cpuSeconds(phases[from].cpuTime),
    targetRssDeltaKiB: phases[to].rssKiB - phases[from].rssKiB,
  }
}
console.log(JSON.stringify({ operations, intervals, scope: 'Ready synthetic AppKit target; 20 reads at 50 ms spacing. Target process only; not Electron, full app workload, cold startup or energy qualification.' }, null, 2))
