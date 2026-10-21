# Only fixed property names become cmdlet parameters. All snapshot strings are
# parsed JSON values, never executable PowerShell or caller-provided filters.
function Equal-Value($a,$b) {
 (ConvertTo-Json -InputObject $a -Depth 5 -Compress) -ceq (ConvertTo-Json -InputObject $b -Depth 5 -Compress)
}
function Require-Allowed($value,$choices) {
 foreach($choice in $choices){if(Equal-Value $value $choice){return}}
 exit 73
}
function Native-Adapter {
 $rows=@(Get-NetAdapter | Where-Object {$_.InterfaceGuid.ToString() -eq $guid})
 if($rows.Count -ne 1 -or $rows[0].PnPDeviceID -ne $before.instanceId){exit 73}
 $rows[0]
}
function Family-Row($rows,$family) {
 $row=@($rows | Where-Object AddressFamily -eq $family)
 if($row.Count -ne 1){throw 'Incomplete IP interface configuration'}
 $row[0]
}
function Normalize-Servers($value) {
 $servers=@(([string]$value -split '[\s,;]+') | Where-Object {$_ -ne ''} | ForEach-Object {([Net.IPAddress]::Parse($_)).ToString()})
 $servers -join ','
}
$current=Read-EinkNetwork $guid
foreach($family in @(2,23)) {
 $saved=Family-Row $current.configuredDhcp $family
 Require-Allowed $saved.Enabled @((Family-Row $before.configuredDhcp $family).Enabled,(Family-Row $desired.configuredDhcp $family).Enabled)
}
$interfaceFields=@('RouterDiscovery','AutomaticMetric','NlMtu','Forwarding','WeakHostSend','WeakHostReceive','Advertising','AdvertiseDefaultRoute')
foreach($family in @(2,23)) {
 $old=Family-Row $before.interfaces $family;$oldSaved=Family-Row $before.persistentInterfaces $family
 $next=Family-Row $desired.interfaces $family;$nextSaved=Family-Row $desired.persistentInterfaces $family
 # A persistent override must match its own saved state. Only active values
 # can temporarily take a persistent value while a two-store update completes.
 $saved=Family-Row $current.persistentInterfaces $family
 foreach($field in @($interfaceFields)+@('Dhcp')){Require-Allowed $saved.$field @($oldSaved.$field,$nextSaved.$field)}
 Require-Allowed $saved.InterfaceMetric @($oldSaved.InterfaceMetric,$nextSaved.InterfaceMetric)
 foreach($row in @((Family-Row $current.interfaces $family),(Family-Row $current.persistentInterfaces $family))) {
  foreach($field in $interfaceFields){Require-Allowed $row.$field @($old.$field,$oldSaved.$field,$next.$field,$nextSaved.$field)}
  if($row.AutomaticMetric -eq 0){Require-Allowed $row.InterfaceMetric @($old.InterfaceMetric,$oldSaved.InterfaceMetric,$next.InterfaceMetric,$nextSaved.InterfaceMetric)}
 }
}
foreach($protocol in @('Tcpip','Tcpip6')) {
 $old=@($before.manualDns | Where-Object protocol -eq $protocol)[0]
 $next=@($desired.manualDns | Where-Object protocol -eq $protocol)[0]
 $actual=@($current.manualDns | Where-Object protocol -eq $protocol)[0]
 Require-Allowed (Normalize-Servers $actual.nameServer) @($old.nameServer,$next.nameServer)
 Require-Allowed ([string]$actual.domain) @([string]$old.domain,[string]$next.domain)
}
foreach($field in @('RegisterThisConnectionsAddress','UseSuffixWhenRegistering')) {
 Require-Allowed $current.dnsClient[0].$field @($before.dnsClient[0].$field,$desired.dnsClient[0].$field)
}
foreach($field in @('TcpipNetbiosOptions','WINSEnableLMHostsLookup','WINSScopeID')) {
 Require-Allowed $current.legacy.$field @($before.legacy.$field,$desired.legacy.$field)
}
if(!(Equal-Value @($current.legacy.ManualServers) @($before.legacy.ManualServers)) -and !(Equal-Value @($current.legacy.ManualServers) @($desired.legacy.ManualServers))){exit 73}

