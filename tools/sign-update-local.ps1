param(
 [Parameter(Mandatory)][string]$Version,
 [string]$WindowsInstaller='-',
 [string]$LinuxBinary='-',
 [Parameter(Mandatory)][string]$Output,
 [Parameter(Mandatory)][string]$ProtectedKey
)
$ErrorActionPreference='Stop'
$secure=(Get-Content -LiteralPath $ProtectedKey -Raw).Trim() | ConvertTo-SecureString
$pointer=[Runtime.InteropServices.Marshal]::SecureStringToBSTR($secure)
try {
 [Runtime.InteropServices.Marshal]::PtrToStringBSTR($pointer) | node (Join-Path $PSScriptRoot 'sign-update.cjs') $Version $WindowsInstaller $LinuxBinary $Output
 if($LASTEXITCODE-ne 0){throw 'Update signing failed'}
} finally {
 [Runtime.InteropServices.Marshal]::ZeroFreeBSTR($pointer)
}
