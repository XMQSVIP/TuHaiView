param(
    [Parameter(Mandatory=$true)][string]$Executable,
    [Parameter(Mandatory=$true)][string]$Root,
    [int]$Runs=5,
    [int]$Seconds=60,
    [ValidateSet('trajectory','open','scroll','soak','idle')][string]$Scenario='trajectory',
    [ValidateSet('mailbox','vsync','immediate')][string]$Present='mailbox',
    [int]$RepaintMs=0,
    [string]$PresentMon='F:\tuhai-validation\tools\PresentMon-2.5.1-x64.exe',
    [string]$AlternateRoot,
    [string]$Manifest,
    [string]$OutputDirectory='F:\tuhai-validation\runs',
    [switch]$SkipPresentMon
)
$ErrorActionPreference='Stop'
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
$env:TUHAI_PERF_ALTERNATE_ROOT=$AlternateRoot
Remove-Item Env:/TUHAI_PERF_CAPTURE -ErrorAction SilentlyContinue
Remove-Item Env:/TUHAI_PERF_LATENCY -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Path $OutputDirectory -Force | Out-Null
$exeHash=(Get-FileHash -LiteralPath $resolvedExecutable -Algorithm SHA256).Hash
$toolHash=if (!$SkipPresentMon) { (Get-FileHash -LiteralPath $PresentMon -Algorithm SHA256).Hash } else { $null }
$dwm=python (Join-Path $PSScriptRoot 'display_timing.py') | ConvertFrom-Json
$display=Get-CimInstance Win32_VideoController | Select-Object Name,DriverVersion,CurrentRefreshRate,CurrentHorizontalResolution,CurrentVerticalResolution
for ($run=1; $run -le $Runs; $run++) {
    if ((Get-PSDrive C).Free -lt 3GB) { throw 'Keep 2 GiB free on C plus 1 GiB headroom for this run' }
    $started=Get-Date
    $stamp=$started.ToString('yyyyMMdd-HHmmss')+"-$run"
    $data=Join-Path (Split-Path -Parent $resolvedExecutable) 'data'
    $before=@(Get-ChildItem -LiteralPath $data -Filter 'performance-*.jsonl' -ErrorAction SilentlyContinue | ForEach-Object FullName)
    $process=Start-Process -FilePath $resolvedExecutable -WindowStyle Normal -PassThru
    $capture=$null
    $csv=Join-Path $OutputDirectory "$stamp-presentmon.csv"
    if (!$SkipPresentMon) {
        $capture=Start-Process -FilePath $PresentMon -WindowStyle Hidden -PassThru -ArgumentList @('--process_id',"$($process.Id)",'--output_file',"`"$csv`"",'--qpc_time','--timed',"$($Seconds+30)",'--terminate_after_timed','--terminate_on_proc_exit','--no_console_stats','--session_name',"tuhai-$stamp") -RedirectStandardError (Join-Path $OutputDirectory "$stamp-presentmon-errors.txt") -RedirectStandardOutput (Join-Path $OutputDirectory "$stamp-presentmon-output.txt")
    }
    $timedOut=$false
    while (!$process.WaitForExit(1000)) {
        if ((Get-PSDrive C).Free -lt 2GB) {
            $process.Kill()
            $process.WaitForExit()
            throw 'Stopped invalid run to preserve 2 GiB free on C'
        }
        if (((Get-Date)-$started).TotalSeconds -gt $Seconds+120) {
            $timedOut=$true
            $process.Kill()
            $process.WaitForExit()
            break
        }
    }
    if ($capture) { $null=$capture.WaitForExit(35000) }
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
    $report['source_disk']=$rootDisk
    $report | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath (Join-Path $OutputDirectory "$stamp-run.json") -Encoding utf8
    $report | ConvertTo-Json -Compress -Depth 8
    if ($process.ExitCode -ne 0) { throw "UI run $run failed" }
}
