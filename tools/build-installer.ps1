param([string]$Compiler='C:\Program Files (x86)\Inno Setup 6\ISCC.exe',[string]$Executable,[string]$OutputDirectory)
$ErrorActionPreference='Stop'
$project=Split-Path $PSScriptRoot -Parent
$arguments=@()
if($Executable){$exe=(Get-Item -LiteralPath $Executable).FullName;$arguments+=('/DAppSource='+$exe)}
if($OutputDirectory){$arguments+=('/O'+[IO.Path]::GetFullPath($OutputDirectory))}
if($env:LUMEN_SIGN_PFX_BASE64 -or $env:LUMEN_SIGN_CERT_SHA1){
 $pwsh=(Get-Process -Id $PID).Path
 $signer=Join-Path $PSScriptRoot 'sign-windows.ps1'
 $arguments+='/DSignBuild'
 $arguments+=('/Slumen_sign=$q'+$pwsh+'$q -NoProfile -File $q'+$signer+'$q -RequireSignature -Files $f')
}
$arguments+=Join-Path $project 'installer/lumen.iss'
& $Compiler @arguments
if($LASTEXITCODE -ne 0){throw 'Inno Setup failed'}
