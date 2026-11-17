param([string]$Compiler='C:\Program Files (x86)\Inno Setup 6\ISCC.exe',[string]$Executable,[string]$OutputDirectory,[string]$XrayDirectory)
$ErrorActionPreference='Stop'
$project=Split-Path $PSScriptRoot -Parent
$arguments=@()
if($Executable){$exe=(Get-Item -LiteralPath $Executable).FullName;$arguments+=('/DAppSource='+$exe)}
if($XrayDirectory){
 $xray=(Get-Item -LiteralPath (Join-Path $XrayDirectory 'xray.exe')).FullName
 $license=(Get-Item -LiteralPath (Join-Path $XrayDirectory 'Xray-LICENSE.txt')).FullName
 $patch=(Get-Item -LiteralPath (Join-Path $XrayDirectory 'xray-dependencies.patch')).FullName
 $pinned='02920479df2e64d67c0bbf5a22cbbf153cd987bb8bffa18ebcdf8a942000b01b'
 if((Get-FileHash -LiteralPath $xray -Algorithm SHA256).Hash -ne $pinned){throw 'Xray-core hash does not match the pinned release'}
 $arguments+=('/DXraySource='+$xray),('/DXrayLicense='+$license),('/DXrayPatch='+$patch)
}
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
