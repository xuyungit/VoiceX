param(
  [string]$CargoTargetDir = "$env:USERPROFILE\voicex-target",
  [switch]$SkipCmakeWorkaround,
  [switch]$SkipTrust
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

function Test-IsAdministrator {
  $identity = [Security.Principal.WindowsIdentity]::GetCurrent()
  $principal = New-Object Security.Principal.WindowsPrincipal($identity)
  return $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
}

function Ensure-Directory {
  param([string]$Path)

  if (-not (Test-Path $Path)) {
    New-Item -ItemType Directory -Path $Path -Force | Out-Null
  }

  return (Resolve-Path $Path).Path.TrimEnd('\')
}

function Add-DefenderTrust {
  param([string]$Path)

  if (-not (Test-IsAdministrator)) {
    Write-Warning "Not running as admin; unable to add exclusion for '$Path'. Please add it manually in Windows Security > Virus & threat protection > Exclusions."
    return
  }

  try {
    $pref = Get-MpPreference -ErrorAction Stop
    $existing = @($pref.ExclusionPath | ForEach-Object { $_.TrimEnd('\') })
    if ($existing -notcontains $Path) {
      Add-MpPreference -ExclusionPath $Path -ErrorAction Stop | Out-Null
      Write-Host "Added Defender exclusion: $Path"
    } else {
      Write-Host "Defender exclusion already exists: $Path"
    }
  }
  catch {
    Write-Warning "Failed to set Defender exclusion for '$Path'. Build may still work if your admin policy already trusts this location."
    Write-Warning $_.Exception.Message
  }
}

function Ensure-CmakePolicyWorkaround {
  param([string]$RepoRoot)

  $cmakeCmd = Get-Command cmake -ErrorAction SilentlyContinue
  if (-not $cmakeCmd) {
    Write-Warning 'cmake not found in PATH. If you see a cmake minimum-version error, install/update cmake first.'
    return
  }

  $cmakeWrap = Join-Path $RepoRoot 'cmake-wrap.cmd'
  @"
@echo off
setlocal
echo %* | findstr /C:"--build" >nul
if %errorlevel%==0 (
  "$($cmakeCmd.Source)" %*
) else (
  "$($cmakeCmd.Source)" -DCMAKE_POLICY_VERSION_MINIMUM=3.5 %*
)
"@ | Set-Content -Path $cmakeWrap -Encoding ASCII
  $env:CMAKE = $cmakeWrap
  Write-Host "Using cmake wrapper for Audiopus CMake compatibility: $cmakeWrap"
}

$repoRoot = Resolve-Path (Join-Path $PSScriptRoot '..')
Set-Location $repoRoot

if (-not (Get-Command pnpm -ErrorAction SilentlyContinue)) {
  throw "pnpm is not available in PATH. Install dependencies first: pnpm install."
}

$projectTargetDir = Ensure-Directory (Join-Path $repoRoot 'src-tauri\target')
$trustedTargetDir = Ensure-Directory $CargoTargetDir

if (-not $SkipTrust) {
  Add-DefenderTrust -Path $projectTargetDir
  Add-DefenderTrust -Path $trustedTargetDir
}
if (-not $SkipCmakeWorkaround) {
  Ensure-CmakePolicyWorkaround -RepoRoot $repoRoot
}

$env:CARGO_TARGET_DIR = $trustedTargetDir
Write-Host "Using CARGO_TARGET_DIR=$env:CARGO_TARGET_DIR"
Write-Host 'Running pnpm tauri build...'

& pnpm tauri build
if ($LASTEXITCODE -ne 0) {
  throw "pnpm tauri build failed with exit code $LASTEXITCODE"
}
