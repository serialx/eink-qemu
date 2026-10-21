function Native-Adapter {
 $rows=@(Get-NetAdapter | Where-Object {$_.InterfaceGuid.ToString() -eq $guid -and $_.PnPDeviceID -eq $before.instanceId})
 if($rows.Count -ne 1){exit 73}
 $rows[0]
}
function Address-Matches($actual,$wanted,$persistent){
 if(([Net.IPAddress]::Parse(($actual.IPAddress -split '%')[0])).ToString() -ne $wanted.IPAddress -or $actual.PrefixLength -ne $wanted.PrefixLength -or $actual.SkipAsSource -ne $wanted.SkipAsSource -or $actual.PrefixOrigin -ne 1 -or $actual.SuffixOrigin -ne 1){return $false}
 if($persistent -and ($actual.ValidLifetime.Ticks -ne $wanted.ValidLifetimeTicks -or $actual.PreferredLifetime.Ticks -ne $wanted.PreferredLifetimeTicks)){return $false}
 return $true
}
function Route-Matches($actual,$wanted,$persistent){
 if($actual.DestinationPrefix -ne $wanted.DestinationPrefix -or $actual.NextHop -ne $wanted.NextHop -or $actual.RouteMetric -ne $wanted.RouteMetric -or $actual.Protocol -ne $wanted.Protocol -or $actual.Publish -ne $wanted.Publish){return $false}
 if($persistent -and ($actual.ValidLifetime.Ticks -ne $wanted.ValidLifetimeTicks -or $actual.PreferredLifetime.Ticks -ne $wanted.PreferredLifetimeTicks)){return $false}
 return $true
}
function Check-Saved($index){
 foreach($actual in @(Get-NetIPAddress -InterfaceIndex $index | Where-Object PrefixOrigin -eq 1)){
  $matches=@($desired.addresses | Where-Object {Address-Matches $actual $_ $false})
  if($matches.Count -ne 1){exit 73}
 }
 foreach($actual in @(Get-NetIPAddress -PolicyStore PersistentStore | Where-Object InterfaceIndex -eq $index)){
  $matches=@($desired.addresses | Where-Object {Address-Matches $actual $_ $true})
  if($matches.Count -ne 1){exit 73}
 }
 foreach($actual in @(Get-NetRoute -PolicyStore PersistentStore | Where-Object InterfaceIndex -eq $index)){
  $matches=@($desired.routes | Where-Object {Route-Matches $actual $_ $true})
  if($matches.Count -ne 1){exit 73}
 }
}
$adapter=Native-Adapter
Check-Saved $adapter.ifIndex
foreach($family in @($desired.addresses | ForEach-Object AddressFamily | Select-Object -Unique)){
 $adapter=Native-Adapter;Check-Saved $adapter.ifIndex
 # A disconnected interface may silently ignore DHCP changes. Verify the
 # native state before attempting to create a persistent address.
 Set-NetIPInterface -InterfaceIndex $adapter.ifIndex -AddressFamily $family -Dhcp Disabled -Confirm:$false
 if([uint32](Get-NetIPInterface -InterfaceIndex $adapter.ifIndex -AddressFamily $family).Dhcp -ne 0){throw 'The destination must be connected before static configuration'}
}
foreach($wanted in $desired.addresses){
 $adapter=Native-Adapter;Check-Saved $adapter.ifIndex
 foreach($store in @('PersistentStore','ActiveStore')){
  $rows=@(Get-NetIPAddress -PolicyStore $store | Where-Object {$_.InterfaceIndex -eq $adapter.ifIndex -and $_.IPAddress -eq $wanted.IPAddress})
  if($rows.Count -gt 1){exit 73}
  if($rows.Count -eq 1){if(!(Address-Matches $rows[0] $wanted ($store -eq 'PersistentStore'))){exit 73};continue}
  $parameters=@{InterfaceIndex=$adapter.ifIndex;IPAddress=$wanted.IPAddress;PrefixLength=[byte]$wanted.PrefixLength;SkipAsSource=[bool]$wanted.SkipAsSource;ValidLifetime=[TimeSpan]::FromTicks([long]$wanted.ValidLifetimeTicks);PreferredLifetime=[TimeSpan]::FromTicks([long]$wanted.PreferredLifetimeTicks);Confirm=$false}
  if($store -eq 'ActiveStore'){$parameters.PolicyStore='ActiveStore'}
  New-NetIPAddress @parameters | Out-Null
 }
}
foreach($wanted in $desired.routes){
 $adapter=Native-Adapter;Check-Saved $adapter.ifIndex
 foreach($store in @('PersistentStore','ActiveStore')){
  $rows=@(Get-NetRoute -PolicyStore $store | Where-Object {$_.InterfaceIndex -eq $adapter.ifIndex -and $_.DestinationPrefix -eq $wanted.DestinationPrefix -and $_.NextHop -eq $wanted.NextHop})
  if($rows.Count -gt 1){exit 73}
  if($rows.Count -eq 1){if(!(Route-Matches $rows[0] $wanted ($store -eq 'PersistentStore'))){exit 73};continue}
  $parameters=@{InterfaceIndex=$adapter.ifIndex;DestinationPrefix=$wanted.DestinationPrefix;NextHop=$wanted.NextHop;RouteMetric=[uint16]$wanted.RouteMetric;Protocol=[uint16]$wanted.Protocol;Publish=[byte]$wanted.Publish;ValidLifetime=[TimeSpan]::FromTicks([long]$wanted.ValidLifetimeTicks);PreferredLifetime=[TimeSpan]::FromTicks([long]$wanted.PreferredLifetimeTicks);Confirm=$false}
  if($store -eq 'ActiveStore'){$parameters.PolicyStore='ActiveStore'}
  New-NetRoute @parameters | Out-Null
 }
}
