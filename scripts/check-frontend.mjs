import { spawnSync } from 'node:child_process'
import { existsSync, readdirSync, readFileSync } from 'node:fs'
import path from 'node:path'
import { fileURLToPath } from 'node:url'

const __dirname = path.dirname(fileURLToPath(import.meta.url))
const repoRoot = path.resolve(__dirname, '..')

const CHECK_SCRIPTS = [
  'scripts/check-i18n.mjs',
  'scripts/check-llm-defaults.mjs',
  'scripts/check-history.mjs',
  'scripts/check-text-diff.mjs'
]

function toPosix(filePath) {
  return filePath.split(path.sep).join('/')
}

function collectSpecFiles(dir) {
  const results = []
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    const fullPath = path.join(dir, entry.name)
    if (entry.isDirectory()) {
      results.push(...collectSpecFiles(fullPath))
    } else if (entry.isFile() && entry.name.endsWith('.spec.ts')) {
      results.push(toPosix(path.relative(repoRoot, fullPath)))
    }
  }
  return results.sort()
}

function collectCoveredSpecFiles() {
  const covered = new Set()
  const queue = []

  const addSpec = relPath => {
    const normalized = toPosix(relPath.replace(/^\/+/, ''))
    if (!covered.has(normalized) && existsSync(path.join(repoRoot, normalized))) {
      covered.add(normalized)
      queue.push(normalized)
    }
  }

  for (const scriptRel of CHECK_SCRIPTS) {
    const content = readFileSync(path.join(repoRoot, scriptRel), 'utf8')
    for (const match of content.matchAll(/['"]\/?(src\/[^'"]+\.spec\.ts)['"]/g)) {
      addSpec(match[1])
    }
  }

  while (queue.length > 0) {
    const currentRel = queue.shift()
    const currentAbs = path.join(repoRoot, currentRel)
    const content = readFileSync(currentAbs, 'utf8')
    for (const match of content.matchAll(/(?:from|import\s*\()\s*['"]([^'"]+\.spec(?:\.ts)?)['"]/g)) {
      const importTarget = match[1]
      if (!importTarget.startsWith('.')) continue
      const resolvedAbs = path.resolve(
        path.dirname(currentAbs),
        importTarget.endsWith('.ts') ? importTarget : `${importTarget}.ts`
      )
      addSpec(path.relative(repoRoot, resolvedAbs))
    }
  }

  return covered
}

const allSpecs = collectSpecFiles(path.join(repoRoot, 'src'))
const coveredSpecs = collectCoveredSpecFiles()
const uncoveredSpecs = allSpecs.filter(spec => !coveredSpecs.has(spec))

if (uncoveredSpecs.length > 0) {
  console.error('Uncovered frontend spec files found (not invoked by any scripts/check-*.mjs runner):')
  for (const spec of uncoveredSpecs) {
    console.error(`  - ${spec}`)
  }
  process.exit(1)
}

for (const scriptRel of CHECK_SCRIPTS) {
  console.log(`\n==> Running ${scriptRel}`)
  const result = spawnSync(process.execPath, [path.join(repoRoot, scriptRel)], {
    cwd: repoRoot,
    stdio: 'inherit'
  })
  if (result.error) {
    console.error(`Failed to execute ${scriptRel}:`, result.error)
    process.exit(1)
  }
  if (result.status !== 0) {
    process.exit(result.status ?? 1)
  }
}

console.log(`\nAll ${CHECK_SCRIPTS.length} frontend check scripts passed (${allSpecs.length}/${allSpecs.length} spec files covered).`)
