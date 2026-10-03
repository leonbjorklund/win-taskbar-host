# Acceptance checks for a consumer app's taskbar content. Injects real input, so run it on a desktop nobody is using.
param([Parameter(Mandatory)][string]$Process)
$ErrorActionPreference = 'Stop'
$build = dotnet build "$PSScriptRoot\Driver" -c Release --nologo -v q
if ($LASTEXITCODE) { $build; throw 'Driver build failed' }
$driver = "$PSScriptRoot\Driver\bin\Release\net10.0-windows\Driver.exe"
$failed = $false
# The sentinel's title bar, from the fixed position Program.cs gives it. Clicks below it land inside it.
$SentinelX, $SentinelY = 600, 312

function D {
    $result = & $driver @args | ConvertFrom-Json
    if ($LASTEXITCODE -ne 0) { throw "Driver failed: $($result.error -join '; ')" }
    $result
}
function Record($check, $pass, $data) {
    if ($pass -eq $false) { $script:failed = $true }
    '{0,-22} {1} {2}' -f $check, $(if ($pass -eq $true) { 'pass' } elseif ($pass -eq $false) { 'FAIL' } else { $pass }), ($data | ConvertTo-Json -Compress -Depth 6)
}
function Widget { (D find WinTaskbarHost.Container).windows | Where-Object process -eq $Process | Select-Object -First 1 }
function Center($widget) { $widget.rect.left + [int]($widget.rect.width / 2) }
function Sentinel {
    $at = D hwnd-at $SentinelX $SentinelY
    if ($at.hwnd -ne $sentinelHwnd) { throw "The sentinel is covered at $SentinelX,$SentinelY by $($at.class)" }
    D click $SentinelX $SentinelY ';' sleep 250 | Out-Null
    if ((D fg).hwnd -ne $sentinelHwnd) { throw 'The sentinel did not become foreground' }
}
function SentinelKeys { @(Get-Content -LiteralPath $keyLog).Count }
# Activates the sentinel, then opens the content's menu.
function OpenMenu {
    Sentinel
    D click (Center (Widget)) $y right | Out-Null
    for ($i = 0; -not ($menu = (D sleep 250 ';' menu-items).menus | Select-Object -First 1); $i++) {
        if ($i -gt 8) { throw 'No menu opened' }
    }
    $menu
}
# Arms Move from the menu and returns the content's center.
function ArmMove {
    $item = (OpenMenu).items | Where-Object name -eq 'Move'
    if (-not $item) { throw 'No Move item in the menu' }
    $after = D click ($item.rect.left + 30) ($item.rect.top + [int]($item.rect.height / 2)) ';' sleep 300 ';' fg ';' menu-items
    # The host's hidden owner window holds the foreground while Move is armed, and while its menu is open.
    $owner = ($after | Where-Object cmd -eq 'fg').class -eq 'WinTaskbarHost.Controller'
    if (-not $owner -or @(($after | Where-Object cmd -eq 'menu-items').menus).Count) { throw 'Move did not arm' }
    Center (Widget)
}

$widget = Widget
if (-not $widget) { throw "$Process has no content in the taskbar" }
$taskbar = D taskbar
Record 'environment' 'info' @{ widget = $widget.rect; taskbar = $taskbar.rect; dpi = $taskbar.dpi }
$y = $widget.rect.top + [int]($widget.rect.height / 2)

