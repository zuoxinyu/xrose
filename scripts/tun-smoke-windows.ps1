# Elevated guardian for the separately launched, ordinary-user tun_smoke example.
param([Parameter(Mandatory=$true)][string]$TestHome,[Parameter(Mandatory=$true)][string]$Socket)
$ErrorActionPreference='Stop'
$root=[IO.Path]::GetFullPath($TestHome).TrimEnd('\')
$tempRoot=[IO.Path]::GetFullPath($env:TEMP).TrimEnd('\')+'\'
if(-not $root.StartsWith($tempRoot,[StringComparison]::OrdinalIgnoreCase) -or -not $Socket.StartsWith('io.xross.tun-test.')){throw 'Isolated TEMP directory and helper socket required'}
$workspace=Split-Path -Parent $PSScriptRoot
$env:XROSS_HOME=$root;$env:XROSS_HELPER_SOCKET=$Socket
$helper=$null;$preserved=$true;$reason='completed'
function Defaults { @(Get-NetRoute | Where-Object DestinationPrefix -in @('0.0.0.0/0','::/0') | ForEach-Object {"$($_.DestinationPrefix)|$($_.NextHop)|$($_.InterfaceIndex)"} | Sort-Object) -join ';' }
function Dns { @(Get-DnsClientServerAddress | Where-Object InterfaceAlias -ne 'xross-tun-smoke' | ForEach-Object {"$($_.InterfaceIndex)|$($_.AddressFamily)|$($_.ServerAddresses -join ',')"} | Sort-Object) -join ';' }
function Management { $connection=[Net.Sockets.TcpClient]::new();try{$pending=$connection.ConnectAsync('10.10.10.1',22);return $pending.Wait(1500) -and $connection.Connected}catch{return $false}finally{$connection.Dispose()} }
$defaults=Defaults;$dns=Dns
try {
 if(-not (Management)){throw 'Management channel unavailable before test'}
 $binary=Join-Path $root 'test-helper.exe'
 [IO.File]::WriteAllBytes($binary,[IO.File]::ReadAllBytes((Join-Path $workspace 'target/debug/xross-cli.exe')))
 & icacls $binary /setintegritylevel H | Out-Null
 if($LASTEXITCODE -ne 0){throw 'Could not prepare helper integrity'}
 $helper=Start-Process -FilePath $binary -ArgumentList '--helper' -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $root 'helper-error.txt')
 [pscustomobject]@{helper_pid=$helper.Id;socket=$Socket} | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $root 'ready.json')
 $deadline=[DateTime]::UtcNow.AddMinutes(3)
 while([DateTime]::UtcNow -lt $deadline -and -not (Test-Path (Join-Path $root 'finish.flag'))){
  $currentDefaults=Defaults;$currentDns=Dns;$management=Management
  if($currentDefaults -ne $defaults -or $currentDns -ne $dns -or -not $management){$preserved=$false;throw "Network baseline changed: routes=$($currentDefaults -ne $defaults), dns=$($currentDns -ne $dns), management=$management"}
  if($helper.HasExited){throw 'Test helper exited unexpectedly'}
  Start-Sleep -Milliseconds 500
 }
 if(-not (Test-Path (Join-Path $root 'finish.flag'))){throw 'Test deadline expired'}
}catch{$reason=$_.Exception.Message}
finally {
 if($helper -and -not $helper.HasExited){Stop-Process -Id $helper.Id -Force;$helper.WaitForExit(5000) | Out-Null}
 Start-Sleep -Seconds 1
 [pscustomobject]@{reason=$reason;network_preserved=$preserved -and (Defaults) -eq $defaults -and (Dns) -eq $dns -and (Management);adapter_removed=@(Get-NetAdapter -Name 'xross-tun-smoke' -ErrorAction SilentlyContinue).Count -eq 0} | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $root 'guardian-result.json')
}
