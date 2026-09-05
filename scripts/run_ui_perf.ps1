param(
    [Parameter(Mandatory=$true)][string]$Executable,
    [Parameter(Mandatory=$true)][string]$Root,
    [int]$Runs=5,
    [int]$Seconds=60
)
$ErrorActionPreference='Stop'
$resolvedExecutable=(Resolve-Path -LiteralPath $Executable).Path
$resolvedRoot=(Resolve-Path -LiteralPath $Root).Path
$env:TUHAI_PERF='1'
$env:TUHAI_PERF_ROOT=$resolvedRoot
$env:TUHAI_PERF_SECONDS="$Seconds"
Remove-Item Env:/TUHAI_PERF_CAPTURE -ErrorAction SilentlyContinue
Remove-Item Env:/TUHAI_PERF_PRESENT -ErrorAction SilentlyContinue
Remove-Item Env:/TUHAI_PERF_LATENCY -ErrorAction SilentlyContinue
for ($run=1; $run -le $Runs; $run++) {
    $started=Get-Date
    $process=Start-Process -FilePath $resolvedExecutable -WindowStyle Hidden -PassThru
    while (!$process.WaitForExit(1000)) {}
    [pscustomobject]@{run=$run;pid=$process.Id;started=$started.ToString('o');exit_code=$process.ExitCode;seconds=((Get-Date)-$started).TotalSeconds} | ConvertTo-Json -Compress
    if ($process.ExitCode -ne 0) { throw "UI run $run failed" }
}