# DHCP persistence is stored in the protocol's interface registry key even
# when CIM PersistentStore.Dhcp is null. Use the documented cmdlet, and retain
# the registry value as independent restoration evidence; never write the key.
foreach($family in @(2,23)) {
 $adapter=Native-Adapter
 $protocol=if($family -eq 2){'Tcpip'}else{'Tcpip6'}
 $saved=Get-ItemProperty -LiteralPath ('HKLM:\SYSTEM\CurrentControlSet\Services\'+$protocol+'\Parameters\Interfaces\'+$guid)
 $old=(Family-Row $before.configuredDhcp $family).Enabled;$next=(Family-Row $desired.configuredDhcp $family).Enabled
 Require-Allowed $saved.EnableDHCP @($old,$next)
 if($saved.EnableDHCP -ne $next){Set-NetIPInterface -InterfaceIndex $adapter.ifIndex -AddressFamily $family -Dhcp ([uint32]$next) -Confirm:$false}
}
# Omit PolicyStore when setting persistent configuration so Windows updates
# both stores. Then restore any distinct ActiveStore values independently.
foreach($persistent in @($true,$false)) {
 foreach($family in @(2,23)) {
  $adapter=Native-Adapter
  $store=if($persistent){'PersistentStore'}else{'ActiveStore'}
  $row=Get-NetIPInterface -InterfaceIndex $adapter.ifIndex -AddressFamily $family -PolicyStore $store
  $next=if($persistent){Family-Row $desired.persistentInterfaces $family}else{Family-Row $desired.interfaces $family}
  $oldActive=Family-Row $before.interfaces $family;$oldSaved=Family-Row $before.persistentInterfaces $family
  $nextActive=Family-Row $desired.interfaces $family;$nextSaved=Family-Row $desired.persistentInterfaces $family
  if($persistent){
   foreach($field in @($interfaceFields)+@('Dhcp')){Require-Allowed $row.$field @($oldSaved.$field,$nextSaved.$field)}
   Require-Allowed $row.InterfaceMetric @($oldSaved.InterfaceMetric,$nextSaved.InterfaceMetric)
  }
  foreach($field in $interfaceFields){Require-Allowed $row.$field @($oldActive.$field,$oldSaved.$field,$nextActive.$field,$nextSaved.$field)}
  if($row.AutomaticMetric -eq 0){Require-Allowed $row.InterfaceMetric @($oldActive.InterfaceMetric,$oldSaved.InterfaceMetric,$nextActive.InterfaceMetric,$nextSaved.InterfaceMetric)}
  $parameters=@{InterfaceIndex=$adapter.ifIndex;AddressFamily=$family;Confirm=$false}
  if(!$persistent){$parameters.PolicyStore='ActiveStore'}
  $changed=$false
  $fields=if($persistent){@($interfaceFields)+@('Dhcp')}else{$interfaceFields}
  foreach($field in $fields) {
   if($row.$field -ne $next.$field) {
    if($null -eq $next.$field){throw 'Cannot erase a saved interface override'}
    $parameter=if($field -eq 'NlMtu'){'NlMtuBytes'}else{$field}
    $parameters[$parameter]=[uint32]$next.$field;$changed=$true
   }
  }
  if($next.AutomaticMetric -eq 0 -and $row.InterfaceMetric -ne $next.InterfaceMetric){$parameters.InterfaceMetric=[uint32]$next.InterfaceMetric;$changed=$true}
  if($changed){Set-NetIPInterface @parameters}
 }
}
foreach($family in @(2,23)) {
 $adapter=Native-Adapter
 $protocol=if($family -eq 2){'Tcpip'}else{'Tcpip6'}
 $next=@($desired.manualDns | Where-Object protocol -eq $protocol)[0]
 $old=@($before.manualDns | Where-Object protocol -eq $protocol)[0]
 $actual=Get-ItemProperty -LiteralPath ('HKLM:\SYSTEM\CurrentControlSet\Services\'+$protocol+'\Parameters\Interfaces\'+$guid)
 Require-Allowed (Normalize-Servers $actual.NameServer) @($old.nameServer,$next.nameServer)
 if((Normalize-Servers $actual.NameServer) -ne $next.nameServer) {
  $row=@(Get-DnsClientServerAddress -InterfaceIndex $adapter.ifIndex -AddressFamily $family)
  if($row.Count -ne 1){throw 'Incomplete DNS client identity'}
  if($next.nameServer -eq ''){Set-DnsClientServerAddress -InputObject $row[0] -ResetServerAddresses -Confirm:$false}
  else{Set-DnsClientServerAddress -InputObject $row[0] -ServerAddresses @($next.nameServer -split ',') -Confirm:$false}
 }
}
$adapter=Native-Adapter
$nextDomain=[string](@($desired.manualDns | Where-Object protocol -eq 'Tcpip')[0].domain)
$oldDomain=[string](Get-ItemProperty -LiteralPath ('HKLM:\SYSTEM\CurrentControlSet\Services\Tcpip\Parameters\Interfaces\'+$guid)).Domain
Require-Allowed $oldDomain @([string](@($before.manualDns | Where-Object protocol -eq 'Tcpip')[0].domain),$nextDomain)
$dnsClient=Get-DnsClient -InterfaceIndex $adapter.ifIndex
$dnsParameters=@{InterfaceIndex=$adapter.ifIndex;Confirm=$false}
$changed=$false
if($nextDomain -cne $oldDomain){$dnsParameters.ConnectionSpecificSuffix=$nextDomain;$changed=$true}
foreach($field in @('RegisterThisConnectionsAddress','UseSuffixWhenRegistering')){
 Require-Allowed $dnsClient.$field @($before.dnsClient[0].$field,$desired.dnsClient[0].$field)
 if($dnsClient.$field -ne $desired.dnsClient[0].$field){$dnsParameters[$field]=[bool]$desired.dnsClient[0].$field;$changed=$true}
}
if($changed){Set-DnsClient @dnsParameters}
$adapter=Native-Adapter
$legacy=@(Get-CimInstance Win32_NetworkAdapterConfiguration | Where-Object SettingID -eq $guid)
if($legacy.Count -ne 1){throw 'Legacy configuration identity changed'}
Require-Allowed $legacy[0].TcpipNetbiosOptions @($before.legacy.TcpipNetbiosOptions,$desired.legacy.TcpipNetbiosOptions)
if($legacy[0].TcpipNetbiosOptions -ne $desired.legacy.TcpipNetbiosOptions){
 if($null -eq $desired.legacy.TcpipNetbiosOptions){throw 'Cannot restore unavailable NetBIOS configuration'}
 $result=Invoke-CimMethod -InputObject $legacy[0] -MethodName SetTcpipNetbios -Arguments @{TcpipNetbiosOptions=[uint32]$desired.legacy.TcpipNetbiosOptions}
 if($result.ReturnValue -ne 0){throw 'NetBIOS configuration requires recovery or restart'}
}
$wins=Get-ItemProperty -LiteralPath ('HKLM:\SYSTEM\CurrentControlSet\Services\NetBT\Parameters\Interfaces\Tcpip_'+$guid)
$currentServers=@($wins.NameServerList | Where-Object {$null -ne $_ -and $_ -ne ''})
if(!(Equal-Value $currentServers @($before.legacy.ManualServers)) -and !(Equal-Value $currentServers @($desired.legacy.ManualServers))){exit 73}
if(!(Equal-Value $currentServers @($desired.legacy.ManualServers))){
 $servers=@($desired.legacy.ManualServers)
 if($servers.Count -gt 2){throw 'WINS server configuration needs an extended native setter'}
 $arguments=@{WINSPrimaryServer='';WINSSecondaryServer=''}
 if($servers.Count -gt 0){$arguments.WINSPrimaryServer=[string]$servers[0]}
 if($servers.Count -gt 1){$arguments.WINSSecondaryServer=[string]$servers[1]}
 $result=Invoke-CimMethod -InputObject $legacy[0] -MethodName SetWINSServer -Arguments $arguments
 if($result.ReturnValue -ne 0){throw 'WINS configuration requires recovery or restart'}
}
