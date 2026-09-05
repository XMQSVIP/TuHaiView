param(
    [ValidateSet('short','presentation','memory-short','memory-full')][string]$Group='short',
    [string]$OutputDirectory='F:\tuhai-validation\final-matrix',
    [int]$Runs=5
)
$ErrorActionPreference='Stop'
$runner=Join-Path $PSScriptRoot 'run_ui_perf.ps1'
$hdd='F:\tuhai-validation\candidate\TuHaiView.exe'
$ssd='C:\tuhai-validation-candidate\TuHaiView.exe'
if ((Get-FileHash $hdd).Hash -ne (Get-FileHash $ssd).Hash) { throw 'SSD/HDD binary mismatch' }
$cases=@(
    @{name='synthetic-10k-ssd';exe=$ssd;root='C:\tuhai-synthetic-10k-20260906';manifest='C:\tuhai-fixtures-20260905\manifest.json'},
    @{name='synthetic-50k-ssd';exe=$ssd;root='C:\tuhai-fixtures-20260905\catalog';manifest='C:\tuhai-fixtures-20260905\manifest.json'},
    @{name='synthetic-10k-hdd';exe=$hdd;root='G:\tuhai-synthetic-10k-20260906';manifest='G:\tuhai-fixtures-20260905\manifest.json'},
    @{name='synthetic-50k-hdd';exe=$hdd;root='G:\tuhai-fixtures-20260905\catalog';manifest='G:\tuhai-fixtures-20260905\manifest.json'},
    @{name='real-10k-hdd';exe=$hdd;root='F:\tuhai-real-fixtures-20260906\catalog\part-01';manifest='F:\tuhai-real-fixtures-20260906\manifest.jsonl'},
    @{name='real-50k-hdd';exe=$hdd;root='F:\tuhai-real-fixtures-20260906\catalog';manifest='F:\tuhai-real-fixtures-20260906\manifest.jsonl'},
    @{name='real-shared-ssd';exe=$ssd;root='C:\tuhai-real-subset-20260906';manifest='C:\tuhai-real-subset-20260906\comparison-manifest.json'},
    @{name='real-shared-hdd';exe=$hdd;root='F:\tuhai-real-fixtures-20260906\comparison-512mib';manifest='F:\tuhai-real-fixtures-20260906\comparison-512mib\comparison-manifest.json'}
)
if ($Group -eq 'short') {
    foreach ($case in $cases) {
        # One explicit index/route warmup is excluded from the five measured repeats.
        & $runner -Executable $case.exe -Root $case.root -Manifest $case.manifest -Scenario scroll -Seconds 90 -Runs 1 -OutputDirectory (Join-Path $OutputDirectory ($case.name+'-warmup'))
        foreach ($scenario in @('open','scroll')) {
            $output=Join-Path $OutputDirectory ($case.name+'-'+$scenario)
            & $runner -Executable $case.exe -Root $case.root -Manifest $case.manifest -Scenario $scenario -Seconds $(if ($scenario -eq 'open') {20} else {60}) -Runs $Runs -OutputDirectory $output
            python (Join-Path $PSScriptRoot 'summarize_runs.py') $output
        }
    }
} elseif ($Group -eq 'presentation') {
    foreach ($configuration in @(@{name='mailbox';present='mailbox';delay=0},@{name='mailbox-paced';present='mailbox';delay=8},@{name='fifo';present='vsync';delay=0})) {
        $output=Join-Path $OutputDirectory $configuration.name
        & $runner -Executable $ssd -Root $cases[1].root -Manifest $cases[1].manifest -Scenario scroll -Seconds 60 -Runs $Runs -Present $configuration.present -RepaintMs $configuration.delay -OutputDirectory $output
        python (Join-Path $PSScriptRoot 'summarize_runs.py') $output
    }
} else {
    $seconds=if ($Group -eq 'memory-full') {1800} else {180}
    foreach ($case in @($cases[1],$cases[3])) {
        $drive=$case.root.Substring(0,1)
        $alternate="$drive`:\tuhai-fixtures-20260905\special"
        $output=Join-Path $OutputDirectory ($case.name+'-'+$Group)
        & $runner -Executable $case.exe -Root $case.root -Manifest $case.manifest -AlternateRoot $alternate -Scenario soak -Seconds $seconds -Runs $Runs -OutputDirectory $output
        python (Join-Path $PSScriptRoot 'summarize_runs.py') $output
    }
}
