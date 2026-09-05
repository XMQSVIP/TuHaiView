param(
    [Parameter(Mandatory=$true)][string]$Executable,
    [Parameter(Mandatory=$true)][string]$SourceImage,
    [string]$OutputDirectory='F:\tuhai-validation\permission-regression'
)
$ErrorActionPreference='Stop'
if (Get-Process -Name TuHaiView,dx12_memory_probe -ErrorAction SilentlyContinue) {
    throw 'Run filesystem correctness checks separately from graphical performance tests'
}
$source=(Resolve-Path -LiteralPath $SourceImage).Path
if ([IO.Path]::GetExtension($source).ToLowerInvariant() -notin '.jpg','.jpeg') {
    throw 'Use a JPEG copy for the trailing-byte version change'
}
$exe=(Resolve-Path -LiteralPath $Executable).Path
$base=[IO.Path]::GetFullPath($OutputDirectory)
$testRoot=[IO.Path]::GetFullPath((Join-Path $base ('case-'+[guid]::NewGuid().ToString('N'))))
if (!$testRoot.StartsWith($base.TrimEnd('\')+'\',[StringComparison]::OrdinalIgnoreCase)) {
    throw 'Test path escaped its output directory'
}
$catalog=Join-Path $testRoot 'pictures'
$protected=Join-Path $catalog 'temporarily-unreadable'
$healthy=Join-Path $catalog 'healthy'
New-Item -ItemType Directory -Path $protected,$healthy -Force | Out-Null
$protectedFile=Join-Path $protected ('protected'+[IO.Path]::GetExtension($source))
Copy-Item -LiteralPath $source -Destination $protectedFile
Copy-Item -LiteralPath $source -Destination (Join-Path $healthy ('healthy'+[IO.Path]::GetExtension($source)))
$testExe=Join-Path $testRoot 'TuHaiView.exe'
Copy-Item -LiteralPath $exe -Destination $testExe
$manifest=Join-Path $testRoot 'manifest.json'
@{source_image=$source;source_sha256=(Get-FileHash -LiteralPath $source).Hash;count=2} |
    ConvertTo-Json | Set-Content -LiteralPath $manifest -Encoding utf8
$runner=Join-Path $PSScriptRoot 'run_ui_perf.ps1'
$report=[ordered]@{test='temporary directory access denial';executable_sha256=(Get-FileHash -LiteralPath $exe).Hash;root=$testRoot;passed=$false;acl_restored=$false;dacl_rules_restored=$false;stages=@()}
function Rule-Signature($Security) {
    # Set-Acl may add SE_DACL_AUTO_INHERITED without changing any access rule.
    # Compare actual ACEs and inheritance protection, not the SDDL control flags.
    $rules=$Security.GetAccessRules($true,$true,[Security.Principal.SecurityIdentifier]) | ForEach-Object {
        '{0}|{1}|{2}|{3}|{4}|{5}' -f $_.IdentityReference.Value,[int]$_.FileSystemRights,$_.AccessControlType,$_.InheritanceFlags,$_.PropagationFlags,$_.IsInherited
    }
    return ([string]$Security.AreAccessRulesProtected)+'|'+(($rules | Sort-Object) -join ';')
}
function Run-Stage([string]$Name) {
    $out=Join-Path $testRoot $Name
    # Correctness run only. No claim about rendered/input latency without PresentMon.
    & $runner -Executable $testExe -Root $catalog -Manifest $manifest -ExpectedRecords 2 -Scenario open -Seconds 10 -Runs 1 -SkipPresentMon -OutputDirectory $out *> (Join-Path $testRoot ($Name+'-console.txt'))
    $metadata=@(Get-ChildItem -LiteralPath $out -Filter '*-run.json')
    if ($metadata.Count -ne 1) { throw 'Expected exactly one run metadata file' }
    $metadata=$metadata[0]
    $run=Get-Content -LiteralPath $metadata.FullName -Raw | ConvertFrom-Json
    if ($run.exit_code -ne 0 -or $run.timed_out -or $run.abort_reason) { throw "$Name did not finish" }
    $db=Join-Path $testRoot 'data\catalog.sqlite3'
    $summary=& python (Join-Path $PSScriptRoot 'verify_permission_stage.py') $db $protectedFile $run.logs[0] $Name
    if ($LASTEXITCODE -ne 0) { throw "$Name catalog or log verification failed" }
    $report.stages+=($summary | ConvertFrom-Json)
}
$acl=Get-Acl -LiteralPath $protected
$acl.GetSecurityDescriptorSddlForm([Security.AccessControl.AccessControlSections]::Access) |
    Set-Content -LiteralPath (Join-Path $testRoot 'original-dacl.sddl')
try {
    Run-Stage 'readable'
    $denied=Get-Acl -LiteralPath $protected
    $identity=[Security.Principal.WindowsIdentity]::GetCurrent().User
    $rule=[Security.AccessControl.FileSystemAccessRule]::new($identity,
        [Security.AccessControl.FileSystemRights]::ReadAndExecute,
        [Security.AccessControl.InheritanceFlags]'ContainerInherit,ObjectInherit',
        [Security.AccessControl.PropagationFlags]::None,
        [Security.AccessControl.AccessControlType]::Deny)
    $denied.AddAccessRule($rule)
    Set-Acl -LiteralPath $protected -AclObject $denied
    $denialObserved=$false
    try { Get-ChildItem -LiteralPath $protected -ErrorAction Stop | Out-Null }
    catch [System.UnauthorizedAccessException] { $denialObserved=$true }
    if (!$denialObserved) { throw 'This environment did not enforce the test ACL; do not claim permission failure was exercised' }
    Run-Stage 'denied'
} finally {
    # Restore only the directory created above, even when app/verification failed.
    Set-Acl -LiteralPath $protected -AclObject $acl
    $report.dacl_rules_restored=(Rule-Signature $acl) -eq (Rule-Signature (Get-Acl -LiteralPath $protected))
    $report.acl_restored=$report.dacl_rules_restored
    $report | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath (Join-Path $testRoot 'result.json') -Encoding utf8
    if (!$report.acl_restored) { throw 'Original access rules or inheritance protection were not restored' }
}
# Change only the test copy after permissions recover; JPEG permits trailing data.
$stream=[IO.File]::Open($protectedFile,[IO.FileMode]::Append,[IO.FileAccess]::Write)
try { $stream.WriteByte(0) } finally { $stream.Dispose() }
Run-Stage 'restored'
$report.passed=$true
$report | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath (Join-Path $testRoot 'result.json') -Encoding utf8
$report | ConvertTo-Json -Depth 8
