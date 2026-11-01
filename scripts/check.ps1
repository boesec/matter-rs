# Local quality gate. Runs the same checks as the CI workflow (.github/workflows/ci.yml):
# format check, clippy with warnings as errors, tests, the no_std tier on a bare-metal target,
# the two feature sets of the binary, and cargo-deny.
#
# Usage: ./scripts/check.ps1
# Every step runs even if an earlier one fails; the script exits with 1 if any step failed.
# Works with Windows PowerShell 5.1 and PowerShell 7.

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$root = Split-Path -Parent $PSScriptRoot
Set-Location $root

# Prefer cargo from PATH; fall back to the rustup default location for shells started before
# the toolchain was installed.
$cargo = Get-Command cargo -ErrorAction SilentlyContinue
if ($null -ne $cargo) {
    $cargo = $cargo.Source
} else {
    $cargo = Join-Path $HOME '.cargo/bin/cargo.exe'
    if (-not (Test-Path $cargo)) {
        Write-Host 'cargo not found: install Rust through rustup (https://rustup.rs).'
        exit 1
    }
}

$steps = @(
    @{ Name = 'fmt';           Args = @('fmt', '--all', '--check') },
    @{ Name = 'clippy';        Args = @('clippy', '--workspace', '--all-targets', '--all-features', '--', '-D', 'warnings') },
    @{ Name = 'test';          Args = @('test', '--workspace') },
    # The no_std tier must build without the standard library; the target comes from
    # rust-toolchain.toml and needs no linker for `cargo check`.
    @{ Name = 'no_std';        Args = @('check', '-p', 'matter-rs-engine', '-p', 'matter-rs-adapter-api', '-p', 'matter-rs-bridge', '--no-default-features', '--target', 'thumbv7em-none-eabihf') },
    # A minimal build of the binary selects only its own features; the default build is the
    # one that ships.
    @{ Name = 'role-bridge';   Args = @('check', '-p', 'matter-rs', '--no-default-features', '--features', 'role-bridge') },
    @{ Name = 'default-build'; Args = @('check', '-p', 'matter-rs') },
    @{ Name = 'deny';          Args = @('deny', 'check') }
)

$failed = @()
foreach ($step in $steps) {
    Write-Host ''
    Write-Host "==> cargo $($step.Args -join ' ')"
    & $cargo @($step.Args)
    if ($LASTEXITCODE -ne 0) {
        Write-Host "FAILED: $($step.Name) (exit code $LASTEXITCODE)"
        $failed += $step.Name
    }
}

Write-Host ''
if ($failed.Count -gt 0) {
    Write-Host "Quality gate failed: $($failed -join ', ')"
    exit 1
}
Write-Host 'Quality gate passed.'
exit 0
