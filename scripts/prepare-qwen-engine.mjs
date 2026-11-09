import { createHash } from 'node:crypto'
import { existsSync, readFileSync, mkdirSync, mkdtempSync, copyFileSync, chmodSync, rmSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'
import { execFileSync } from 'node:child_process'

const root = fileURLToPath(new URL('../', import.meta.url))
export const engine = JSON.parse(readFileSync(join(root, 'src/shared/localAsrModels.json'), 'utf8')).engine
const sha256 = path => createHash('sha256').update(readFileSync(path)).digest('hex')

export function prepareQwenEngine() {
  const destination = join(root, 'src-tauri/binaries/qwen-asr-aarch64-apple-darwin')
  if (existsSync(destination) && sha256(destination) === engine.executableSha256) return destination
  const temporary = mkdtempSync(join(tmpdir(), 'voicex-qwen-engine-'))
  try {
    const archive = join(temporary, 'engine.tar.gz')
    execFileSync('curl', ['-fL', '--retry', '2', '--connect-timeout', '10', '--max-time', '120', engine.archiveUrl, '-o', archive], { stdio: 'inherit' })
    if (sha256(archive) !== engine.archiveSha256) throw new Error('Qwen engine archive checksum mismatch')
    execFileSync('tar', ['-xzf', archive, '-C', temporary])
    const binary = join(temporary, 'qwen-asr')
    if (sha256(binary) !== engine.executableSha256) throw new Error('Qwen engine executable checksum mismatch')
    mkdirSync(resolve(destination, '..'), { recursive: true })
    copyFileSync(binary, destination)
    chmodSync(destination, 0o755)
    return destination
  } finally {
    rmSync(temporary, { recursive: true, force: true })
  }
}
