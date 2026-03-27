param(
  [switch]$SkipVsDev
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

# This script must be dot-sourced to persist environment changes.
$dotSourced = $MyInvocation.InvocationName -eq '.' -or $MyInvocation.Line -match '^\s*\.'
if (-not $dotSourced) {
  Write-Warning "Dot-source this script to persist env vars: `. `"$PSCommandPath`""
  return
}

$repoRoot = Resolve-Path (Join-Path $PSScriptRoot '..')
Set-Location $repoRoot

$cargoBin = Join-Path $env:USERPROFILE '.cargo\bin'
if (Test-Path $cargoBin) {
  $env:Path = "$cargoBin;$env:Path"
}

$cmakeCmd = Get-Command cmake -ErrorAction SilentlyContinue
if (-not $cmakeCmd) {
  Write-Warning 'cmake not found in PATH. Install CMake 3.27+ or add it to PATH.'
} else {
  $cmakeWrap = Join-Path $repoRoot 'cmake-wrap.cmd'
  @"
@echo off
setlocal
echo %* | findstr /C:"--build" >nul
if %errorlevel%==0 (
  "$($cmakeCmd.Source)" %*
) else (
  "$($cmakeCmd.Source)" -DCMAKE_POLICY_VERSION_MINIMUM=3.5 %*
)
"@ | Set-Content $cmakeWrap -Encoding ASCII
  $env:CMAKE = $cmakeWrap
}

if (-not $SkipVsDev) {
  if (-not (Get-Command cl -ErrorAction SilentlyContinue)) {
    Write-Warning 'MSVC toolchain not detected in this shell. Run Enter-VSDev or use Developer PowerShell for VS.'
  }
}

Write-Host 'VoiceX env ready: cargo PATH + CMAKE wrapper set.' -ForegroundColor Green
