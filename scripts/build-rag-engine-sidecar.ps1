Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$repoRoot = Resolve-Path (Join-Path $PSScriptRoot "..")
$engineGoDir = Join-Path $repoRoot "external\rag-engine\engine\go"
$binariesDir = Join-Path $repoRoot "src-tauri\binaries"
$goCacheDir = Join-Path $repoRoot "src-tauri\target\rag-engine-go-build"

if (-not (Test-Path $engineGoDir)) {
  throw "rag-engine submodule is missing. Run: git submodule update --init --recursive"
}

$hostLine = rustc -vV | Select-String "^host:"
if (-not $hostLine) {
  throw "Could not determine Rust target triple from rustc -vV."
}

$targetTriple = ($hostLine.ToString() -replace "^host:\s*", "").Trim()
$binaryName = "ai-engine-server-$targetTriple"
$isWindows = [System.Runtime.InteropServices.RuntimeInformation]::IsOSPlatform(
  [System.Runtime.InteropServices.OSPlatform]::Windows
)
if ($isWindows -and -not $binaryName.EndsWith(".exe")) {
  $binaryName = "$binaryName.exe"
}

New-Item -ItemType Directory -Force -Path $binariesDir | Out-Null
New-Item -ItemType Directory -Force -Path $goCacheDir | Out-Null
$outputPath = Join-Path $binariesDir $binaryName

Push-Location $engineGoDir
try {
  $previousGoCache = $env:GOCACHE
  $env:GOCACHE = $goCacheDir
  go build -buildvcs=false -o $outputPath ./cmd/server
}
finally {
  $env:GOCACHE = $previousGoCache
  Pop-Location
}

Write-Host "Built rag-engine sidecar: $outputPath"
