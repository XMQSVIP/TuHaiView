param(
    [Parameter(Mandatory=$true)][string]$Executable,
    [Parameter(Mandatory=$true)][string]$Root,
    [int]$Runs=5,
    [int]$Seconds=60,
    [ValidateSet('trajectory','open','scroll','soak','idle')][string]$Scenario='trajectory',
    [ValidateSet('mailbox','vsync','immediate')][string]$Present='mailbox',
    [int]$RepaintMs=0,
    [ValidateSet(0,1)][int]$TimerMs=0,
    [string]$PresentMon='F:\tuhai-validation\tools\PresentMon-2.5.1-x64.exe',
    [string]$AlternateRoot,
    [string]$Manifest,
    [int]$ExpectedRecords=0,
    [switch]$RequireScanCompletion,
    [switch]$AllocatorDiagnostics,
    [string]$OutputDirectory='F:\tuhai-validation\runs',
    [switch]$SkipPresentMon
)
$ErrorActionPreference='Stop'
# A visible window does not stop Windows' display idle timer. Reset it only while
# this explicit benchmark runs; no persistent power-plan setting is changed.
if (-not ('TuHaiBenchmarkPower' -as [type])) {
    Add-Type -TypeDefinition @'
using System.Runtime.InteropServices;
public static class TuHaiBenchmarkPower {
    [DllImport("kernel32.dll", SetLastError=true)]
    public static extern uint SetThreadExecutionState(uint flags);
}
'@
}
function Reset-BenchmarkIdleTimer {
    if ([TuHaiBenchmarkPower]::SetThreadExecutionState(3) -eq 0) {
        throw 'Cannot keep display/system awake for a valid graphics benchmark'
    }
}
$resolvedExecutable=(Resolve-Path -LiteralPath $Executable).Path
$resolvedRoot=(Resolve-Path -LiteralPath $Root).Path
$manifestInfo=if ($Manifest) { @{path=(Resolve-Path -LiteralPath $Manifest).Path;sha256=(Get-FileHash -LiteralPath $Manifest).Hash} } else { $null }
$rootDisk=Get-Partition -DriveLetter $resolvedRoot.Substring(0,1) | Get-Disk | Select-Object Number,FriendlyName,BusType
$env:TUHAI_PERF='1'
$env:TUHAI_PERF_ROOT=$resolvedRoot
$env:TUHAI_PERF_SECONDS="$Seconds"
$env:TUHAI_PERF_SCENARIO=$Scenario
$env:TUHAI_PERF_PRESENT=$Present
$env:TUHAI_PERF_REPAINT_MS=if ($RepaintMs -gt 0) { "$RepaintMs" } else { '' }
$env:TUHAI_PERF_TIMER_MS=if ($TimerMs -eq 1) { '1' } else { '' }
$env:TUHAI_PERF_ALTERNATE_ROOT=$AlternateRoot
Remove-Item Env:/TUHAI_PERF_CAPTURE -ErrorAction SilentlyContinue
Remove-Item Env:/TUHAI_PERF_LATENCY -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Path $OutputDirectory -Force | Out-Null
$OutputDirectory=(Resolve-Path -LiteralPath $OutputDirectory).Path
$env:TUHAI_PERF_LOG_DIR=$OutputDirectory
$env:TUHAI_PERF_ALLOCATOR=if ($AllocatorDiagnostics) { '1' } else { '' }
$exeHash=(Get-FileHash -LiteralPath $resolvedExecutable -Algorithm SHA256).Hash
$toolHash=if (!$SkipPresentMon) { (Get-FileHash -LiteralPath $PresentMon -Algorithm SHA256).Hash } else { $null }
$dwm=python (Join-Path $PSScriptRoot 'display_timing.py') | ConvertFrom-Json
$display=Get-CimInstance Win32_VideoController | Select-Object Name,DriverVersion,CurrentRefreshRate,CurrentHorizontalResolution,CurrentVerticalResolution
for ($run=1; $run -le $Runs; $run++) {
    if ((Get-PSDrive C).Free -lt 3GB) { throw 'Keep 2 GiB free on C plus 1 GiB headroom for this run' }
    Reset-BenchmarkIdleTimer
    $started=Get-Date
    $stamp=$started.ToString('yyyyMMdd-HHmmss')+"-$run"
    # Older measured binaries predate TUHAI_PERF_LOG_DIR; support both without losing their logs.
    $data=@($OutputDirectory,(Join-Path (Split-Path -Parent $resolvedExecutable) 'data'))
    $before=@(Get-ChildItem -LiteralPath $data -Filter 'performance-*.jsonl' -ErrorAction SilentlyContinue | ForEach-Object FullName)
    $process=Start-Process -FilePath $resolvedExecutable -WindowStyle Normal -PassThru -RedirectStandardOutput (Join-Path $OutputDirectory "$stamp-app-output.txt") -RedirectStandardError (Join-Path $OutputDirectory "$stamp-app-errors.txt")
    $capture=$null
    $captureArguments=$null
    $csv=Join-Path $OutputDirectory "$stamp-presentmon.csv"
    if (!$SkipPresentMon) {
        $captureArguments=@('--process_id',"$($process.Id)",'--output_file',"`"$csv`"",'--qpc_time','--timed',"$($Seconds+30)",'--terminate_after_timed','--terminate_on_proc_exit','--no_console_stats','--session_name',"tuhai-$stamp")
        $capture=Start-Process -FilePath $PresentMon -WindowStyle Hidden -PassThru -ArgumentList $captureArguments -RedirectStandardError (Join-Path $OutputDirectory "$stamp-presentmon-errors.txt") -RedirectStandardOutput (Join-Path $OutputDirectory "$stamp-presentmon-output.txt")
    }
    $timedOut=$false
    $abortReason=$null
    while (!$process.WaitForExit(1000)) {
        Reset-BenchmarkIdleTimer
        if ((Get-PSDrive C).Free -lt 2GB+128MB) {
            $process.Kill()
            $process.WaitForExit()
            $abortReason='Stopped invalid run to preserve 2 GiB free on C'
            break
        }
        if (((Get-Date)-$started).TotalSeconds -gt $Seconds+120) {
            $timedOut=$true
            $process.Kill()
            $process.WaitForExit()
            break
        }
    }
    # Non-elevated PresentMon may miss the process-exit notification. End only
    # this run's named ETW session, allowing the capture process to flush its CSV.
    $captureStop=$null
    if ($capture -and !$capture.WaitForExit(1500)) {
        $stop=Start-Process -FilePath $PresentMon -WindowStyle Hidden -PassThru -ArgumentList @('--session_name',"tuhai-$stamp",'--terminate_existing_session') -RedirectStandardError (Join-Path $OutputDirectory "$stamp-capture-stop-errors.txt") -RedirectStandardOutput (Join-Path $OutputDirectory "$stamp-capture-stop-output.txt")
        if ($stop.WaitForExit(5000)) { $captureStop=$stop.ExitCode }
    }
    if ($capture -and !$capture.WaitForExit(35000)) { $capture.Kill(); $capture.WaitForExit(); $abortReason='Capture did not finish within its exit timeout' }
    $logs=@(Get-ChildItem -LiteralPath $data -Filter 'performance-*.jsonl' -ErrorAction SilentlyContinue | Where-Object { $_.FullName -notin $before })
    $savedLogs=@()
    foreach ($log in $logs) {
        $saved=Join-Path $OutputDirectory ("$stamp-"+$log.Name)
        Move-Item -LiteralPath $log.FullName -Destination $saved
        $certificate=[IO.Path]::ChangeExtension($log.FullName,'complete.json')
        if (Test-Path -LiteralPath $certificate) {
            Move-Item -LiteralPath $certificate -Destination ([IO.Path]::ChangeExtension($saved,'complete.json'))
        }
        $savedLogs+=$saved
    }
    $report=[ordered]@{run=$run;pid=$process.Id;started=$started.ToString('o');exit_code=$process.ExitCode;seconds=((Get-Date)-$started).TotalSeconds;timed_out=$timedOut;executable=$resolvedExecutable;sha256=$exeHash;root=$resolvedRoot;scenario=$Scenario;present=$Present;repaint_ms=$RepaintMs;display=$display;dwm=$dwm;system_cache='unknown';application_cache='preserved; GPU empty at process start';logs=$savedLogs;presentmon=$csv;presentmon_sha256=$toolHash;presentmon_exit=if ($capture -and $capture.HasExited) { $capture.ExitCode } else { $null }}
    $report['dataset_manifest']=$manifestInfo
    $report['expected_records']=$ExpectedRecords
    $report['require_scan_completion']=[bool]$RequireScanCompletion
    $report['requested_seconds']=$Seconds
    $report['source_disk']=$rootDisk
    $report['log_output_directory']=$OutputDirectory
    $report['allocator_diagnostics']=[bool]$AllocatorDiagnostics
    $report['timer_ms']=$TimerMs
    $report['presentmon_arguments']=$captureArguments
    $report['presentmon_stop_exit']=$captureStop
    $report['wgpu_environment']=@{discard_hal_labels=$env:WGPU_DISCARD_HAL_LABELS;debug=$env:WGPU_DEBUG;validation=$env:WGPU_VALIDATION}
    $report['display_awake_request']='ES_DISPLAY_REQUIRED | ES_SYSTEM_REQUIRED; periodic, no persistent power-plan change'
    $report['abort_reason']=$abortReason
    $report | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath (Join-Path $OutputDirectory "$stamp-run.json") -Encoding utf8
    $report | ConvertTo-Json -Compress -Depth 8
    if ($abortReason) { throw $abortReason }
    if ($process.ExitCode -ne 0) { throw "UI run $run failed" }
    if (!$SkipPresentMon -and (!(Test-Path -LiteralPath $csv) -or (Get-Item -LiteralPath $csv).Length -lt 200)) {
        throw 'PresentMon produced no display samples; run saved as invalid. Repair capture before repeating the matrix.'
    }
    if (!$SkipPresentMon) {
        $validation=@((Join-Path $PSScriptRoot 'validate_ui_run.py'),(Join-Path $OutputDirectory "$stamp-run.json"),'--expected-records',"$ExpectedRecords")
        if ($RequireScanCompletion) { $validation+='--require-scan' }
        & python @validation
        if ($LASTEXITCODE -ne 0) { throw 'Incomplete/invalid UI run saved; fix warmup or capture before continuing the matrix' }
    }
}
