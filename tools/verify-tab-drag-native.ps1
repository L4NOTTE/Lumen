param([Parameter(Mandatory)][string]$Executable,[Parameter(Mandatory)][string]$OutputDirectory,[int]$Repeats=1)
$ErrorActionPreference='Stop'
Add-Type -AssemblyName UIAutomationClient
Add-Type -AssemblyName UIAutomationTypes
Add-Type -TypeDefinition @"
using System;
using System.Collections.Generic;
using System.Text;
using System.Runtime.InteropServices;
public static class LumenDragProbe {
    [DllImport("user32.dll")] public static extern bool SetCursorPos(int x,int y);
    [DllImport("user32.dll")] public static extern void mouse_event(uint flags,uint x,uint y,uint data,UIntPtr extra);
    [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr hwnd);
    private delegate bool EnumProc(IntPtr hwnd, IntPtr data);
    [DllImport("user32.dll")] private static extern bool EnumWindows(EnumProc callback, IntPtr data);
    [DllImport("user32.dll")] private static extern bool IsWindowVisible(IntPtr hwnd);
    [DllImport("user32.dll")] private static extern uint GetWindowThreadProcessId(IntPtr hwnd, out uint pid);
    [DllImport("user32.dll", CharSet=CharSet.Unicode)] private static extern int GetWindowText(IntPtr hwnd, StringBuilder text, int max);
    public static string Title(IntPtr hwnd) {
        var title = new StringBuilder(1024);
        GetWindowText(hwnd, title, title.Capacity);
        return title.ToString();
    }
    public static IntPtr[] Windows(uint pid) {
        var result = new List<IntPtr>();
        EnumWindows((hwnd, data) => {
            uint owner;
            GetWindowThreadProcessId(hwnd, out owner);
            if (owner == pid && IsWindowVisible(hwnd)) result.Add(hwnd);
            return true;
        }, IntPtr.Zero);
        return result.ToArray();
    }
}
"@
function Wait-Until($action) {
    for($n=0;$n -lt 800;$n++) {
        $result=& $action
        if($result){return $result}
        Start-Sleep -Milliseconds 10
    }
    throw 'Timed out waiting for the test UI'
}
function Elements($window){
    $window.FindAll([Windows.Automation.TreeScope]::Descendants,[Windows.Automation.Condition]::TrueCondition)
}
function Drag($source,$destinationX,$destinationY,$window) {
    $hwnd=[intptr]$window.Current.NativeWindowHandle
    [void][LumenDragProbe]::SetForegroundWindow($hwnd)
    if([LumenDragProbe]::GetForegroundWindow() -ne $hwnd){throw 'Test window does not have foreground focus'}
    $rect=$source.Current.BoundingRectangle
    $x=$rect.X+[math]::Min($rect.Width/2,25)
    $y=$rect.Y+$rect.Height/2
    if($rect.IsEmpty){throw 'Source tab has no visible bounding box'}
    [void][LumenDragProbe]::SetCursorPos($x,$y)
    [LumenDragProbe]::mouse_event(2,0,0,0,[UIntPtr]::Zero)
    try {
        for($i=1;$i -le 14;$i++){
            [void][LumenDragProbe]::SetCursorPos(($x+($destinationX-$x)*$i/14),($y+($destinationY-$y)*$i/14))
            Start-Sleep -Milliseconds 22
        }
    } finally {
        [LumenDragProbe]::mouse_event(4,0,0,0,[UIntPtr]::Zero)
    }
}
New-Item -ItemType Directory -Path $OutputDirectory | Out-Null
$dir=(Resolve-Path -LiteralPath $OutputDirectory).Path
$env:LUMEN_DATA_DIR=$dir
Remove-Item Env:LUMEN_TEST_HIDDEN -ErrorAction SilentlyContinue
'{"startup":"new","adblock":false}' | Set-Content (Join-Path $dir 'settings.json')
$portFile=Join-Path $dir 'port'
$pages=Join-Path $dir 'pages.jsonl'
$fixture=Join-Path $PSScriptRoot 'performance-fixture.cjs'
$server=Start-Process -FilePath (Get-Command node).Source -ArgumentList ('"'+$fixture+'" "'+$portFile+'" "'+$pages+'" 1') -WindowStyle Hidden -PassThru
$app=$null
try {
    [void](Wait-Until {Test-Path -LiteralPath $portFile})
    $port=Get-Content -LiteralPath $portFile -Raw
    $app=Start-Process -FilePath $Executable -ArgumentList '--secondary','--pos','40,60','--url',"http://127.0.0.1:$port/page?step=1" -PassThru
    [void](Wait-Until {Test-Path -LiteralPath $pages})
    $mainHandle=Wait-Until {[LumenDragProbe]::Windows($app.Id) | Select-Object -First 1}
    $main=[Windows.Automation.AutomationElement]::FromHandle($mainHandle)
    $add=Wait-Until {Elements $main | Where-Object {$_.Current.AutomationId -eq 'plus' -or $_.Current.Name -match 'Ctrl\+T'} | Select-Object -First 1}
    Write-Host "Main toolbar ready"
    $add.GetCurrentPattern([Windows.Automation.InvokePattern]::Pattern).Invoke()
    Start-Sleep -Milliseconds 400
    $samples=@(for($run=1;$run -le $Repeats;$run++) {
    $page=Wait-Until {Elements $main | Where-Object {$_.Current.Name -eq 'Lumen performance fixture' -and $_.Current.BoundingRectangle.Height -lt 38 -and $_.Current.BoundingRectangle.Y -lt ($main.Current.BoundingRectangle.Y + 45)} | Select-Object -First 1}
    $rect=$page.Current.BoundingRectangle
    $handle=$main.Current.NativeWindowHandle
    Drag $page ($rect.X+180) ($rect.Y+130) $main
    $detachTimer=[Diagnostics.Stopwatch]::StartNew();
    $detachedHandle=Wait-Until {[LumenDragProbe]::Windows($app.Id) | Where-Object {$_ -ne [intptr]$handle -and [LumenDragProbe]::Title($_) -eq 'Lumen performance fixture'} | Select-Object -First 1}
    $detachMs=$detachTimer.Elapsed.TotalMilliseconds
    $detached=[Windows.Automation.AutomationElement]::FromHandle($detachedHandle)
    $tab=Wait-Until {Elements $detached | Where-Object {$_.Current.Name -eq 'Lumen performance fixture' -and $_.Current.BoundingRectangle.Height -lt 65 -and $_.Current.BoundingRectangle.Y -lt ($detached.Current.BoundingRectangle.Y + 70)} | Select-Object -First 1}
    $mainRect=$main.Current.BoundingRectangle
    $destination=(Elements $main | Where-Object {$_.Current.AutomationId -eq 'plus' -or $_.Current.Name -match 'Ctrl\+T'} | Select-Object -First 1).Current.BoundingRectangle
    Drag $tab ($destination.X-4) ($destination.Y+$destination.Height/2) $detached
    $returnTimer=[Diagnostics.Stopwatch]::StartNew();
    [void](Wait-Until {@([LumenDragProbe]::Windows($app.Id) | Where-Object {[LumenDragProbe]::Title($_)}).Count -eq 1})
    $returnMs=$returnTimer.Elapsed.TotalMilliseconds
    $titles=@(Elements $main | Where-Object {$_.Current.Name -eq 'Lumen performance fixture' -and $_.Current.BoundingRectangle.Height -lt 38})
    $loads=@(Get-Content -LiteralPath $pages).Count
    if(!$titles.Count -or $loads -ne 1){throw 'Returned tab or page state missing'}
    if($run -eq 1 -and $titles[0].Current.BoundingRectangle.X -le $rect.X+30){throw 'Returned tab did not land after the remaining tab'}
    @{ handle=$detachedHandle.ToInt64(); detachMs=$detachMs; returnMs=$returnMs }
    })
    $loads=@(Get-Content -LiteralPath $pages).Count
    @{ detached=$true; returned=$true; droppedAfterOtherTab=$true; loads=$loads; samples=$samples } | ConvertTo-Json |
        Tee-Object -FilePath (Join-Path $dir 'drag-ui-results.json')
} finally {
    if($app -and -not $app.HasExited){[void]$app.CloseMainWindow();if(-not $app.WaitForExit(3000)){Stop-Process -Id $app.Id}}
    if(-not $server.HasExited){Stop-Process -Id $server.Id}
}



