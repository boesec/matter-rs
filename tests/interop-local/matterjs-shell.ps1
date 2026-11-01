# Local interop scenario: the On/Off node example commissioned and driven by the matter.js shell.
#
# Builds the example, starts it with a fresh state directory, commissions it with the shell
# through the pairing code the example prints, reads, toggles and writes through the shell,
# restarts the node on its persisted state and checks that it is still commissioned. The
# assertions are in the step files next to this script; the driver is drive-shell.mjs.
#
# Usage: ./tests/interop-local/matterjs-shell.ps1 -MatterJs <matter.js clone> [-Interface <name>]
#        [-WorkDir <directory>] [-Port <n>]
#
# Needs: cargo, Node.js, a built matter.js clone (packages/nodejs-shell/dist/esm/app.js). The
# node listens on UDP port 5540 (or -Port) and 5353; on Windows the firewall must allow the
# example binary to receive on them. Nothing of this scenario is committed except the scripts:
# the logs land in the work directory and contain the pairing code.
#
# Exit code 0 when every step passed, 1 otherwise. Works with Windows PowerShell 5.1 and
# PowerShell 7.

param(
    [Parameter(Mandatory = $true)] [string] $MatterJs,
    [string] $Interface = '',
    [string] $WorkDir = (Join-Path ([System.IO.Path]::GetTempPath()) 'matter-rs-interop-local'),
    [int] $Port = 5540
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$here = $PSScriptRoot
$root = Split-Path -Parent (Split-Path -Parent $here)
$app = Join-Path $MatterJs 'packages/nodejs-shell/dist/esm/app.js'
if (-not (Test-Path $app)) {
    Write-Host "matter.js shell not found at $app; build the clone first (see README.md)"
    exit 1
}

$cargo = Get-Command cargo -ErrorAction SilentlyContinue
if ($null -ne $cargo) { $cargo = $cargo.Source } else { $cargo = Join-Path $HOME '.cargo/bin/cargo.exe' }

Write-Host '==> building the example'
& $cargo build -p matter-rs-engine-rsmatter --example onoff_node --manifest-path (Join-Path $root 'Cargo.toml')
if ($LASTEXITCODE -ne 0) { exit 1 }
$example = Join-Path $root 'target/debug/examples/onoff_node.exe'

if (Test-Path $WorkDir) { Remove-Item -Recurse -Force $WorkDir }
New-Item -ItemType Directory -Force $WorkDir | Out-Null
$stateDir = Join-Path $WorkDir 'node-state'
$shellStorage = Join-Path $WorkDir 'controller-storage'
$shellLog = Join-Path $WorkDir 'shell.log'

function Start-Node([string[]] $extraArgs, [string] $name) {
    $out = Join-Path $WorkDir "$name.out.log"
    $err = Join-Path $WorkDir "$name.err.log"
    $arguments = @('--state-dir', $stateDir, '--port', "$Port") + $extraArgs
    if ($Interface -ne '') { $arguments += @('--interface', $Interface) }
    $process = Start-Process -FilePath $example -ArgumentList $arguments -PassThru -NoNewWindow `
        -RedirectStandardOutput $out -RedirectStandardError $err
    $deadline = (Get-Date).AddSeconds(60)
    while ((Get-Date) -lt $deadline) {
        Start-Sleep -Milliseconds 500
        if ($process.HasExited) { break }
        if ((Test-Path $out) -and (Select-String -Path $out -Pattern 'onoff_node: ready' -Quiet)) {
            return @{ Process = $process; Out = $out; Err = $err }
        }
    }
    Write-Host "the node did not become ready; see $err"
    if (-not $process.HasExited) { Stop-Process -Id $process.Id -Force }
    exit 1
}

function Stop-Node($node) {
    if (-not $node.Process.HasExited) { Stop-Process -Id $node.Process.Id -Force }
    Start-Sleep -Milliseconds 500
}

function Invoke-Steps([string] $steps, [string] $pairingCode) {
    $env:MATTER_JS_APP = $app
    $env:SHELL_STORAGE = $shellStorage
    $env:SHELL_LOG = $shellLog
    $env:PAIRING_CODE = $pairingCode
    # The driver's output goes to the host, so that the function returns the exit code only.
    & node (Join-Path $here 'drive-shell.mjs') (Join-Path $here $steps) | ForEach-Object { Write-Host $_ }
    return $LASTEXITCODE
}

Write-Host '==> starting the node (fresh state)'
$node = Start-Node @('--print-pairing') 'node-1'
$pairing = Select-String -Path $node.Out -Pattern 'onoff_node: pairing code (\d+)' | Select-Object -First 1
if ($null -eq $pairing) {
    Write-Host 'the node printed no pairing code'
    Stop-Node $node
    exit 1
}
$pairingCode = $pairing.Matches[0].Groups[1].Value

Write-Host '==> commissioning and driving through the shell'
$first = Invoke-Steps 'steps-commission.json' $pairingCode
Stop-Node $node

Write-Host '==> restarting the node on its persisted state'
$node = Start-Node @() 'node-2'
$second = Invoke-Steps 'steps-restart.json' ''
Stop-Node $node

if ($first -eq 0 -and $second -eq 0) {
    Write-Host "PASS: commission, read, toggle, write and restart (logs in $WorkDir)"
    exit 0
}
Write-Host "FAIL: commission run $first, restart run $second (logs in $WorkDir)"
exit 1
