# Read-only: resolve DNS, inspect routes, and time ICMP/TCP connections.
# No config files, credentials, routes, MTU or firewall settings are modified.
[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$Server,
    [ValidateRange(1,65535)][int]$ServerPort = 443,
    [Parameter(Mandatory)][System.Net.IPAddress]$TargetIP,
    [ValidateRange(1,65535)][int]$TargetPort = 443,
    [string]$TargetName,
    [ValidateRange(1,10)][int]$Count = 5
)
$ErrorActionPreference = 'Stop'
function Resolve-IPv4([string]$DnsName) {
    $watch = [Diagnostics.Stopwatch]::StartNew()
    $query = [Net.Dns]::GetHostAddressesAsync($DnsName)
    if (-not $query.Wait(3000)) { throw "DNS timeout: $DnsName" }
    $addresses = @($query.Result | Where-Object AddressFamily -eq InterNetwork)
    if (-not $addresses.Count) { throw "No IPv4 address: $DnsName" }
    Write-Host ("DNS {0}: {1:N1} ms; IPv4: {2}" -f $DnsName,$watch.Elapsed.TotalMilliseconds,($addresses -join ', '))
    return $addresses[0]
}
function Test-Endpoint([string]$Label, [Net.IPAddress]$Address, [int]$Port) {
    Write-Host "=== $Label $Address`:$Port ==="
    if (Get-Command Find-NetRoute -ErrorAction SilentlyContinue) {
        Find-NetRoute -RemoteIPAddress $Address.IPAddressToString |
            Select-Object InterfaceAlias,InterfaceIndex,IPAddress,DestinationPrefix,NextHop |
            Format-Table -AutoSize | Out-Host
    }
    $results = for ($index = 0; $index -lt $Count; $index++) {
        $ping = [Net.NetworkInformation.Ping]::new()
        try {
            $reply = $ping.Send($Address, 1500)
            $icmp = if ($reply.Status -eq 'Success') { "$($reply.RoundtripTime) ms" } else { "$($reply.Status)" }
        } catch { $icmp = 'Unavailable' } finally { $ping.Dispose() }
        $client = [Net.Sockets.TcpClient]::new()
        $watch = [Diagnostics.Stopwatch]::StartNew()
        try {
            $connecting = $client.ConnectAsync($Address, $Port)
            if (-not $connecting.Wait(2000)) { throw 'timeout' }
            $tcp = '{0:N1} ms' -f $watch.Elapsed.TotalMilliseconds
        } catch { $tcp = 'Failed/timeout (verify port and firewall)' } finally { $client.Dispose() }
        [pscustomobject]@{ Sample=$index+1; ICMP=$icmp; TCP=$tcp }
    }
    $results | Format-Table -AutoSize | Out-Host
}
if ($TargetIP.AddressFamily -ne 'InterNetwork') { throw 'Use an IPv4 TargetIP for this diagnostic.' }
$serverAddress = Resolve-IPv4 $Server
Test-Endpoint 'Outer server / CDN edge' $serverAddress $ServerPort
Test-Endpoint 'Internal service over current route' $TargetIP $TargetPort
if ($TargetName) {
    $resolvedTarget = Resolve-IPv4 $TargetName
    Write-Host "Compare internal DNS result $resolvedTarget with the expected IP $TargetIP."
}
Get-NetIPInterface -AddressFamily IPv4 |
    Where-Object ConnectionState -eq Connected |
    Select-Object InterfaceAlias,NlMtu,InterfaceMetric | Format-Table -AutoSize | Out-Host
Write-Host 'ICMP may be blocked. TCP measures connection establishment only, not application response time.'
