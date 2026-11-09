import { spawnSync } from 'node:child_process'
import { prepareQwenEngine } from './prepare-qwen-engine.mjs'

const args = process.argv.slice(2)
const targetIndex = args.findIndex(arg => arg === '--target' || arg === '-t')
const target = targetIndex >= 0 ? args[targetIndex + 1] : args.find(arg => arg.startsWith('--target='))?.split('=')[1]
const appleSilicon = target ? target === 'aarch64-apple-darwin' : process.platform === 'darwin' && process.arch === 'arm64'
if (['dev', 'build', 'bundle'].includes(args[0]) && appleSilicon && !args.includes('--help') && !args.includes('-h')) {
  prepareQwenEngine()
  const separator = args.indexOf('--')
  args.splice(separator < 0 ? args.length : separator, 0, '--config', JSON.stringify({ bundle: { externalBin: ['binaries/qwen-asr'] } }))
}
const result = spawnSync('pnpm', ['exec', 'tauri', ...args], { stdio: 'inherit', shell: process.platform === 'win32' })
if (result.error) throw result.error
process.exit(result.status ?? 1)
