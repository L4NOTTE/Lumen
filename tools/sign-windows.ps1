param([Parameter(Mandatory)][string[]]$Files,[string]$CertificateThumbprint=$env:LUMEN_SIGN_CERT_SHA1,[switch]$RequireSignature)
$ErrorActionPreference='Stop'
$pfx=$env:LUMEN_SIGN_PFX_BASE64
if(-not $pfx -and -not $CertificateThumbprint){
 if($RequireSignature -or $env:LUMEN_SIGN_PFX_PASSWORD){throw 'Signing is required or partially configured, but no certificate was provided'}
 Write-Output 'Unsigned build: no signing certificate configured. SmartScreen reputation is not guaranteed.'
 exit 0
}
$tool=Get-Command signtool.exe -ErrorAction SilentlyContinue
if($tool){$signtool=$tool.Source}else{
 $kits=Join-Path ${env:ProgramFiles(x86)} 'Windows Kits\10\bin'
 $signtool=Get-ChildItem -LiteralPath $kits -Filter signtool.exe -Recurse -File|Where-Object {$_.Directory.Name -eq 'x64'}|Sort-Object FullName -Descending|Select-Object -First 1 -ExpandProperty FullName
}
if(-not $signtool){throw 'Windows SDK SignTool is required'}
$targets=@($Files|ForEach-Object {(Get-Item -LiteralPath $_ -ErrorAction Stop).FullName})
$imported=@();$tempFile=$null
try{
 if($pfx){
  $before=@(Get-ChildItem Cert:\CurrentUser\My|Select-Object -ExpandProperty Thumbprint)
  $tempRoot=if($env:RUNNER_TEMP){$env:RUNNER_TEMP}else{[IO.Path]::GetTempPath()}
  $tempFile=Join-Path $tempRoot ('lumen-sign-'+[Guid]::NewGuid().ToString()+'.pfx')
  [IO.File]::WriteAllBytes($tempFile,[Convert]::FromBase64String($pfx))
  $password=ConvertTo-SecureString -String ([string]$env:LUMEN_SIGN_PFX_PASSWORD) -AsPlainText -Force
  $certificates=@(Import-PfxCertificate -FilePath $tempFile -CertStoreLocation Cert:\CurrentUser\My -Password $password)
  $imported=@($certificates|Where-Object {$_.Thumbprint -notin $before})
  $signers=@($certificates|Where-Object {$_.HasPrivateKey -and '1.3.6.1.5.5.7.3.3' -in $_.EnhancedKeyUsageList.ObjectId.Value})
  if($signers.Count -ne 1){throw 'PFX must contain exactly one Code Signing private key'}
  $CertificateThumbprint=$signers[0].Thumbprint
 }
 $CertificateThumbprint=$CertificateThumbprint.Replace(' ','')
 if($CertificateThumbprint -notmatch '^[0-9a-fA-F]{40}$'){throw 'Invalid certificate thumbprint'}
 $certificate=Get-Item -LiteralPath ('Cert:\CurrentUser\My\'+$CertificateThumbprint)
 if(-not $certificate.HasPrivateKey -or '1.3.6.1.5.5.7.3.3' -notin $certificate.EnhancedKeyUsageList.ObjectId.Value){throw 'A Code Signing certificate with a private key is required'}
 if($certificate.NotBefore -gt (Get-Date) -or $certificate.NotAfter -le (Get-Date)){throw 'Signing certificate is not currently valid'}
 $chain=[Security.Cryptography.X509Certificates.X509Chain]::new()
 try{if(-not $chain.Build($certificate)){throw 'Signing certificate chain is not trusted'}}finally{$chain.Dispose()}
 $timestamp=if($env:LUMEN_TIMESTAMP_URL){$env:LUMEN_TIMESTAMP_URL}else{'https://timestamp.digicert.com'}
 if(([Uri]$timestamp).Scheme -ne 'https'){throw 'Use an HTTPS RFC3161 timestamp URL'}
 foreach($file in $targets){
  & $signtool sign /fd SHA256 /td SHA256 /tr $timestamp /sha1 $CertificateThumbprint /s My /d Lumen $file
  if($LASTEXITCODE -ne 0){throw 'SignTool signing failed'}
  & $signtool verify /pa /all $file
  if($LASTEXITCODE -ne 0){throw 'SignTool verification failed'}
  $signature=Get-AuthenticodeSignature -LiteralPath $file
  if($signature.Status -ne 'Valid' -or $signature.SignerCertificate.Thumbprint -ne $CertificateThumbprint -or -not $signature.TimeStamperCertificate){throw 'Expected a valid, timestamped signature from the configured publisher'}
 }
}finally{
 if($tempFile -and (Test-Path -LiteralPath $tempFile)){Remove-Item -LiteralPath $tempFile -Force}
 foreach($certificate in $imported){$location='Cert:\CurrentUser\My\'+$certificate.Thumbprint;if(Test-Path -LiteralPath $location){if($certificate.HasPrivateKey){Remove-Item -LiteralPath $location -DeleteKey -Force}else{Remove-Item -LiteralPath $location -Force}}}
}
