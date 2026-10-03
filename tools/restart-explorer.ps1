# Restarts Explorer and checks that the app's taskbar content comes back in the same process, place and z-order.
param([Parameter(Mandatory)][string]$Process, [ValidateSet('graceful', 'abrupt')][string]$Mode = 'graceful')
$ErrorActionPreference = 'Stop'
$build = dotnet build "$PSScriptRoot\Driver" -c Release --nologo -v q
if ($LASTEXITCODE) { $build; throw 'Driver build failed' }
$driver = "$PSScriptRoot\Driver\bin\Release\net10.0-windows\Driver.exe"
function D { & $driver @args | ConvertFrom-Json }
function Widget { (D find WinTaskbarHost.Container).windows | Where-Object process -eq $Process | Select-Object -First 1 }

$before = Widget
if (-not $before) { throw "$Process has no content in the taskbar" }
$taskbar = D taskbar
$explorer = Get-Process -Id $taskbar.pid
$clock = [Diagnostics.Stopwatch]::StartNew()
$exited = $false
if ($Mode -eq 'graceful') {
    # Same as the taskbar's hidden "Exit Explorer" command: Explorer tears its windows down itself.
    Add-Type -Namespace Native -Name User32 -MemberDefinition '[DllImport("user32.dll")] public static extern bool PostMessage(IntPtr h, uint m, IntPtr w, IntPtr l);'
    [Native.User32]::PostMessage([IntPtr]$taskbar.hwnd, 0x5B4, 0, 0) | Out-Null
    $exited = $explorer.WaitForExit(20000)
}
if (-not $exited) {
    # A killed shell normally restarts by itself, and a second shell next to it never initializes its taskbar.
    Stop-Process -Id $explorer.Id -Force -ErrorAction Ignore
    $explorer.WaitForExit(10000) | Out-Null
    for ($i = 0; $i -lt 150 -and -not (D taskbar).ok; $i++) { Start-Sleep -Milliseconds 100 }
}
if (-not (D taskbar).ok) { Start-Process "$env:WINDIR\explorer.exe" }

while ($clock.Elapsed.TotalSeconds -lt 60) {
    $now = Widget
    if ($now -and $now.hwnd -ne $before.hwnd) { break }
    Start-Sleep -Milliseconds 100
}
$reattachMs = $clock.ElapsedMilliseconds
Start-Sleep -Milliseconds 1500
$after = Widget
$at = D hwnd-at ($after.rect.left + [int]($after.rect.width / 2)) ($after.rect.top + [int]($after.rect.height / 2))
$result = [ordered]@{
    requestedMode = $Mode
    mode = if ($exited) { 'graceful' } else { 'abrupt' }
    reattachMs = $reattachMs
    recreated = $after -and $after.hwnd -ne $before.hwnd
    samePid = $after.pid -eq $before.pid
    samePlace = $after.rect.left -eq $before.rect.left -and $after.rect.top -eq $before.rect.top -and $after.rect.width -eq $before.rect.width
    aboveTaskbar = $at.process -eq $Process
}
$result | ConvertTo-Json -Compress
if (-not ($result.recreated -and $result.samePid -and $result.samePlace -and $result.aboveTaskbar)) { exit 1 }