$keyLog = Join-Path ([IO.Path]::GetTempPath()) "wth-sentinel-$PID.txt"
$sentinel = Start-Process $driver 'sentinel', "`"$keyLog`"", $PID -NoNewWindow -PassThru
try {
    for ($i = 0; -not ($sentinelHwnd = [long](Get-Process -Id $sentinel.Id).MainWindowHandle); $i++) {
        if ($i -gt 100) { throw 'The sentinel did not open' }
        Start-Sleep -Milliseconds 100
    }
    # A click reaches the content and, as on native taskbar buttons, activates the taskbar.
    Sentinel
    $at = D hwnd-at (Center $widget) $y
    D click (Center $widget) $y ';' sleep 300 | Out-Null
    $foreground = (D fg).process
    Record 'content-click' ($at.process -eq $Process -and $foreground -eq 'explorer') @{ target = $at.class; foregroundAfter = $foreground }

    # Move drags fast and releases far above the taskbar, then moves back.
    $direction = if ($widget.rect.left -gt $taskbar.rect.width / 2) { -1 } else { 1 }
    $start = (Widget).rect.left
    $from = ArmMove
    D drag $from $y ($from + 300 * $direction) ($y - 400) 6 ';' sleep 400 | Out-Null
    $moved = (Widget).rect.left
    $from = ArmMove
    D drag $from $y ($from - 300 * $direction) $y 6 ';' sleep 400 | Out-Null
    $back = (Widget).rect.left
    Record 'move-commit' ($moved -eq $start + 300 * $direction -and $back -eq $start) @{ start = $start; moved = $moved; back = $back }

    # Escape during a drag restores the position, and the foreground app never sees the key. One Driver chain holds
    # the button, so a failure releases it.
    $keys = SentinelKeys
    $start = (Widget).rect.left
    $from = ArmMove
    $chain = D drag $from $y ($from + 250 * $direction) $y 8 --no-release ';' sleep 200 ';' find WinTaskbarHost.Container ';' key ESCAPE ';' sleep 200 ';' up left ';' sleep 300
    $during = (($chain | Where-Object cmd -eq 'find').windows | Where-Object process -eq $Process | Select-Object -First 1).rect.left
    $restored = (Widget).rect.left
    $keysAdded = (SentinelKeys) - $keys
    Record 'escape-cancels-drag' ($null -ne $during -and $during -ne $start -and $restored -eq $start -and $keysAdded -eq 0) @{ start = $start; during = $during; after = $restored; sentinelKeysAdded = $keysAdded }

    # While Move is armed the host's hidden owner window holds the foreground, so an outside click must end that.
    $start = (Widget).rect.left
    ArmMove | Out-Null
    D click $SentinelX ($SentinelY + 100) ';' sleep 300 | Out-Null
    $after = (Widget).rect.left
    $foreground = D fg
    Record 'outside-click-cancels' ($after -eq $start -and $foreground.class -ne 'WinTaskbarHost.Controller') @{ start = $start; after = $after; foregroundAfter = $foreground.process }

    # A right-click cancels Move without opening the menu.
    $start = (Widget).rect.left
    $from = ArmMove
    $result = D click $from $y right ';' sleep 500 ';' fg ';' menu-items
    $foreground = $result | Where-Object cmd -eq 'fg'
    $open = @(($result | Where-Object cmd -eq 'menu-items').menus).Count
    if ($open) { D key ESCAPE ';' sleep 300 | Out-Null }
    $after = (Widget).rect.left
    Record 'right-click-cancels' ($open -eq 0 -and $after -eq $start -and $foreground.class -ne 'WinTaskbarHost.Controller') @{ menusOpen = $open; start = $start; after = $after; foregroundAfter = $foreground.process }

    $keys = SentinelKeys
    OpenMenu | Out-Null
    D key ESCAPE ';' sleep 300 | Out-Null
    $open = @((D menu-items).menus).Count
    $keysAdded = (SentinelKeys) - $keys
    Record 'menu-escape' ($open -eq 0 -and $keysAdded -eq 0) @{ menusOpen = $open; sentinelKeysAdded = $keysAdded }
    OpenMenu | Out-Null
    D click $SentinelX ($SentinelY + 100) ';' sleep 300 | Out-Null
    $open = @((D menu-items).menus).Count
    Record 'menu-outside-click' ($open -eq 0) @{ menusOpen = $open; foregroundAfter = (D fg).process }

    # Open and close Start with the Windows key while sampling the content's corners.
    $w = Widget
    $points = @(($w.rect.left + 4), ($w.rect.top + 3), ($w.rect.right - 5), ($w.rect.bottom - 4))
    Sentinel
    $job = Start-ThreadJob { param($driver, $points) & $driver watch 6000 50 @points } -ArgumentList $driver, $points
    Start-Sleep -Milliseconds 800
    # The content stays hit-testable, so Explorer's XAML layer never covers it.
    $open = D key LWIN ';' sleep 1500 ';' start-open ';' hwnd-at (Center $w) $y
    $shut = D key LWIN ';' sleep 1500 ';' start-open ';' hwnd-at (Center $w) $y
    $opened = ($open | Where-Object cmd -eq 'start-open').open
    $closed = -not ($shut | Where-Object cmd -eq 'start-open').open
    $onTop = @($open + $shut | Where-Object { $_.cmd -eq 'hwnd-at' -and $_.process -eq $Process }).Count -eq 2
    $watch = Receive-Job $job -Wait -AutoRemoveJob | ConvertFrom-Json
    $steady = $watch.ok -eq $true -and @($watch.points | Where-Object distinct -ne 1).Count -eq 0
    Record 'start-windows-key' ($opened -and $closed -and $steady -and $onTop) @{ opened = $opened; closed = $closed; onTop = $onTop; samples = $watch.samples; points = $watch.points }
} finally {
    Stop-Process -Id $sentinel.Id -ErrorAction Ignore
    Remove-Item -LiteralPath $keyLog -ErrorAction Ignore
}
if ($failed) { exit 1 }
