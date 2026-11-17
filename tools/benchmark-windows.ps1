param(
    [Parameter(Mandatory)][string]$Executable,
    [Parameter(Mandatory)][string]$OutputDirectory,
    [int]$Runs = 3,
    [int]$TabCycles = 0,
    [ValidateRange(2, 100)][int]$Tabs = 11,
    [switch]$NavigateTabs,
    [switch]$TrackSession,
    [switch]$ReuseProfile,
    [ValidateRange(0, 100)][int]$RestoreTabs = 0,
    [switch]$WakeRestoredTab,
    [ValidateRange(0, 60)][int]$IdleWaitSeconds = 9,
    [switch]$Chrome,
    [ValidateSet('standard', 'balanced', 'economy')][string]$EngineMemory,
    [int]$CyclePauseSeconds = 3,
    [ValidateRange(0, 300)][int]$BackgroundWaitSeconds = 0,
    [ValidateRange(0, 60)][int]$MinimizeWaitSeconds = 0,
    [int]$IdleSamples = 0,
    [int]$IdleSampleSeconds = 15
)
$ErrorActionPreference = 'Stop'
if ($RestoreTabs -and ($Chrome -or $TabCycles -or $NavigateTabs)) { throw 'Session restoration needs Lumen without tab cycles' }
if ($WakeRestoredTab -and $RestoreTabs -lt 2) { throw 'Waking a saved tab needs at least two saved tabs' }
$exe = (Get-Item -LiteralPath $Executable).FullName
New-Item -ItemType Directory -Path $OutputDirectory | Out-Null
$root = (Resolve-Path -LiteralPath $OutputDirectory).Path
function Measure-Family([int]$ProcessId) {
    $all = Get-CimInstance Win32_Process -Property ProcessId, ParentProcessId, CreationDate
    $byId = @{}
    foreach ($row in $all) { $byId[[int]$row.ProcessId] = $row }
    $rootCreated = $byId[$ProcessId].CreationDate
    if (-not $rootCreated) { throw 'Lumen process identity could not be verified' }
    $family = [Collections.Generic.HashSet[int]]::new()
    [void]$family.Add($ProcessId)
    do {
        $previous = $family.Count
        foreach ($p in $all) {
            if ($family.Contains([int]$p.ParentProcessId) -and $p.CreationDate -ge $rootCreated) {
                $parent = $byId[[int]$p.ParentProcessId]
                if ($parent.CreationDate -le $p.CreationDate) { [void]$family.Add([int]$p.ProcessId) }
            }
        }
    } while ($family.Count -gt $previous)
    $identities = @{}
    $processes = @(Get-Process -Id @($family) -ErrorAction SilentlyContinue | Where-Object {
        $created = $_.StartTime
        if (!$created -or [math]::Abs(($created - $byId[$_.Id].CreationDate).TotalSeconds) -ge 1) { return $false }
        $identities[$_.Id] = "$($_.Id):$($created.ToUniversalTime().Ticks)"
        $true
    })
    $roles = @{}
    if ($NavigateTabs) {
        $filter = ($processes.Id | ForEach-Object { "ProcessId = $_" }) -join ' OR '
        $roleByPid = @{}
        if ($filter) {
            foreach ($row in Get-CimInstance Win32_Process -Filter $filter -Property ProcessId,CommandLine,Name) {
                $role = if ($row.ProcessId -eq $ProcessId) { if ($Chrome) { 'chrome' } else { 'lumen' } }
                    elseif ($row.CommandLine -match '--type=renderer\b') { 'renderer' }
                    elseif ($row.CommandLine -match '--type=gpu-process\b') { 'gpu' }
                    elseif ($row.CommandLine -match '--type=utility\b') { 'utility' }
                    elseif ($row.Name -eq 'msedgewebview2.exe') { 'webview-browser' }
                    else { 'other' }
                $roleByPid[[int]$row.ProcessId] = $role
            }
        }
        foreach ($proc in $processes) {
            $role = $roleByPid[$proc.Id]
            if (-not $role) { $role = 'other' }
            if (-not $roles.ContainsKey($role)) { $roles[$role] = @{ processes = 0; privateMiB = 0.0; handles = 0 } }
            $roles[$role].processes++
            $roles[$role].privateMiB += $proc.PrivateMemorySize64 / 1MB
            $roles[$role].handles += $proc.HandleCount
        }
        foreach ($role in $roles.Values) { $role.privateMiB = [math]::Round($role.privateMiB, 1) }
    }
    @{
        timestamp = [DateTimeOffset]::Now.ToString('o')
        roles = $roles
        ids = @($processes.Id)
        identities = @($identities.Values)
        processes = $processes.Count
        privateMiB = [math]::Round(($processes | Measure-Object PrivateMemorySize64 -Sum).Sum / 1MB, 1)
        workingSetMiB = [math]::Round(($processes | Measure-Object WorkingSet64 -Sum).Sum / 1MB, 1)
        cpuByProcess = @{} + ($processes | ForEach-Object -Begin { $ticks = @{} } -Process { $ticks[$identities[$_.Id]] = $_.CPU } -End { $ticks })
        threads = ($processes | ForEach-Object { $_.Threads.Count } | Measure-Object -Sum).Sum
    }
}
function Wait-Until([scriptblock]$Predicate, [int]$Milliseconds) {
    $watch = [Diagnostics.Stopwatch]::StartNew()
    do {
        if (& $Predicate) { return $true }
        Start-Sleep -Milliseconds 80
    } while ($watch.ElapsedMilliseconds -lt $Milliseconds)
    return $false
}
function Count-Survivors($Identities) {
    $count = 0
    foreach ($identity in $Identities) {
        $id, $ticks = $identity.Split(':', 2)
        $proc = Get-Process -Id ([int]$id) -ErrorAction SilentlyContinue
        $created = $proc.StartTime
        if ($created -and $created.ToUniversalTime().Ticks -eq [long]$ticks) { $count++ }
    }
    return $count
}
function Median($Values) {
    $v = @($Values | Sort-Object)
    $i = [int][math]::Floor(($v.Count - 1) / 2)
    if ($v.Count % 2) { return [math]::Round($v[$i], 2) }
    return [math]::Round(($v[$i] + $v[$i + 1]) / 2, 2)
}
function Open-SecondSavedTab($Browser) {
    Add-Type -AssemblyName UIAutomationClient
    Add-Type -AssemblyName UIAutomationTypes
    if (-not ('LumenBenchmarkMouse' -as [type])) {
        Add-Type @'
using System;
using System.Runtime.InteropServices;
public static class LumenBenchmarkMouse {
    [StructLayout(LayoutKind.Sequential)] struct Point { public int X; public int Y; }
    [DllImport("user32.dll")] static extern bool GetCursorPos(out Point point);
    [DllImport("user32.dll")] static extern bool SetCursorPos(int x, int y);
    [DllImport("user32.dll")] static extern void mouse_event(uint flags, uint x, uint y, uint data, UIntPtr extra);
    public static void Click(int x, int y) {
        Point previous;
        if (!GetCursorPos(out previous) || !SetCursorPos(x, y)) throw new InvalidOperationException();
        mouse_event(2, 0, 0, 0, UIntPtr.Zero);
        System.Threading.Thread.Sleep(40);
        mouse_event(4, 0, 0, 0, UIntPtr.Zero);
        System.Threading.Thread.Sleep(40);
        SetCursorPos(previous.X, previous.Y);
    }
}
'@
    }
    $shell = New-Object -ComObject WScript.Shell
    if (!$shell.AppActivate($Browser.Id)) { throw 'Could not activate the test window' }
    $Browser.Refresh()
    $tree = [Windows.Automation.AutomationElement]::FromHandle($Browser.MainWindowHandle)
    $name = [Windows.Automation.PropertyCondition]::new([Windows.Automation.AutomationElement]::NameProperty, 'Memory fixture 2')
    $label = $tree.FindFirst([Windows.Automation.TreeScope]::Descendants, $name)
    if (!$label -or $label.Current.IsOffscreen) { throw 'Saved tab label not visible in the test window' }
    $bounds = $label.Current.BoundingRectangle
    $started = [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds()
    [LumenBenchmarkMouse]::Click([int]($bounds.X + [math]::Min($bounds.Width / 2, 25)), [int]($bounds.Y + $bounds.Height / 2))
    $started
}
function Measure-Minimized($Browser) {
    if ($MinimizeWaitSeconds -eq 0) { return }
    if (-not ('LumenBenchmarkWindow' -as [type])) {
        Add-Type @'
using System;
using System.Runtime.InteropServices;
public static class LumenBenchmarkWindow {
    [DllImport("user32.dll")] public static extern bool IsIconic(IntPtr hwnd);
    [DllImport("user32.dll")] public static extern bool ShowWindowAsync(IntPtr hwnd, int command);
}
'@
    }
    $Browser.Refresh()
    $window = $Browser.MainWindowHandle
    if ($window -eq 0) { throw 'Could not identify the test window to minimize' }
    [void][LumenBenchmarkWindow]::ShowWindowAsync($window, 6)
    try {
        if (-not (Wait-Until { [LumenBenchmarkWindow]::IsIconic($window) } 5000)) { throw 'Minimize timed out' }
        Start-Sleep -Seconds $MinimizeWaitSeconds
        $snapshot = Measure-Family $Browser.Id
    } finally {
        [void][LumenBenchmarkWindow]::ShowWindowAsync($window, 9)
        if (-not (Wait-Until { -not [LumenBenchmarkWindow]::IsIconic($window) } 5000)) { throw 'Restore timed out' }
    }
    return $snapshot
}
function Measure-TabCycles($Browser, [int]$Cycles, [string]$ReadyFile, [string]$SamplesFile, [string]$ChromeProfile, [string]$Workload) {
    if ($Chrome) {
        $driver = Join-Path $PSScriptRoot 'benchmark-chrome-tabs.cjs'
        for ($cycle = 1; $cycle -le $Cycles; $cycle++) {
            & node $driver $ChromeProfile open $Workload $ReadyFile ($Tabs - 1)
            if ($LASTEXITCODE -ne 0) { throw 'Chrome tab workload failed' }
            $peak = Measure-Family $Browser.Id
            $background = if ($BackgroundWaitSeconds -gt 0) {
                Start-Sleep -Seconds $BackgroundWaitSeconds
                Measure-Family $Browser.Id
            }
            & node $driver $ChromeProfile close $Workload $ReadyFile ($Tabs - 1)
            if ($LASTEXITCODE -ne 0) { throw 'Chrome tab close failed' }
            Start-Sleep -Seconds $CyclePauseSeconds
            $row = @{ cycle = $cycle; tabs = $Tabs; peak = $peak; background = $background; afterClose = Measure-Family $Browser.Id }
            $row | ConvertTo-Json -Depth 8 -Compress | Add-Content -LiteralPath $SamplesFile
            $row
        }
        return
    }
    Add-Type -AssemblyName UIAutomationClient
    Add-Type -AssemblyName UIAutomationTypes
    $tree = $null
    $scope = [Windows.Automation.TreeScope]::Descendants
    $prop = [Windows.Automation.AutomationElement]::NameProperty
    $tabClose = [Windows.Automation.PropertyCondition]::new($prop, [string][char]0xD7)
    $plusButton = [Windows.Automation.PropertyCondition]::new(
        [Windows.Automation.AutomationElement]::AutomationIdProperty, 'plus')
    $invoke = [Windows.Automation.InvokePattern]::Pattern
    $toolbarWait = [Diagnostics.Stopwatch]::StartNew()
    do {
        $Browser.Refresh()
        if ($Browser.MainWindowHandle -ne 0) {
            $tree = [Windows.Automation.AutomationElement]::FromHandle($Browser.MainWindowHandle)
            $add = $tree.FindFirst($scope, $plusButton)
        }
        if (!$add) { Start-Sleep -Milliseconds 150 }
    } until ($add -or $toolbarWait.ElapsedMilliseconds -ge 7000)
    if (!$add) { throw "New tab button not exposed through UI Automation" }
    $linkCondition = [Windows.Automation.PropertyCondition]::new(
        [Windows.Automation.AutomationElement]::ControlTypeProperty, [Windows.Automation.ControlType]::Hyperlink)
    $pageCount = 12
    $result = for ($cycle = 1; $cycle -le $Cycles; $cycle++) {
        $newTabReadyMs = @()
        for ($n = 1; $n -lt $Tabs; $n++) {
            $expected = $n + 1
            $newTabTimer = [Diagnostics.Stopwatch]::StartNew()
            $add.GetCurrentPattern($invoke).Invoke()
            if (-not (Wait-Until { $tree.FindAll($scope, $tabClose).Count -ge $expected } 6000)) { throw "Tab open timed out" }
            if ($NavigateTabs) {
                $link = $null
                if (-not (Wait-Until {
                    $script:fixtureLink = $tree.FindAll($scope, $linkCondition) |
                        Where-Object { -not $_.Current.IsOffscreen -and $_.Current.Name -match 'Memory fixture|/workload' } |
                        Select-Object -First 1
                    $null -ne $script:fixtureLink
                } 6000)) { throw "Local memory fixture link not found" }
                $newTabReadyMs += $newTabTimer.ElapsedMilliseconds
                $script:fixtureLink.GetCurrentPattern($invoke).Invoke()
                $pageCount++
                if (-not (Wait-Until { @(Get-Content -LiteralPath $ReadyFile).Count -ge $pageCount } 6000)) {
                    throw "Local memory fixture navigation timed out"
                }
            }
        }
        $peak = Measure-Family $Browser.Id
        $minimized = Measure-Minimized $Browser
        $background = if ($BackgroundWaitSeconds -gt 0) {
            Start-Sleep -Seconds $BackgroundWaitSeconds
            Measure-Family $Browser.Id
        }
        for ($n = $Tabs - 1; $n -ge 1; $n--) {
            $closeButtons = $tree.FindAll($scope, $tabClose)
            try { $closeButtons[$closeButtons.Count - 1].GetCurrentPattern($invoke).Invoke() }
            catch [System.Management.Automation.MethodInvocationException] {
                if ($_.Exception.InnerException -isnot [Windows.Automation.ElementNotAvailableException]) { throw }
                $closeButtons = $tree.FindAll($scope, $tabClose)
                if ($closeButtons.Count -gt $n) { $closeButtons[$closeButtons.Count - 1].GetCurrentPattern($invoke).Invoke() }
            }
            if (-not (Wait-Until { $tree.FindAll($scope, $tabClose).Count -eq $n } 6000)) { throw "Tab close timed out" }
        }
        Start-Sleep -Seconds $CyclePauseSeconds
        $row = @{ cycle = $cycle; tabs = $Tabs; peak = $peak; background = $background; minimized = $minimized; afterClose = Measure-Family $Browser.Id; newTabReadyMs = $newTabReadyMs }
        $row | ConvertTo-Json -Depth 8 -Compress | Add-Content -LiteralPath $SamplesFile
        $row
    }
    return @($result)
}
$sharedProfile = Join-Path $root 'shared-profile'
if ($ReuseProfile) { New-Item -ItemType Directory $sharedProfile | Out-Null }
$rows = foreach ($run in 1..$Runs) {
    $dir = Join-Path $root "run-$run"
    New-Item -ItemType Directory $dir | Out-Null
    $profile = if ($ReuseProfile) { $sharedProfile } else { $dir }
    $env:LUMEN_DATA_DIR = $profile
    Remove-Item Env:LUMEN_TEST_HIDDEN -ErrorAction SilentlyContinue
    if ($EngineMemory -and (!$ReuseProfile -or $run -eq 1)) {
        @{ engine_memory = $EngineMemory } | ConvertTo-Json -Compress |
            Set-Content -LiteralPath (Join-Path $profile 'settings.json')
    }
    $portFile = Join-Path $dir 'port'
    $readyFile = Join-Path $dir 'pages.jsonl'
    $fixture = Join-Path $PSScriptRoot 'performance-fixture.cjs'
    $server = Start-Process -FilePath (Get-Command node).Source -ArgumentList ('"'+$fixture+'" "'+$portFile+'" "'+$readyFile+'" 12') -WindowStyle Hidden -PassThru
    $browser = $null
    $sessionWatcher = $null
    $sessionEvents = @()
    try {
        if ($RestoreTabs) {
            $settings = @{ startup = 'restore'; adblock_level = 'basic' }
            if ($EngineMemory) { $settings.engine_memory = $EngineMemory }
            $settings | ConvertTo-Json -Compress | Set-Content -LiteralPath (Join-Path $profile 'settings.json')
        }
        if (-not (Wait-Until { Test-Path -LiteralPath $portFile } 10000)) { throw 'Fixture startup timed out' }
        $port = Get-Content -LiteralPath $portFile -Raw
        if ($RestoreTabs) {
            @{ active = 0; tabs = @(1..$RestoreTabs | ForEach-Object {
                @{ u = "http://127.0.0.1:$port/workload?tab=$_"; t = "Memory fixture $_" }
            }) } | ConvertTo-Json -Depth 3 -Compress | Set-Content -LiteralPath (Join-Path $profile 'session.json')
        }
        if ($TrackSession -or $RestoreTabs) {
            $sessionWatcher = [IO.FileSystemWatcher]::new($profile, 'session.json')
            foreach ($eventName in @('Created','Changed','Renamed')) {
                $source = "session-$run-$eventName"
                Register-ObjectEvent -InputObject $sessionWatcher -EventName $eventName -SourceIdentifier $source | Out-Null
                $sessionEvents += $source
            }
            $sessionWatcher.EnableRaisingEvents = $true
        }
        if ($NavigateTabs) {
            ConvertTo-Json -InputObject @(@{ t = 'Memory fixture'; u = "http://127.0.0.1:$port/workload" }) -Compress |
                Set-Content -LiteralPath (Join-Path $profile 'links.json')
        }
        $start = [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds()
        $arguments = @('--url', "http://127.0.0.1:$port/page?step=1")
        if ($RestoreTabs) { $arguments = @('--restore-session') }
        elseif (-not $TrackSession) { $arguments = @('--secondary') + $arguments }
        $chromeProfile = Join-Path $profile 'chrome-profile'
        $workloadUrl = "http://127.0.0.1:$port/workload"
        if ($Chrome) {
            $arguments = @("--user-data-dir=`"$chromeProfile`"", '--no-first-run', '--no-default-browser-check',
                '--remote-debugging-port=0', "http://127.0.0.1:$port/page?step=1")
        }
        $browser = Start-Process -FilePath $exe -ArgumentList $arguments -PassThru
        $initialLoads = if ($RestoreTabs) { 1 } else { 12 }
        if (-not (Wait-Until { (Test-Path -LiteralPath $readyFile) -and @(Get-Content -LiteralPath $readyFile).Count -ge $initialLoads } 25000)) {
            throw 'Fixture navigation timed out'
        }
        $pages = @(Get-Content -LiteralPath $readyFile | ForEach-Object { ConvertFrom-Json $_ })
        $busy = Measure-Family $browser.Id
        if ($IdleWaitSeconds -gt 0) { Start-Sleep -Seconds $IdleWaitSeconds }
        $idle = Measure-Family $browser.Id
        if ($RestoreTabs) {
            $initial = @(Get-Content -LiteralPath $readyFile | ForEach-Object { ConvertFrom-Json $_ })
            if ($initial.Count -ne 1 -or $initial[0].tab -ne 1) { throw 'Unexpected page loads during session restoration' }
        }
        $sessionNotificationsBeforeWake = if ($RestoreTabs) {
            @($sessionEvents | ForEach-Object { Get-Event -SourceIdentifier $_ -ErrorAction SilentlyContinue }).Count
        }
        $wake = if ($WakeRestoredTab) {
            $wakeStart = Open-SecondSavedTab $browser
            if (-not (Wait-Until { @(Get-Content -LiteralPath $readyFile).Count -ge 2 } 6000)) { throw 'Sleeping tab did not load' }
            $page = Get-Content -LiteralPath $readyFile | Select-Object -Last 1 | ConvertFrom-Json
            if ($page.tab -ne 2) { throw 'The browser restored the wrong tab' }
            @{ loadMs = $page.time - $wakeStart; memory = Measure-Family $browser.Id }
        }
        $cycles = if ($TabCycles -gt 0) { @(Measure-TabCycles $browser $TabCycles $readyFile (Join-Path $dir "cycles.jsonl") $chromeProfile $workloadUrl) } else { @() }
        $idleTimeline = if ($IdleSamples -gt 0) {
            @(for ($sample = 1; $sample -le $IdleSamples; $sample++) {
                Start-Sleep -Seconds $IdleSampleSeconds
                Measure-Family $browser.Id
            })
        } else { @() }
        $idleCpu = $null
        if ($idleTimeline.Count -gt 0) {
            $last = @($idleTimeline)[-1]
            $seconds = ([DateTimeOffset]::Parse($last.timestamp) - [DateTimeOffset]::Parse($idle.timestamp)).TotalSeconds
            $used = 0.0
            foreach ($key in $idle.cpuByProcess.Keys) {
                if ($last.cpuByProcess.ContainsKey($key)) { $used += $last.cpuByProcess[$key] - $idle.cpuByProcess[$key] }
            }
            $idleCpu = [math]::Round(100 * $used / $seconds, 3)
        }
        $observed = @($busy.identities + $idle.identities + @($wake.memory.identities) + @($cycles | ForEach-Object { $_.peak.identities + $_.afterClose.identities }) + @($idleTimeline | ForEach-Object identities) | Sort-Object -Unique)
        $timer = [Diagnostics.Stopwatch]::StartNew()
        $browser.Refresh()
        if (-not $browser.CloseMainWindow()) { throw 'Graceful close could not be requested' }
        if (-not $browser.WaitForExit(12000)) { throw 'Lumen did not exit after closing its window' }
        $closeMs = $timer.ElapsedMilliseconds
        $exited = Wait-Until { (Count-Survivors $observed) -eq 0 } 12000
        $pageMedian = if (!$RestoreTabs) { Median @($pages | Select-Object -Skip 1 -ExpandProperty loadMs) }
        @{
            startupToFirstLoadMs = $pages[0].time - $start
            navigationBatchMs = $pages[-1].time - $pages[0].time
            pageLoadMedianMs = $pageMedian
            closeMs = $closeMs
            sessionNotifications = if ($TrackSession -or $RestoreTabs) {
                @($sessionEvents | ForEach-Object { Get-Event -SourceIdentifier $_ -ErrorAction SilentlyContinue }).Count
            } else { $null }
            busy = $busy
            idle = $idle
            wake = $wake
            sessionNotificationsBeforeWake = $sessionNotificationsBeforeWake
            tabCycles = $cycles
            idleTimeline = $idleTimeline
            idleCpuPercentOfOneCore = $idleCpu
            observedSurvivors = (Count-Survivors $observed)
            exited = $exited
        }
    } finally {
        if ($sessionWatcher) { $sessionWatcher.Dispose() }
        foreach ($source in $sessionEvents) {
            Unregister-Event -SourceIdentifier $source -ErrorAction SilentlyContinue
            Remove-Event -SourceIdentifier $source -ErrorAction SilentlyContinue
        }
        if ($browser -and -not $browser.HasExited) { Stop-Process -Id $browser.Id -ErrorAction SilentlyContinue }
        if (-not $server.HasExited) { Stop-Process -Id $server.Id -ErrorAction SilentlyContinue }
    }
}
$result = @{
    runs = @($rows)
    executable = $exe
    workload = if ($RestoreTabs) { 'Restore a saved local session.' } else { '12 sequential local HTTP documents, followed by optional tab cycles.' }
    restoredTabs = $RestoreTabs
    wakeRestoredTab = [bool]$WakeRestoredTab
    reuseProfile = [bool]$ReuseProfile
    idleWaitSeconds = $IdleWaitSeconds
    tabDriver = if ($Chrome) { 'Chrome DevTools Protocol' } else { 'Windows UI Automation' }
    engineMemory = $EngineMemory
    navigatedTabs = [bool]$NavigateTabs
    cyclePauseSeconds = $CyclePauseSeconds
    backgroundWaitSeconds = $BackgroundWaitSeconds
    minimizeWaitSeconds = $MinimizeWaitSeconds
    idleSampleSeconds = $IdleSampleSeconds
}
$result | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath (Join-Path $root 'performance.json')
$result | ConvertTo-Json -Depth 8
if (@($rows | Where-Object { -not $_.exited }).Count) { exit 1 }
