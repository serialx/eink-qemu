# Read only. The native caller supplies a strictly validated adapter GUID.
function Read-EinkNetwork($guid) {
 $adapters=@(Get-NetAdapter | Where-Object { $_.InterfaceGuid.ToString() -eq $guid })
 if ($adapters.Count -ne 1) { throw 'The network adapter identity changed.' }
 $adapter=$adapters[0]; $index=$adapter.ifIndex
 # Query the whole persistent store before filtering: an empty store is valid,
 # whereas a failed query must not be mistaken for an empty configuration.
 $persistentAddresses=@(Get-NetIPAddress -PolicyStore PersistentStore | Where-Object InterfaceIndex -eq $index)
 $persistentRoutes=@(Get-NetRoute -PolicyStore PersistentStore | Where-Object InterfaceIndex -eq $index)
 $legacy=@(Get-CimInstance Win32_NetworkAdapterConfiguration | Where-Object { $_.SettingID -eq $guid })
 if ($legacy.Count -ne 1) { throw 'Missing legacy network configuration.' }
 $manualDns=@('Tcpip','Tcpip6' | ForEach-Object {
  $key=Get-ItemProperty -LiteralPath ('HKLM:\SYSTEM\CurrentControlSet\Services\'+$_+'\Parameters\Interfaces\'+$guid)
  [ordered]@{protocol=$_; nameServer=[string]$key.NameServer; domain=[string]$key.Domain}
 })
 [ordered]@{
  guid=$adapter.InterfaceGuid.ToString()
  instanceId=$adapter.PnPDeviceID
  mac=$adapter.MacAddress
  interfaces=@(Get-NetIPInterface -InterfaceIndex $index | Select-Object AddressFamily,Dhcp,RouterDiscovery,AutomaticMetric,InterfaceMetric,NlMtu,Forwarding,WeakHostSend,WeakHostReceive,Advertising,AdvertiseDefaultRoute)
  addresses=@($persistentAddresses | Select-Object IPAddress,PrefixLength,AddressFamily,PrefixOrigin,SuffixOrigin,SkipAsSource,@{Name='ValidLifetimeTicks';Expression={$_.ValidLifetime.Ticks}},@{Name='PreferredLifetimeTicks';Expression={$_.PreferredLifetime.Ticks}})
  routes=@($persistentRoutes | Select-Object DestinationPrefix,NextHop,RouteMetric,Protocol,Publish,@{Name='ValidLifetimeTicks';Expression={$_.ValidLifetime.Ticks}},@{Name='PreferredLifetimeTicks';Expression={$_.PreferredLifetime.Ticks}})
  activeAddresses=@(Get-NetIPAddress -InterfaceIndex $index | Select-Object IPAddress,PrefixLength,AddressFamily,PrefixOrigin,SuffixOrigin,SkipAsSource,AddressState)
  activeRoutes=@(Get-NetRoute -InterfaceIndex $index | Select-Object DestinationPrefix,NextHop,RouteMetric,Protocol,Publish)
  dns=@(Get-DnsClientServerAddress -InterfaceIndex $index | Select-Object AddressFamily,ServerAddresses)
  dnsClient=@(Get-DnsClient -InterfaceIndex $index | Select-Object ConnectionSpecificSuffix,RegisterThisConnectionsAddress,UseSuffixWhenRegistering)
  manualDns=$manualDns
  legacy=($legacy[0] | Select-Object TcpipNetbiosOptions,WINSEnableLMHostsLookup,WINSPrimaryServer,WINSSecondaryServer,WINSScopeID)
 }
}
