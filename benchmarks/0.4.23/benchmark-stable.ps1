param([Parameter(Mandatory)][string]$Executable,[int]$Runs=3)
$ErrorActionPreference='Stop'
Add-Type -AssemblyName UIAutomationClient
Add-Type -AssemblyName UIAutomationTypes
$exe=(Resolve-Path -LiteralPath $Executable).Path
$profile=Join-Path ([IO.Path]::GetTempPath()) ('Lumen-bench-'+[guid]::NewGuid())
New-Item -ItemType Directory -Path $profile|Out-Null
'{"adblock":false,"startup":"new","sleep_minutes":0,"eco":false}' | Set-Content -Encoding ASCII (Join-Path $profile 'settings.json')
$env:LUMEN_DATA_DIR=$profile
Remove-Item Env:LUMEN_TEST_HIDDEN -ErrorAction SilentlyContinue
$scope=[Windows.Automation.TreeScope]::Descendants
$condition=[Windows.Automation.PropertyCondition]::new([Windows.Automation.AutomationElement]::NameProperty,[string][char]0xD7)
$plusCondition=[Windows.Automation.PropertyCondition]::new([Windows.Automation.AutomationElement]::AutomationIdProperty,'plus')
function Find-Plus {
 $tree.FindAll($scope,[Windows.Automation.Condition]::TrueCondition) | Where-Object {
  $_.Current.AutomationId -eq 'plus' -or $_.Current.Name -match 'Ctrl\+T'
 } | Select-Object -First 1
}
function Wait-For([scriptblock]$Test) {
 $watch=[Diagnostics.Stopwatch]::StartNew()
 do {
  $result=& $Test
  if($result){return $result}
  Start-Sleep -Milliseconds 5
 } while($watch.Elapsed.TotalSeconds -lt 15)
 throw 'UI Automation timeout'
}
function Measure-Memory([int]$RootId) {
 $all=Get-CimInstance Win32_Process -Property ProcessId,ParentProcessId
 $ids=[Collections.Generic.HashSet[int]]::new()
 [void]$ids.Add($RootId)
 do {
  $count=$ids.Count
  foreach($p in $all){if($ids.Contains([int]$p.ParentProcessId)){[void]$ids.Add([int]$p.ProcessId)}}
 } while($ids.Count -gt $count)
 $procs=@(Get-Process -Id @($ids) -ErrorAction SilentlyContinue)
 @{workingSetMiB=[math]::Round(($procs | Measure-Object WorkingSet64 -Sum).Sum/1MB,1);privateMiB=[math]::Round(($procs | Measure-Object PrivateMemorySize64 -Sum).Sum/1MB,1);processes=$procs.Count}
}
$results=for($run=0;$run -le $Runs;$run++){
 $watch=[Diagnostics.Stopwatch]::StartNew()
 $app=Start-Process -FilePath $exe -ArgumentList '--secondary','--pos','40,60' -PassThru
 try{
  $button=Wait-For {
   $app.Refresh()
   $handle=$app.MainWindowHandle
   if($handle -ne [IntPtr]::Zero){
    $script:tree=[Windows.Automation.AutomationElement]::FromHandle($handle)
    (Find-Plus)
   }
  }
  $startupMs=$watch.Elapsed.TotalMilliseconds

  $created=@()
  $count=1
  $rows=foreach($goal in 1,5,20){
   while($count -lt $goal){
    $count++
    $watch.Restart()
    for($retry=0;;$retry++){
     try{
      (Find-Plus).GetCurrentPattern([Windows.Automation.InvokePattern]::Pattern).Invoke()
      break
     }catch{
      if($tree.FindAll($scope,$condition).Count -eq $count){break}
      if($retry -ge 20){throw}
      Start-Sleep -Milliseconds 100
     }
    }
    [void](Wait-For {$tree.FindAll($scope,$condition).Count -eq $count})
    Start-Sleep -Milliseconds 120
    $created+=$watch.Elapsed.TotalMilliseconds
   }
   Start-Sleep -Seconds 2
   @{tabs=$goal;usage=Measure-Memory $app.Id}
  }
  if($run -gt 0){@{startupUiAutomationMs=[math]::Round($startupMs,1);createTabMeanMs=[math]::Round(($created|Measure-Object -Average).Average,2);rows=@($rows)}}
 }finally{
  if(!$app.HasExited){[void]$app.CloseMainWindow();if(!$app.WaitForExit(8000)){Stop-Process -Id $app.Id}}
 }
}
@{runs=@($results);sha256=(Get-FileHash -LiteralPath $exe).Hash;method='Windows UI Automation. One warmup followed by measured runs on one isolated profile. Memory after two seconds of idle at each tab count.'}|ConvertTo-Json -Depth 8



