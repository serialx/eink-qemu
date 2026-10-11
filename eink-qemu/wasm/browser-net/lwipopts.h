#ifndef X4_BROWSER_LWIPOPTS_H
#define X4_BROWSER_LWIPOPTS_H
#define NO_SYS 1
#define SYS_LIGHTWEIGHT_PROT 0
#define LWIP_SOCKET 0
#define LWIP_NETCONN 0
#define LWIP_IPV6 0
#define LWIP_DHCP 0
/* DHCP clients transmit from 0.0.0.0 before acquiring a lease. */
#define LWIP_IP_ACCEPT_UDP_PORT(port) ((port) == PP_NTOHS(67))
#define LWIP_DNS 0
#define LWIP_NETIF_HOSTNAME 0
#define LWIP_STATS 0
#define MEM_LIBC_MALLOC 1
#define MEMP_MEM_MALLOC 1
#define MEM_ALIGNMENT 4
#define TCP_MSS 1460
#define TCP_WND 32768
#define TCP_SND_BUF 32768
#define TCP_SND_QUEUELEN 128
#define MEMP_NUM_TCP_PCB 16
#define IP_SOF_BROADCAST 1
#define IP_SOF_BROADCAST_RECV 1
#define LWIP_CHECKSUM_CTRL_PER_NETIF 0
#define LWIP_TCP_KEEPALIVE 0
#endif
