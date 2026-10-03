$ErrorActionPreference = 'Stop'
$root = Resolve-Path "$PSScriptRoot\..\.."
$out = "$root\target\x86_64-pc-windows-msvc\release"
$vs = & "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe" -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
if (-not $vs) { throw 'MSVC x64 build tools not found. Install Visual Studio or Build Tools with the C++ workload.' }
& "$vs\Common7\Tools\Launch-VsDevShell.ps1" -Arch amd64 -HostArch amd64 -SkipAutomaticLocation | Out-Null
cl /nologo /W4 /WX /std:c11 /I"$root\crates\win-taskbar-host-ffi\include" "$PSScriptRoot\abi_check.c" /Fo"$out\abi_check.obj" /Fe"$out\abi_check.exe" /link "$out\win_taskbar_host.dll.lib" user32.lib
if ($LASTEXITCODE) { exit $LASTEXITCODE }
& "$out\abi_check.exe"
exit $LASTEXITCODE
