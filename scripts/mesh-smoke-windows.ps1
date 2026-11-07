# Run from an elevated PowerShell. This guardian is deliberately outside the
# helper Job Object, so it can recover the test after helper/worker crashes.
param(
    [Parameter(Mandatory=$true)][string]$TestHome,
    [Parameter(Mandatory=$true)][string]$ProfileIds,
    [ValidateRange(15,900)][int]$Seconds=180,
    [string]$ProbeAddresses='',
    [ValidateRange(0,65535)][int]$ProbeTcpPort=0,
    [string]$DnsName='',
    [switch]$CrashOwner,
    [ValidateRange(0,300)][int]$CrashDelaySeconds=40,
    [string]$Gateway='10.10.10.1'
)
$ErrorActionPreference='Stop'
[Console]::OutputEncoding=[Text.UTF8Encoding]::new($false)
$OutputEncoding=[Console]::OutputEncoding
$principal=[Security.Principal.WindowsPrincipal]::new([Security.Principal.WindowsIdentity]::GetCurrent())
if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) { throw 'Windows UAC elevation is required for this test.' }
$root=[IO.Path]::GetFullPath($TestHome).TrimEnd('\')
$tempRoot=[IO.Path]::GetFullPath($env:TEMP).TrimEnd('\')+'\'
if (-not $root.StartsWith($tempRoot,[StringComparison]::OrdinalIgnoreCase)) { throw 'TestHome must be a throwaway directory below TEMP.' }
$ids=@($ProfileIds.Split(',') | Where-Object {$_})
foreach($id in $ids) { $parsed=[Guid]::Empty; if (-not [Guid]::TryParse($id,[ref]$parsed)) { throw 'Invalid profile ID' } }
$source=Join-Path (Split-Path -Parent $PSScriptRoot) 'target\debug\xrose-cli.exe'
$binDir=Join-Path $root 'test-bin'
New-Item -ItemType Directory -Path $binDir -Force | Out-Null
$cli=Join-Path $binDir 'xrose-cli.exe'
Copy-Item -LiteralPath $source -Destination $cli -Force
$env:XROSE_HOME=$root
$env:XROSE_HELPER_SOCKET='io.xrose.mesh-test.'+[Guid]::NewGuid().ToString('N')
$env:XROSE_HELPER_SOCKET | Set-Content -LiteralPath (Join-Path $root 'test-socket.txt')
$helper=$null; $session=$null
$connected=@{}; $probes=@{}; $dnsVerified=[string]::IsNullOrEmpty($DnsName); $connectedAt=$null
$targets=@($ProbeAddresses.Split(',') | Where-Object {$_})
foreach($target in $targets) { $address=$null; if(-not [Net.IPAddress]::TryParse($target,[ref]$address)){throw 'Invalid probe IP'} }
$result=[ordered]@{completed=$false;reason='starting';profiles=$ids;connected_profiles=@();reachable_addresses=@();magic_dns_verified=$false;owner_crash_cleanup=$false;default_route_preserved=$false;dns_preserved=$false}
function DefaultRoutes {
    @(Get-NetRoute | Where-Object {$_.DestinationPrefix -in @('0.0.0.0/0','::/0')} | ForEach-Object {"$($_.DestinationPrefix)|$($_.NextHop)|$($_.InterfaceIndex)"} | Sort-Object) -join ';'
}
function OriginalDns {
    @(Get-DnsClientServerAddress | Where-Object {$_.InterfaceAlias -notlike 'xrose-mesh-*'} | ForEach-Object {"$($_.InterfaceIndex)|$($_.AddressFamily)|$($_.ServerAddresses -join ',')"} | Sort-Object) -join ';'
}
function ManagementReachable {
    $client=New-Object Net.Sockets.TcpClient
    try { $pending=$client.ConnectAsync($Gateway,22); return ($pending.Wait(2500) -and $client.Connected) } catch { return $false } finally {$client.Dispose()}
}
$routes=DefaultRoutes; $dns=OriginalDns
if (-not (ManagementReachable)) { throw 'OpenWrt management channel is not reachable before the test.' }
Invoke-WebRequest -UseBasicParsing -Uri 'https://www.gstatic.com/generate_204' -TimeoutSec 8 | Out-Null
try {
    $helper=Start-Process -FilePath $cli -ArgumentList '--helper' -WindowStyle Hidden -PassThru
    Start-Sleep -Seconds 2
    if ($helper.HasExited) { throw 'Temporary helper failed to start.' }
    $arguments=@('mesh','connect')+$ids+@('--seconds',$Seconds.ToString())
    $session=Start-Process -FilePath $cli -ArgumentList $arguments -WindowStyle Hidden -PassThru -RedirectStandardOutput (Join-Path $root 'smoke.stdout.txt') -RedirectStandardError (Join-Path $root 'smoke.stderr.txt')
    $null=$session.Handle
    $deadline=[DateTime]::UtcNow.AddSeconds($Seconds+15)
    $failures=0
    while(-not $session.HasExited -and [DateTime]::UtcNow -lt $deadline) {
        Start-Sleep -Seconds 2
        if ((DefaultRoutes) -ne $routes -or (OriginalDns) -ne $dns -or -not (ManagementReachable)) { throw 'Network baseline changed; test stopped immediately.' }
        try { $response=Invoke-WebRequest -UseBasicParsing -Uri 'https://www.gstatic.com/generate_204' -TimeoutSec 5; $failures=0 } catch { $failures++ }
        if ($failures -ge 2) { throw 'Original internet connectivity check failed twice; test stopped.' }
        $status=& $cli mesh status
        $status | Set-Content -LiteralPath (Join-Path $root 'smoke-status.json') -Encoding UTF8
        if($LASTEXITCODE -ne 0){throw 'Temporary helper became unavailable.'}
        $snapshot=$status | ConvertFrom-Json
        foreach($network in $snapshot.networks){if($network.phase -eq 'connected'){$connected[$network.profile_id]=$true}}
        if($connected.Count -eq $ids.Count){
            if($null -eq $connectedAt){$connectedAt=[DateTime]::UtcNow}
            foreach($target in $targets){
                if(-not $probes.ContainsKey($target)){
                    $ping=New-Object Net.NetworkInformation.Ping
                    try { $reply=$ping.Send($target,1500); if($reply.Status -eq 'Success'){$probes[$target]=$true} } finally {$ping.Dispose()}
                    if(-not $probes.ContainsKey($target) -and $ProbeTcpPort -gt 0){
                        $client=New-Object Net.Sockets.TcpClient
                        try {$pending=$client.ConnectAsync($target,$ProbeTcpPort); if($pending.Wait(2000) -and $client.Connected){$probes[$target]=$true}} catch {} finally {$client.Dispose()}
                    }
                }
            }
            if(-not $dnsVerified){
                try {$answers=Resolve-DnsName -Name $DnsName -Type A -DnsOnly -ErrorAction Stop; $dnsVerified=@($answers | Where-Object {$targets -contains $_.IPAddress}).Count -gt 0} catch {}
            }
            if($CrashOwner -and $probes.Count -eq $targets.Count -and $dnsVerified -and ([DateTime]::UtcNow-$connectedAt).TotalSeconds -ge $CrashDelaySeconds){
                Stop-Process -Id $session.Id -Force
                Start-Sleep -Seconds 7
                $after=(& $cli mesh status | ConvertFrom-Json).networks
                $result.owner_crash_cleanup=@($after | Where-Object {$ids -contains $_.profile_id -and $_.phase -notin @('disconnected','failed')}).Count -eq 0
                $names=@($ids | ForEach-Object {"xrose-mesh-et-$($_.Substring(0,8))"; "xrose-mesh-ts-$($_.Substring(0,8))"})
                $markers=@($ids | ForEach-Object {"Xrose mesh $_"})
                $result.owner_crash_cleanup=$result.owner_crash_cleanup -and @(Get-NetAdapter | Where-Object {$names -contains $_.Name}).Count -eq 0 -and @(Get-DnsClientNrptRule | Where-Object {$markers -contains $_.DisplayName}).Count -eq 0
                break
            }
        }
    }
    if($session.HasExited){$session.WaitForExit()}
    $result.completed=($result.owner_crash_cleanup -or ($session.HasExited -and $session.ExitCode -eq 0)) -and $connected.Count -eq $ids.Count -and $probes.Count -eq $targets.Count -and $dnsVerified
    $result.session_exit_code=$session.ExitCode
    $result.reason=if($result.completed){'bounded session ended'}else{'session failed or timed out'}
} catch { $result.reason=if($_.Exception.Message.Length -le 256){$_.Exception.Message}else{'Test report parsing failed; inspect the isolated status file.'} }
finally {
    # Only terminate processes created by this invocation.
    if($session -and -not $session.HasExited){Stop-Process -Id $session.Id -Force -ErrorAction SilentlyContinue}
    if($helper -and -not $helper.HasExited){Stop-Process -Id $helper.Id -Force -ErrorAction SilentlyContinue}
    Start-Sleep -Seconds 2
    foreach($id in $ids) {
        Get-DnsClientNrptRule | Where-Object {$_.DisplayName -eq "Xrose mesh $id"} | ForEach-Object {Remove-DnsClientNrptRule -Name $_.Name -Force}
    }
    $result.default_route_preserved=(DefaultRoutes) -eq $routes
    $result.dns_preserved=(OriginalDns) -eq $dns
    $result.connected_profiles=@($connected.Keys)
    $result.reachable_addresses=@($probes.Keys)
    $result.magic_dns_verified=$dnsVerified -and -not [string]::IsNullOrEmpty($DnsName)
    $result | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $root 'smoke-result.json') -Encoding UTF8
}
