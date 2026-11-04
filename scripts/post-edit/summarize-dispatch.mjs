import { readFileSync } from 'node:fs'
const samples = JSON.parse(readFileSync(process.argv[2], 'utf8'))
const summary = enabled => {
  const series = samples.filter(s => s.enabled === enabled).map(s => s.dispatch_us / 1000).sort((a, b) => a - b)
  const at = p => series[Math.ceil(p * series.length) - 1]
  return { n: series.length, p50_ms: at(0.5), p95_ms: at(0.95), max_ms: at(1) }
}
const off = summary(false), on = summary(true)
console.log(JSON.stringify({ off, on, p95_increment_ms: on.p95_ms - off.p95_ms }, null, 2))
