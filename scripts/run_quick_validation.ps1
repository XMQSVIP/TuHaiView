param(
    [ValidateSet('warmup','reproduce','compare','memory')][string]$Stage,
    [string]$OutputDirectory='F:\tuhai-validation\quick-v5',
    [string]$BaselineSsd='C:\tuhai-validation-v4\TuHaiView.exe',
    [string]$BaselineHdd='F:\tuhai-validation\validation-v4\TuHaiView.exe',
    [string]$CandidateSsd='C:\tuhai-validation-v5\TuHaiView.exe',
    [string]$CandidateHdd='F:\tuhai-validation\validation-v5\TuHaiView.exe'
)
$ErrorActionPreference='Stop'
$runner=Join-Path $PSScriptRoot 'run_ui_perf.ps1'
$cases=@(
    @{name='ssd-synthetic50k';old=$BaselineSsd;new=$CandidateSsd;root='C:\tuhai-fixtures-20260905\catalog';manifest='C:\tuhai-fixtures-20260905\manifest.json';alternate='C:\tuhai-fixtures-20260905\special';warmup=30},
    @{name='hdd-real50k';old=$BaselineHdd;new=$CandidateHdd;root='F:\tuhai-real-fixtures-20260906\catalog';manifest='F:\tuhai-real-fixtures-20260906\manifest.jsonl';alternate='G:\tuhai-fixtures-20260905\special';warmup=300}
)
if ((Get-FileHash -LiteralPath $CandidateSsd).Hash -ne (Get-FileHash -LiteralPath $CandidateHdd).Hash) { throw 'Candidate hash mismatch' }
if ((Get-FileHash -LiteralPath $BaselineSsd).Hash -ne (Get-FileHash -LiteralPath $BaselineHdd).Hash) { throw 'Baseline hash mismatch' }
$retryCounts=@{}
function Invoke-Round($case,$version,$scenario,$seconds,$label,[bool]$requireScan=$false) {
    $out=Join-Path $OutputDirectory ($case.name+'-'+$version+'-'+$label)
    if (!$retryCounts.ContainsKey($out)) {$retryCounts[$out]=0}
    while ($true) {
        $parameters=@{Executable=$case[$version];Root=$case.root;Manifest=$case.manifest;ExpectedRecords=50000;Scenario=$scenario;Seconds=$seconds;Runs=1;OutputDirectory=$out}
        if($requireScan){$parameters.RequireScanCompletion=$true}
        if($scenario -eq 'soak'){$parameters.AlternateRoot=$case.alternate}
        $before=@(Get-ChildItem -LiteralPath $out -Filter '*-run.json' -ErrorAction SilentlyContinue | ForEach-Object FullName)
        try {
            & $runner @parameters
            if($LASTEXITCODE -ne 0){throw "Run exited $LASTEXITCODE"}
            break
        } catch {
            # Archive only invalid attempts from this exact output directory;
            # valid performance failures are retained among the measured runs.
            $invalid=@(Get-ChildItem -LiteralPath $out -Filter '*-run.json' -ErrorAction SilentlyContinue | Where-Object { $_.FullName -notin $before } | Sort-Object LastWriteTime | Select-Object -Last 1)
            if(!$invalid){throw}
            $latest=$invalid[0]
            $archive=Join-Path $out 'invalid-attempts'
            New-Item -ItemType Directory -Path $archive -Force | Out-Null
            $prefix=$latest.BaseName -replace '-run$',''
            # Leave raw evidence at its recorded path. Move only metadata/reports.
            foreach($suffix in @('-run.json','-validated-summary.json','-summary.json')) {
                $source=Join-Path $out ($prefix+$suffix)
                if(Test-Path -LiteralPath $source){Move-Item -LiteralPath $source -Destination (Join-Path $archive ($prefix+$suffix))}
            }
            $retryCounts[$out]++
            if($retryCounts[$out] -gt 2){throw "Group stopped after two replacements: $out. $($_.Exception.Message)"}
        }
    }
}
foreach($case in $cases) {
    if($Stage -eq 'warmup') {
        foreach($version in @('old','new')) {
            Invoke-Round $case $version 'open' $case.warmup 'index-warmup' $true
            Invoke-Round $case $version 'scroll' 90 'route-warmup'
        }
    } elseif($Stage -eq 'reproduce') {
        Invoke-Round $case 'new' 'scroll' 60 'reproduce-scroll'
        Invoke-Round $case 'new' 'soak' 180 'reproduce-memory'
    } elseif($Stage -eq 'compare') {
        foreach($version in @('old','new')) {
            foreach($label in @('index-warmup','route-warmup')) {
                $folder=Join-Path $OutputDirectory ($case.name+'-'+$version+'-'+$label)
                $last=Get-ChildItem -LiteralPath $folder -Filter '*-run.json' -ErrorAction SilentlyContinue | Sort-Object LastWriteTime | Select-Object -Last 1
                if(!$last){throw "Missing independent warmup: $folder"}
                $meta=Get-Content -Raw -LiteralPath $last.FullName | ConvertFrom-Json
                if($meta.sha256 -ne (Get-FileHash -LiteralPath $case[$version]).Hash){throw 'Warmup hash differs from measured binary'}
                $summary=Get-Content -Raw -LiteralPath ($last.FullName -replace '-run.json$','-validated-summary.json') | ConvertFrom-Json
                if($summary.immediate_validation_errors.Count -gt 0){throw 'Warmup is invalid'}
                if($label -eq 'route-warmup' -and !$summary.scroll_cache_evidence.fully_resident){throw 'Warmup route has missing visible textures'}
            }
        }
        for($round=1;$round -le 5;$round++) {
            foreach($scenario in @('open','scroll')) {
                $versions=if($round%2 -eq 1){@('old','new')}else{@('new','old')}
                foreach($version in $versions) {
                    Invoke-Round $case $version $scenario $(if($scenario -eq 'open'){20}else{60}) $scenario
                }
            }
        }
        foreach($version in @('old','new')) {foreach($scenario in @('open','scroll')) {
            python (Join-Path $PSScriptRoot 'summarize_runs.py') (Join-Path $OutputDirectory ($case.name+'-'+$version+'-'+$scenario))
            if($LASTEXITCODE -ne 0){throw 'Aggregation failed'}
        }}
    } else {
        for($round=1;$round -le 3;$round++){Invoke-Round $case 'new' 'soak' 180 'memory'}
    }
}
