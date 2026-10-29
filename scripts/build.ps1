param([switch]$Release, [switch]$Test, [switch]$Check)
$ErrorActionPreference = 'Stop'
$projectRoot = Split-Path -Parent $PSScriptRoot
# Optional portable toolchain: CORRAL_PORTABLE_RUST, else the project's own .tools rust folder; otherwise cargo from PATH.
$portableRoot = if ($env:CORRAL_PORTABLE_RUST) { $env:CORRAL_PORTABLE_RUST } else { Join-Path $projectRoot '.tools\rust' }
if (Test-Path -LiteralPath (Join-Path $portableRoot 'bin\cargo.exe')) {
    $env:PATH = "$(Join-Path $portableRoot 'bin');$(Join-Path $portableRoot 'lib\rustlib\x86_64-pc-windows-gnu\bin');$(Join-Path $portableRoot 'lib\rustlib\x86_64-pc-windows-gnu\bin\self-contained');$env:PATH"
    $env:CARGO_HOME = Join-Path $projectRoot '.tools\cargo-home'
    $env:RUSTC = Join-Path $portableRoot 'bin\rustc.exe'
}
Push-Location $projectRoot
try {
    $command = if ($Test) { 'test' } elseif ($Check) { 'check' } else { 'build' }
    $arguments = @($command)
    if ($Release) { $arguments += '--release' }
    & cargo @arguments
    if ($LASTEXITCODE -ne 0) { throw "Cargo $command failed" }
    if ($Test) {
        # `cargo test` only builds the test harness; keep target\debug\corral.exe in sync for live trials.
        $buildArguments = @('build'); if ($Release) { $buildArguments += '--release' }
        & cargo @buildArguments
        if ($LASTEXITCODE -ne 0) { throw 'Cargo build failed' }
    }
} finally { Pop-Location }

