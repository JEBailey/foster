# Host adapter only: Clang parses C; the Foster program maps types and emits bindings.
[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$Header,
    [Parameter(Mandatory)][string]$Output,
    [string[]]$Sources = @(),
    [string[]]$Libraries = @(),
    [string[]]$IncludeDirectories = @(),
    [string[]]$Functions = @(),
    [string]$Foster = 'foster',
    [string]$Clang = 'clang',
    [switch]$SkipUnsupported,
    [switch]$ManifestOnly
)
$ErrorActionPreference = 'Stop'
$headerPath = (Resolve-Path -LiteralPath $Header).Path
$outputPath = [IO.Path]::GetFullPath($Output)
if ([IO.Path]::GetExtension($outputPath) -ne '.dll') { throw '-Output must name a .dll file.' }
$null = [IO.Directory]::CreateDirectory([IO.Path]::GetDirectoryName($outputPath))
$manifestPath = [IO.Path]::ChangeExtension($outputPath, '.bindings.json')
$reportPath = [IO.Path]::ChangeExtension($outputPath, '.unsupported.txt')
$declarationsPath = [IO.Path]::ChangeExtension($outputPath, '.declarations.tsv')
$astPath = [IO.Path]::ChangeExtension($outputPath, '.ast.json')
$utf8 = New-Object Text.UTF8Encoding($false)
$includePaths = @($IncludeDirectories | ForEach-Object { (Resolve-Path -LiteralPath $_).Path })
$sourcePaths = @($Sources | ForEach-Object { (Resolve-Path -LiteralPath $_).Path })
$libraryPaths = @($Libraries | ForEach-Object { (Resolve-Path -LiteralPath $_).Path })
# The shim keeps the exact header used for discovery in the reviewed manifest.
$shimPath = [IO.Path]::ChangeExtension($outputPath, '.import.h')
$shim = ''
if ($headerPath -match '["\r\n]') { throw 'Header path cannot contain quotes or newlines.' }
$shim += '#include "' + $headerPath.Replace('\', '/') + '"' + "`n"
[IO.File]::WriteAllText($shimPath, $shim, $utf8)
$clangArgs = @('--target=x86_64-pc-windows-msvc', '-x', 'c', '-std=c11', '-fsyntax-only', '-Xclang', '-ast-dump=json')
foreach ($directory in $includePaths) { $clangArgs += @('-I', $directory) }
$clangArgs += $shimPath
$astText = (& $Clang @clangArgs | Out-String)
if ($LASTEXITCODE -ne 0) { throw "Clang could not parse $headerPath" }
[IO.File]::WriteAllText($astPath, $astText, $utf8)
$ast = $astText | ConvertFrom-Json
$records = New-Object 'System.Collections.Generic.List[string]'
$found = New-Object 'System.Collections.Generic.HashSet[string]'
$currentFile = ''
foreach ($node in $ast.inner) {
    # Clang omits repeated file names on consecutive top-level declarations.
    $location = $node.loc
    if ($location.expansionLoc) { $location = $location.expansionLoc }
    if ($location.file) { $currentFile = [IO.Path]::GetFullPath($location.file) }
    if ($node.kind -eq 'TypedefDecl') {
        $underlying = $node.type.desugaredQualType
        if (!$underlying) { $underlying = $node.type.qualType }
        $records.Add("T`t$($node.name)`t$underlying")
    }
    if ($node.kind -ne 'FunctionDecl' -or $currentFile -ne $headerPath) { continue }
    if ($Functions.Count -gt 0 -and $node.name -cnotin $Functions) { continue }
    $null = $found.Add($node.name)
    $signature = $node.type.qualType
    $reason = ''
    if ($node.variadic) { $reason = 'variadic functions require a C adapter' }
    elseif ($node.storageClass -eq 'static' -or $node.inline) { $reason = 'static/inline functions require a C adapter' }
    elseif ($signature -notmatch '^([^()]+)\(([^()]*)\)$') { $reason = 'complex declarator requires a C adapter' }
    elseif ($Matches[2].Trim() -eq '') { $reason = 'old-style declaration does not specify parameter types; use (void)' }
    if ($reason) { $records.Add("R`t$($node.name)`t$reason"); continue }
    $returnType = $Matches[1].Trim()
    $fields = @('F', $node.name, $returnType)
    foreach ($parameter in $node.inner) {
        if ($parameter.kind -ne 'ParmVarDecl') { continue }
        $parameterType = $parameter.type.desugaredQualType
        if (!$parameterType) { $parameterType = $parameter.type.qualType }
        $fields += $parameterType
    }
    $records.Add(($fields -join "`t"))
}
foreach ($function in $Functions) {
    if (!$found.Contains($function)) { throw "No declaration for '$function' in $headerPath" }
}
if ($found.Count -eq 0) { throw "No function declarations found in $headerPath" }
[IO.File]::WriteAllText($declarationsPath, ($records -join "`n"), $utf8)
& $Foster run $PSScriptRoot -- $declarationsPath $shimPath.Replace('\', '/') $manifestPath $reportPath
if ($LASTEXITCODE -ne 0) { throw 'Foster header mapping failed.' }
$manifest = [IO.File]::ReadAllText($manifestPath) | ConvertFrom-Json
$manifest | Add-Member -NotePropertyName sources -NotePropertyValue $sourcePaths
$manifest | Add-Member -NotePropertyName libraries -NotePropertyValue $libraryPaths
$manifest | Add-Member -NotePropertyName include_directories -NotePropertyValue $includePaths
[IO.File]::WriteAllText($manifestPath, ($manifest | ConvertTo-Json -Depth 20), $utf8)
$report = [IO.File]::ReadAllText($reportPath)
if ($report) {
    Write-Warning $report
    if (!$SkipUnsupported) { throw "Unsupported declarations: review $reportPath and $manifestPath, or explicitly select -Functions / -SkipUnsupported." }
}
if ($ManifestOnly) { Write-Output $manifestPath; return }
if (@($manifest.operations).Count -eq 0) { throw 'No supported operations to build.' }
& $Foster bridge $manifestPath --output $outputPath --cc $Clang
if ($LASTEXITCODE -ne 0) { throw 'C bridge compilation failed.' }
Write-Output ([IO.Path]::ChangeExtension($outputPath, '.fos'))
