# Generate reviewed pointer/resource bindings and run the headless image example.
[CmdletBinding()]
param([string]$Foster = '', [string]$Raylib = '', [string]$Cbind = '')
$ErrorActionPreference = 'Stop'
$root = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '../..'))
if (!$Foster) { $Foster = Join-Path $root 'target/debug/foster.exe' }
if (!$Raylib) { $Raylib = Join-Path $root 'target/raylib/raylib-6.0_win64_msvc16' }
$Foster = (Resolve-Path -LiteralPath $Foster).Path
$Raylib = (Resolve-Path -LiteralPath $Raylib).Path
$destination = Join-Path $root 'target/raylib-resources'
$null = [IO.Directory]::CreateDirectory($destination)
if (!$Cbind) {
    $Cbind = Join-Path $destination 'cbind.exe'
    & $Foster build (Join-Path $root 'tools/cbind') --native --output $Cbind
    if ($LASTEXITCODE -ne 0) { throw 'cbind compilation failed' }
}
$contracts = Join-Path $PSScriptRoot 'contracts.json'
$profile = Get-Content -LiteralPath $contracts -Raw | ConvertFrom-Json
$functions = @($profile.operations.symbol) + @($profile.resources.destroy) + @($profile.resources.valid) + @($profile.exclude)
$output = Join-Path $destination 'raylib_bridge.dll'
$bindingArguments = @('--header', (Join-Path $Raylib 'include/raylib.h'),
    '--include', (Join-Path $Raylib 'include'), '--library', (Join-Path $Raylib 'lib/raylibdll.lib'),
    '--contracts', $contracts, '--output', $output)
foreach ($function in ($functions | Sort-Object -Unique)) { $bindingArguments += @('--function', $function) }
& $Cbind @bindingArguments
if ($LASTEXITCODE -ne 0) { throw 'resource binding generation failed' }
Copy-Item -LiteralPath (Join-Path $Raylib 'lib/raylib.dll') -Destination (Join-Path $destination 'raylib.dll') -Force
$sourceDirectory = Join-Path $destination 'src'
$null = [IO.Directory]::CreateDirectory($sourceDirectory)
Copy-Item -LiteralPath ([IO.Path]::ChangeExtension($output, '.fos')) -Destination (Join-Path $sourceDirectory 'raylib.fos') -Force
Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'resources.fos') -Destination (Join-Path $sourceDirectory 'main.fos') -Force
[IO.File]::WriteAllText((Join-Path $destination 'foster.toml'), "[package]`nname = `"raylib_resources`"`nsource = `"src`"`n")
$fixture = Join-Path $destination 'fixture.txt'
[IO.File]::WriteAllText($fixture, ("Foster caf" + [char]0xE9 + "`n"), [Text.UTF8Encoding]::new($false))
$runOutput = @(& $Foster run $destination -- $fixture)
$runOutput | Write-Output
if ($LASTEXITCODE -ne 0 -or $runOutput.Count -eq 0 -or $runOutput[-1] -ne 'Result.Ok(42)') { throw 'resource example failed' }
