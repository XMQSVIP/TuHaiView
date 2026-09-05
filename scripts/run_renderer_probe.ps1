param(
    [Parameter(Mandatory=$true)][string]$Executable,
    [Parameter(Mandatory=$true)][string]$OutputDirectory,
    [ValidateRange(10,1800)][int]$Seconds=180,
    [ValidateRange(0,300)][int]$IdleSeconds=30,
    [ValidateRange(0,1000)][int]$RepaintMs=0,
    [switch]$ExtraPoll,
    [switch]$Fifo,
    [switch]$NoWidgets,
    [switch]$Warp,
    [ValidateSet('none','queue','belt')][string]$BufferMode='none'
)
$ErrorActionPreference='Stop'
if (Get-Process -Name TuHaiView,dx12_memory_probe -ErrorAction SilentlyContinue) {
    throw 'Wait for the active application/renderer benchmark to finish'
}
New-Item -ItemType Directory -Path $OutputDirectory -Force | Out-Null
$OutputDirectory=(Resolve-Path -LiteralPath $OutputDirectory).Path
$Executable=(Resolve-Path -LiteralPath $Executable).Path
$stamp=(Get-Date).ToString('yyyyMMdd-HHmmss')
$log=Join-Path $OutputDirectory "$stamp-probe.jsonl"
$env:TUHAI_PROBE_LOG=$log
$env:TUHAI_PROBE_SECONDS="$Seconds"
$env:TUHAI_PROBE_IDLE_SECONDS="$IdleSeconds"
$env:TUHAI_PROBE_REPAINT_MS="$RepaintMs"
$env:TUHAI_PROBE_EXTRA_POLL=if ($ExtraPoll) {'1'}else{'0'}
$env:TUHAI_PROBE_FIFO=if ($Fifo) {'1'}else{'0'}
$env:TUHAI_PROBE_NO_WIDGETS=if ($NoWidgets) {'1'}else{'0'}
$env:TUHAI_PROBE_WARP=if ($Warp) {'1'}else{'0'}
$env:TUHAI_PROBE_BUFFER_MODE=$BufferMode
if (-not ('TuHaiRendererProbePower' -as [type])) {
    Add-Type -TypeDefinition @'
using System.Runtime.InteropServices;
public static class TuHaiRendererProbePower {
    [DllImport("kernel32.dll")]
    public static extern uint SetThreadExecutionState(uint flags);
}
'@
}
$started=Get-Date
$process=Start-Process -FilePath $Executable -WindowStyle Normal -PassThru -RedirectStandardOutput (Join-Path $OutputDirectory "$stamp-output.txt") -RedirectStandardError (Join-Path $OutputDirectory "$stamp-errors.txt")
$aborted=$null
while (!$process.WaitForExit(1000)) {
    if ([TuHaiRendererProbePower]::SetThreadExecutionState(3) -eq 0) {
        $aborted='display_awake_request_failed'
    }
    if (((Get-Date)-$started).TotalSeconds -gt $Seconds+$IdleSeconds+60) {
        $aborted='probe_timeout'
    }
    if ($aborted) { $process.Kill(); $process.WaitForExit(); break }
}
$report=[ordered]@{
    diagnostic_only=$true;executable=$Executable;sha256=(Get-FileHash -LiteralPath $Executable).Hash;
    started=$started.ToString('o');seconds=$Seconds;idle_seconds=$IdleSeconds;
    repaint_ms=$RepaintMs;extra_poll=[bool]$ExtraPoll;fifo=[bool]$Fifo;no_widgets=[bool]$NoWidgets;warp=[bool]$Warp;buffer_mode=$BufferMode;
    exit_code=$process.ExitCode;abort_reason=$aborted;log=$log;
    dwm=(python (Join-Path $PSScriptRoot 'display_timing.py') | ConvertFrom-Json)
}
$report | ConvertTo-Json -Depth 6 | Set-Content -LiteralPath (Join-Path $OutputDirectory "$stamp-run.json") -Encoding utf8
if ($aborted -or $process.ExitCode -ne 0) { throw 'Renderer probe did not complete' }
python (Join-Path $PSScriptRoot 'summarize_renderer_probe.py') $log --output (Join-Path $OutputDirectory "$stamp-summary.json")
if ($LASTEXITCODE -ne 0) { throw 'Renderer probe log incomplete' }
