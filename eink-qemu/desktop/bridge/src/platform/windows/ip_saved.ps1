$adapter=@(Get-NetAdapter | Where-Object {$_.InterfaceGuid.ToString() -eq $guid})
if($adapter.Count -ne 1){throw 'Saved connection identity changed'}
$index=$adapter[0].ifIndex
$wins=Get-ItemProperty -LiteralPath ('HKLM:\SYSTEM\CurrentControlSet\Services\NetBT\Parameters\Interfaces\Tcpip_'+$guid)
$dhcp=@('Tcpip','Tcpip6' | ForEach-Object {
 $value=Get-ItemProperty -LiteralPath ('HKLM:\SYSTEM\CurrentControlSet\Services\'+$_+'\Parameters\Interfaces\'+$guid)
 [ordered]@{AddressFamily=$(if($_ -eq 'Tcpip'){2}else{23});Enabled=$value.EnableDHCP}
})
$result=[ordered]@{
 guid=$adapter[0].InterfaceGuid.ToString();instanceId=$adapter[0].PnPDeviceID
 interfaces=@(Get-NetIPInterface -PolicyStore PersistentStore -InterfaceIndex $index | Select-Object AddressFamily,Dhcp,RouterDiscovery,AutomaticMetric,InterfaceMetric,NlMtu,Forwarding,WeakHostSend,WeakHostReceive,Advertising,AdvertiseDefaultRoute)
 addresses=@(Get-NetIPAddress -PolicyStore PersistentStore | Where-Object InterfaceIndex -eq $index | Select-Object IPAddress,PrefixLength,AddressFamily,PrefixOrigin,SuffixOrigin,SkipAsSource,@{Name='ValidLifetimeTicks';Expression={$_.ValidLifetime.Ticks}},@{Name='PreferredLifetimeTicks';Expression={$_.PreferredLifetime.Ticks}})
 routes=@(Get-NetRoute -PolicyStore PersistentStore | Where-Object InterfaceIndex -eq $index | Select-Object DestinationPrefix,NextHop,RouteMetric,Protocol,Publish,@{Name='ValidLifetimeTicks';Expression={$_.ValidLifetime.Ticks}},@{Name='PreferredLifetimeTicks';Expression={$_.PreferredLifetime.Ticks}})
 dhcp=$dhcp;netbios=$wins.NetbiosOptions
 wins=@($wins.NameServerList | Where-Object {$null -ne $_ -and $_ -ne ''})
}
[Console]::Out.Write(($result|ConvertTo-Json -Depth 8 -Compress))
