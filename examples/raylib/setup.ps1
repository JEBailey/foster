# Fetch the pinned official SDK and run the Foster header importer.
[CmdletBinding()]
param([string]$Foster = '', [string]$Raylib = '')
$ErrorActionPreference = 'Stop'
$root = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '../..'))
if (!$Foster) { $Foster = Join-Path $root 'target/debug/foster.exe' }
if (!(Test-Path -LiteralPath $Foster)) { throw 'Build the compiler first: cargo build --bin foster' }
$Foster = (Resolve-Path -LiteralPath $Foster).Path
if (!$Raylib) {
    $sdkCache = Join-Path $root 'target/raylib'
    $null = [IO.Directory]::CreateDirectory($sdkCache)
    $archive = Join-Path $sdkCache 'raylib-6.0_win64_msvc16.zip'
    if (!(Test-Path -LiteralPath $archive)) {
        Invoke-WebRequest 'https://github.com/raysan5/raylib/releases/download/6.0/raylib-6.0_win64_msvc16.zip' -OutFile $archive
    }
    $expected = 'C93C7DC74576E00E3EE57FA2BD5FD109FBFC5ACA87E12046DD7EC2C2268B3F78'
    if ((Get-FileHash -LiteralPath $archive -Algorithm SHA256).Hash -ne $expected) { throw 'raylib archive checksum mismatch' }
    Expand-Archive -LiteralPath $archive -DestinationPath $sdkCache -Force
    $Raylib = Join-Path $sdkCache 'raylib-6.0_win64_msvc16'
}
$Raylib = (Resolve-Path -LiteralPath $Raylib).Path
$outputDirectory = Join-Path $root 'target/raylib-demo'
$null = [IO.Directory]::CreateDirectory($outputDirectory)
$functions = @('DemoInitWindow', 'CloseWindow', 'IsWindowReady', 'SetTargetFPS', 'WindowShouldClose',
    'GetMousePosition', 'CheckCollisionPointRec', 'IsMouseButtonPressed', 'IsMouseButtonDown',
    'ColorFromHSV', 'GetFrameTime', 'BeginDrawing', 'EndDrawing', 'ClearBackground',
    'DrawRectangleRec', 'DrawText', 'TakeScreenshot')
$cbind = Join-Path $outputDirectory 'cbind.exe'
& $Foster build (Join-Path $root 'tools/cbind') --native --output $cbind
if ($LASTEXITCODE -ne 0) { throw 'cbind compilation failed' }
$bindingArguments = @('--header', (Join-Path $Raylib 'include/raylib.h'),
    '--header', (Join-Path $PSScriptRoot 'window.h'), '--source', (Join-Path $PSScriptRoot 'window.c'),
    '--include', (Join-Path $Raylib 'include'), '--library', (Join-Path $Raylib 'lib/raylibdll.lib'),
    '--c-string', 'DrawText:0', '--c-string', 'TakeScreenshot:0',
    '--output', (Join-Path $outputDirectory 'raylib_bridge.dll'))
foreach ($function in $functions) { $bindingArguments += @('--function', $function) }
& $cbind @bindingArguments
if ($LASTEXITCODE -ne 0) { throw 'raylib binding generation failed' }
Copy-Item -LiteralPath (Join-Path $Raylib 'lib/raylib.dll') -Destination (Join-Path $outputDirectory 'raylib.dll') -Force
Copy-Item -LiteralPath (Join-Path $outputDirectory 'raylib_bridge.fos') -Destination (Join-Path $PSScriptRoot 'src/raylib.fos') -Force
Write-Output 'Bindings ready. Run: ./target/debug/foster.exe run examples/raylib'
