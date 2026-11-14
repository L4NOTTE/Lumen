param(
    [Parameter(Mandatory)][string]$Executable,
    [Parameter(Mandatory)][string]$OutputDirectory,
    [int]$Runs = 3,
    [int]$TabCycles = 0,
    [ValidateRange(2, 100)][int]$Tabs = 11,
    [switch]$NavigateTabs,
    [switch]$TrackSession,
    [int]$CyclePauseSeconds = 3,
    [int]$IdleSamples = 0,
    [int]$IdleSampleSeconds = 15
)
$ErrorActionPreference = 'Stop'
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
    $processes = @(Get-Process -Id @($family) -ErrorAction SilentlyContinue)
    $roles = @{}
    if ($NavigateTabs) {
        $filter = ($processes.Id | ForEach-Object { "ProcessId = $_" }) -join ' OR '
        $roleByPid = @{}
        if ($filter) {
            foreach ($row in Get-CimInstance Win32_Process -Filter $filter -Property ProcessId,CommandLine,Name) {
                $role = if ($row.ProcessId -eq $ProcessId) { 'lumen' }
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
        identities = @($processes | ForEach-Object { "$($_.Id):$($_.StartTime.ToUniversalTime().Ticks)" })
        processes = $processes.Count
        privateMiB = [math]::Round(($processes | Measure-Object PrivateMemorySize64 -Sum).Sum / 1MB, 1)
        workingSetMiB = [math]::Round(($processes | Measure-Object WorkingSet64 -Sum).Sum / 1MB, 1)
        cpuByProcess = @{} + ($processes | ForEach-Object -Begin { $ticks = @{} } -Process { $ticks["$($_.Id):$($_.StartTime.ToUniversalTime().Ticks)"] = $_.CPU } -End { $ticks })
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
        if ($proc -and $proc.StartTime.ToUniversalTime().Ticks -eq [long]$ticks) { $count++ }
    }
    return $count
}
function Median($Values) {
    $v = @($Values | Sort-Object)
    $i = [int][math]::Floor(($v.Count - 1) / 2)
    if ($v.Count % 2) { return [math]::Round($v[$i], 2) }
    return [math]::Round(($v[$i] + $v[$i + 1]) / 2, 2)
}
function Measure-TabCycles($Browser, [int]$Cycles, [string]$ReadyFile, [string]$SamplesFile) {
    Add-Type -AssemblyName UIAutomationClient
    Add-Type -AssemblyName UIAutomationTypes
    $Browser.Refresh()
    $tree = [Windows.Automation.AutomationElement]::FromHandle($Browser.MainWindowHandle)
    $scope = [Windows.Automation.TreeScope]::Descendants
    $prop = [Windows.Automation.AutomationElement]::NameProperty
    $tabClose = [Windows.Automation.PropertyCondition]::new($prop, [string][char]0xD7)
    $invoke = [Windows.Automation.InvokePattern]::Pattern
    $add = @($tree.FindAll($scope, [Windows.Automation.Condition]::TrueCondition) | Where-Object { $_.Current.Name -match "Ctrl\+T" -or $_.Current.AutomationId -eq "plus" }) | Select-Object -First 1
    if (-not $add) { throw "New tab button not exposed through UI Automation" }
    $linkCondition = [Windows.Automation.PropertyCondition]::new(
        [Windows.Automation.AutomationElement]::ControlTypeProperty, [Windows.Automation.ControlType]::Hyperlink)
    $pageCount = 12
    $result = for ($cycle = 1; $cycle -le $Cycles; $cycle++) {
        for ($n = 1; $n -lt $Tabs; $n++) {
            $expected = $n + 1
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
                $script:fixtureLink.GetCurrentPattern($invoke).Invoke()
                $pageCount++
                if (-not (Wait-Until { @(Get-Content -LiteralPath $ReadyFile).Count -ge $pageCount } 6000)) {
                    throw "Local memory fixture navigation timed out"
                }
            }
        }
        $peak = Measure-Family $Browser.Id
        for ($n = $Tabs - 1; $n -ge 1; $n--) {
            $tabs = $tree.FindAll($scope, $tabClose)
            $tabs[$tabs.Count - 1].GetCurrentPattern($invoke).Invoke()
            if (-not (Wait-Until { $tree.FindAll($scope, $tabClose).Count -eq $n } 6000)) { throw "Tab close timed out" }
        }
        Start-Sleep -Seconds $CyclePauseSeconds
        $row = @{ cycle = $cycle; tabs = $Tabs; peak = $peak; afterClose = Measure-Family $Browser.Id }
        $row | ConvertTo-Json -Depth 8 -Compress | Add-Content -LiteralPath $SamplesFile
        $row
    }
    return @($result)
}
$rows = foreach ($run in 1..$Runs) {
    $dir = Join-Path $root "run-$run"
    New-Item -ItemType Directory $dir | Out-Null
    $env:LUMEN_DATA_DIR = $dir
    Remove-Item Env:LUMEN_TEST_HIDDEN -ErrorAction SilentlyContinue
    '{"startup":"new","adblock":true,"adblock_level":"basic"}' | Set-Content (Join-Path $dir 'settings.json')
    $portFile = Join-Path $dir 'port'
    $readyFile = Join-Path $dir 'pages.jsonl'
    $fixture = Join-Path $PSScriptRoot 'performance-fixture.cjs'
    $server = Start-Process -FilePath (Get-Command node).Source -ArgumentList ('"'+$fixture+'" "'+$portFile+'" "'+$readyFile+'" 12') -WindowStyle Hidden -PassThru
    $browser = $null
    $sessionWatcher = $null
    $sessionEvents = @()
    try {
        if ($TrackSession) {
            $sessionWatcher = [IO.FileSystemWatcher]::new($dir, 'session.json')
            foreach ($eventName in @('Created','Changed','Renamed')) {
                $source = "session-$run-$eventName"
                Register-ObjectEvent -InputObject $sessionWatcher -EventName $eventName -SourceIdentifier $source | Out-Null
                $sessionEvents += $source
            }
            $sessionWatcher.EnableRaisingEvents = $true
        }
        if (-not (Wait-Until { Test-Path -LiteralPath $portFile } 10000)) { throw 'Fixture startup timed out' }
        $port = Get-Content -LiteralPath $portFile -Raw
        if ($NavigateTabs) {
            ConvertTo-Json -InputObject @(@{ t = 'Memory fixture'; u = "http://127.0.0.1:$port/workload" }) -Compress |
                Set-Content -LiteralPath (Join-Path $dir 'links.json')
        }
        $start = [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds()
        $arguments = @('--url', "http://127.0.0.1:$port/page?step=1")
        if (-not $TrackSession) { $arguments = @('--secondary') + $arguments }
        $browser = Start-Process -FilePath $exe -ArgumentList $arguments -PassThru
        if (-not (Wait-Until { (Test-Path -LiteralPath $readyFile) -and @(Get-Content -LiteralPath $readyFile).Count -ge 12 } 25000)) {
            throw 'Fixture navigation timed out'
        }
        $pages = @(Get-Content -LiteralPath $readyFile | ForEach-Object { ConvertFrom-Json $_ })
        $busy = Measure-Family $browser.Id
        Start-Sleep -Milliseconds 9000
        $idle = Measure-Family $browser.Id
        $cycles = if ($TabCycles -gt 0) { @(Measure-TabCycles $browser $TabCycles $readyFile (Join-Path $dir "cycles.jsonl")) } else { @() }
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
        $observed = @($busy.identities + $idle.identities + @($cycles | ForEach-Object { $_.peak.identities + $_.afterClose.identities }) + @($idleTimeline | ForEach-Object identities) | Sort-Object -Unique)
        $timer = [Diagnostics.Stopwatch]::StartNew()
        $browser.Refresh()
        if (-not $browser.CloseMainWindow()) { throw 'Graceful close could not be requested' }
        if (-not $browser.WaitForExit(12000)) { throw 'Lumen did not exit after closing its window' }
        $closeMs = $timer.ElapsedMilliseconds
        $exited = Wait-Until { (Count-Survivors $observed) -eq 0 } 12000
        @{
            startupToFirstLoadMs = $pages[0].time - $start
            navigationBatchMs = $pages[-1].time - $pages[0].time
            pageLoadMedianMs = Median @($pages | Select-Object -Skip 1 -ExpandProperty loadMs)
            closeMs = $closeMs
            sessionNotifications = if ($TrackSession) {
                @($sessionEvents | ForEach-Object { Get-Event -SourceIdentifier $_ -ErrorAction SilentlyContinue }).Count
            } else { $null }
            busy = $busy
            idle = $idle
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
$result = @{ runs = @($rows); executable = $exe; workload = 'Fresh profile and 12 sequential local HTTP documents, followed by optional UI Automation tab cycles.'; navigatedTabs = [bool]$NavigateTabs; cyclePauseSeconds = $CyclePauseSeconds; idleSampleSeconds = $IdleSampleSeconds }
$result | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath (Join-Path $root 'performance.json')
$result | ConvertTo-Json -Depth 8
if (@($rows | Where-Object { -not $_.exited }).Count) { exit 1 }
