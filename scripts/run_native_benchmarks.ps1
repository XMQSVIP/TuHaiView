param(
    [string]$OutputDirectory='F:\tuhai-validation\native-final',
    [string]$CandidateTests='F:\tuhai-validation\candidate-tests.exe',
    [string]$BaselineTests='F:\tuhai-validation\baseline-tests.exe',
    [string]$ProductExecutable,
    [string]$ProductSourceRevision
)
$ErrorActionPreference='Stop'
New-Item -ItemType Directory -Path $OutputDirectory -Force | Out-Null
$candidate=(Resolve-Path -LiteralPath $CandidateTests).Path
$baseline=(Resolve-Path -LiteralPath $BaselineTests).Path
if (Get-Process -Name TuHaiView,dx12_memory_probe -ErrorAction SilentlyContinue) {
    throw 'Do not overlap native benchmarks with a running graphical test'
}
$provenance=[ordered]@{
    started=(Get-Date).ToString('o');candidate_tests=$candidate;baseline_tests=$baseline;
    candidate_test_sha256=(Get-FileHash -LiteralPath $candidate).Hash;
    baseline_test_sha256=(Get-FileHash -LiteralPath $baseline).Hash;
    product_executable=$ProductExecutable;product_source_revision=$ProductSourceRevision;
    product_sha256=if ($ProductExecutable) { (Get-FileHash -LiteralPath $ProductExecutable).Hash } else { $null };
    system_file_cache='unknown';completed=$false
}
$provenance | ConvertTo-Json -Depth 4 | Set-Content (Join-Path $OutputDirectory 'provenance.json') -Encoding utf8
$env:TUHAI_FIXTURES='G:\tuhai-fixtures-20260905'
$env:TUHAI_REAL_FIXTURES='F:\tuhai-real-fixtures-20260906'
$env:TUHAI_DB_BENCH_ROOT='F:\tuhai-validation\db-benchmark'
Remove-Item Env:\TUHAI_PERF -ErrorAction SilentlyContinue
function Invoke-Benchmark([string]$Executable,[string]$Filter,[string]$Name,[bool]$Ignored=$true) {
    $arguments=@($Filter,'--nocapture','--test-threads=1')
    if ($Ignored) { $arguments+='--ignored' }
    $process=Start-Process -FilePath $Executable -ArgumentList $arguments -WindowStyle Hidden -PassThru -Wait -RedirectStandardOutput (Join-Path $OutputDirectory ($Name+'.txt')) -RedirectStandardError (Join-Path $OutputDirectory ($Name+'-errors.txt'))
    if ($process.ExitCode -ne 0) { throw "Benchmark $Name failed with $($process.ExitCode)" }
    $testOutput=Get-Content -LiteralPath (Join-Path $OutputDirectory ($Name+'.txt')) -Raw
    if ($testOutput -notmatch 'test result: ok\. 1 passed; 0 failed; 0 ignored;') {
        throw "Benchmark $Name did not execute exactly one successful test"
    }
    Write-Output "$Name passed"
}
Invoke-Benchmark $candidate 'gpu_sliced_upload_readback_and_cancel' 'gpu'
Invoke-Benchmark $candidate 'jpeg_scaled_comparison_and_special_formats' 'jpeg'
Invoke-Benchmark $candidate 'real_jpeg_cache_compression' 'real-cache'
for ($run=1;$run -le 5;$run++) {
    $order=if ($run % 2 -eq 0) {@('candidate','baseline')} else {@('baseline','candidate')}
    foreach ($kind in $order) {
        $exe=if ($kind -eq 'candidate') {$candidate} else {$baseline}
        Invoke-Benchmark $exe 'batch_upsert_50k_completes_within_budget' "db-$kind-$run"
    }
    foreach ($fast in @(0,1)) {
        $env:TUHAI_PEAK_FAST="$fast"
        Invoke-Benchmark $candidate 'single_jpeg_process_peak' "peak-$fast-$run"
    }
    Invoke-Benchmark $candidate 'native_watcher_delivers_small_changes_without_manual_queueing' "watcher-$run" $false
}
Get-FileHash -LiteralPath $candidate,$baseline | ConvertTo-Json | Set-Content (Join-Path $OutputDirectory 'test-binary-hashes.json') -Encoding utf8
$provenance.completed=$true
$provenance | ConvertTo-Json -Depth 4 | Set-Content (Join-Path $OutputDirectory 'provenance.json') -Encoding utf8
