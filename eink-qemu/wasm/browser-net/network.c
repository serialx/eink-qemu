/* In-page Ethernet endpoint. No host sockets or relay are used.
 * SPDX-License-Identifier: GPL-2.0-or-later */
#include <emscripten.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include "lwip/init.h"
#include "lwip/netif.h"
#include "lwip/etharp.h"
#include "lwip/tcp.h"
#include "lwip/udp.h"
#include "lwip/timeouts.h"
#include "netif/ethernet.h"
#include "mbedtls/ssl.h"
#include "mbedtls/entropy.h"
#include "mbedtls/ctr_drbg.h"
#include "mbedtls/x509_crt.h"
#include "mbedtls/pk.h"
#include "psa/crypto.h"
#include "certificate.h"

#define CONNECTIONS 8
#define RX_SIZE 65536
#define TX_SIZE 4096
static struct netif interface;
static struct udp_pcb *services[3];
static mbedtls_ssl_config tls_config;
static mbedtls_x509_crt certificate;
static mbedtls_pk_context key;
static mbedtls_entropy_context entropy;
static mbedtls_ctr_drbg_context random_state;
static unsigned next_id;

typedef struct Connection {
    struct tcp_pcb *pcb;
    unsigned id;
    uint32_t touched;
    int tls, ready, ending;
    size_t rx_len, tx_len;
    unsigned char rx[RX_SIZE], tx[TX_SIZE];
    mbedtls_ssl_context ssl;
} Connection;
static Connection *connections[CONNECTIONS];

u32_t sys_now(void) { return (u32_t)emscripten_get_now(); }
EM_JS(void, random_bytes, (unsigned char *out, size_t len), {
    crypto.getRandomValues(HEAPU8.subarray(out, out + len));
});
uint32_t browser_random(void)
{
    uint32_t value;
    random_bytes((unsigned char *)&value, sizeof(value));
    return value;
}
int mbedtls_hardware_poll(void *data, unsigned char *out, size_t len, size_t *olen)
{
    (void)data;
    random_bytes(out, len);
    *olen = len;
    return 0;
}
EM_JS(void, emit_frame, (const void *ptr, int len), {
    Module.onFrame(HEAPU8.slice(ptr, ptr + len));
});
EM_JS(void, emit_data, (unsigned id, const void *ptr, int len), {
    Module.onData(id, HEAPU8.slice(ptr, ptr + len));
});
EM_JS(void, emit_open, (unsigned id, int tls), { Module.onOpen(id, !!tls); });
EM_JS(void, emit_close, (unsigned id), { Module.onClose(id); });
EM_JS(void, emit_udp, (int service, const void *ptr, int len), {
    Module.onDatagram(service, HEAPU8.slice(ptr, ptr + len));
});

static Connection *lookup(unsigned id)
{
    for (unsigned i = 0; i < CONNECTIONS; i++) {
        if (connections[i] && connections[i]->id == id) return connections[i];
    }
    return NULL;
}
static void release(Connection *c)
{
    emit_close(c->id);
    for (unsigned i = 0; i < CONNECTIONS; i++) {
        if (connections[i] == c) connections[i] = NULL;
    }
    mbedtls_ssl_free(&c->ssl);
    free(c);
}
static void detach(struct tcp_pcb *pcb)
{
    tcp_arg(pcb, NULL);
    tcp_recv(pcb, NULL);
    tcp_err(pcb, NULL);
}
void net_abort(unsigned id)
{
    Connection *c = lookup(id);
    if (!c) return;
    detach(c->pcb);
    tcp_abort(c->pcb);
    release(c);
}
static void connection_error(void *arg, err_t err)
{
    (void)err;
    release(arg);
}
static int tls_send(void *arg, const unsigned char *buf, size_t len)
{
    Connection *c = arg;
    size_t n = LWIP_MIN(len, tcp_sndbuf(c->pcb));
    if (!n || tcp_write(c->pcb, buf, n, TCP_WRITE_FLAG_COPY) != ERR_OK) {
        return MBEDTLS_ERR_SSL_WANT_WRITE;
    }
    tcp_output(c->pcb);
    return n;
}
static int tls_receive(void *arg, unsigned char *buf, size_t len)
{
    Connection *c = arg;
    size_t n = LWIP_MIN(len, c->rx_len);
    if (!n) return MBEDTLS_ERR_SSL_WANT_READ;
    memcpy(buf, c->rx, n);
    c->rx_len -= n;
    memmove(c->rx, c->rx + n, c->rx_len);
    tcp_recved(c->pcb, n);
    return n;
}
static err_t receive(void *arg, struct tcp_pcb *pcb, struct pbuf *p, err_t err)
{
    Connection *c = arg;
    (void)err;
    if (!p) {
        net_abort(c->id);
        return ERR_ABRT;
    }
    c->touched = sys_now();
    if (c->tls) {
        if (p->tot_len > RX_SIZE - c->rx_len) return ERR_MEM;
        pbuf_copy_partial(p, c->rx + c->rx_len, p->tot_len, 0);
        c->rx_len += p->tot_len;
    } else {
        for (struct pbuf *q = p; q; q = q->next) emit_data(c->id, q->payload, q->len);
        tcp_recved(pcb, p->tot_len);
    }
    pbuf_free(p);
    return ERR_OK;
}
static err_t accept_connection(void *arg, struct tcp_pcb *pcb, err_t err)
{
    unsigned slot;
    (void)err;
    for (slot = 0; slot < CONNECTIONS && connections[slot]; slot++) {}
    if (slot == CONNECTIONS) { tcp_abort(pcb); return ERR_ABRT; }
    Connection *c = calloc(1, sizeof(*c));
    if (!c) { tcp_abort(pcb); return ERR_ABRT; }
    c->pcb = pcb;
    c->id = ++next_id;
    c->tls = arg != NULL;
    c->ready = !c->tls;
    c->touched = sys_now();
    mbedtls_ssl_init(&c->ssl);
    if (c->tls) {
        if (mbedtls_ssl_setup(&c->ssl, &tls_config)) {
            mbedtls_ssl_free(&c->ssl);
            free(c);
            tcp_abort(pcb);
            return ERR_ABRT;
        }
        mbedtls_ssl_set_bio(&c->ssl, c, tls_send, tls_receive, NULL);
    }
    connections[slot] = c;
    emit_open(c->id, c->tls);
    tcp_arg(pcb, c);
    tcp_recv(pcb, receive);
    tcp_err(pcb, connection_error);
    tcp_nagle_disable(pcb);
    return ERR_OK;
}
/* One bounded pending plaintext block. Retry only after it has been accepted;
 * mbedTLS requires the identical buffer when SSL_write returns WANT_WRITE. */
int net_write(unsigned id, const unsigned char *data, int len)
{
    Connection *c = lookup(id);
    if (!c || len < 0 || len > TX_SIZE) return -1;
    if (c->tx_len || !c->ready || c->ending) return 0;
    memcpy(c->tx, data, len);
    c->tx_len = len;
    c->touched = sys_now();
    return len;
}
void net_end(unsigned id)
{
    Connection *c = lookup(id);
    if (c) c->ending = 1;
}
void net_tick(void)
{
    sys_check_timeouts();
    for (unsigned i = 0; i < CONNECTIONS; i++) {
        Connection *c = connections[i];
        if (!c) continue;
        if ((uint32_t)(sys_now() - c->touched) > 120000) { net_abort(c->id); continue; }
        int ret = 0;
        if (!c->ready) {
            ret = mbedtls_ssl_handshake(&c->ssl);
            if (!ret) c->ready = 1;
            else if (ret != MBEDTLS_ERR_SSL_WANT_READ && ret != MBEDTLS_ERR_SSL_WANT_WRITE) {
                fprintf(stderr, "Browser Wi-Fi TLS handshake failed: -0x%x\n", -ret);
                net_abort(c->id);
                continue;
            }
        }
        if (c->ready && c->tls && !c->ending) {
            unsigned char data[4096];
            for (int n = 0; n < 8; n++) {
                ret = mbedtls_ssl_read(&c->ssl, data, sizeof(data));
                if (ret <= 0) break;
                emit_data(c->id, data, ret);
            }
            if (ret <= 0 && ret != MBEDTLS_ERR_SSL_WANT_READ && ret != MBEDTLS_ERR_SSL_WANT_WRITE) {
                net_abort(c->id);
                continue;
            }
        }
        if (c->tx_len) {
            if (c->tls) ret = mbedtls_ssl_write(&c->ssl, c->tx, c->tx_len);
            else ret = tls_send(c, c->tx, c->tx_len);
            if (ret > 0) {
                c->tx_len -= ret;
                memmove(c->tx, c->tx + ret, c->tx_len);
                c->touched = sys_now();
            } else if (ret != MBEDTLS_ERR_SSL_WANT_READ && ret != MBEDTLS_ERR_SSL_WANT_WRITE) {
                net_abort(c->id);
                continue;
            }
        }
        if (c->ending && !c->tx_len && !c->pcb->unsent && !c->pcb->unacked) {
            /* HTTP uses Content-Length and Connection: close. Finish TLS before
             * TCP FIN so clients can distinguish completion from truncation. */
            ret = c->tls ? mbedtls_ssl_close_notify(&c->ssl) : 0;
            if (!ret) {
                detach(c->pcb);
                if (tcp_close(c->pcb) == ERR_OK) release(c);
                else { tcp_arg(c->pcb, c); tcp_recv(c->pcb, receive); tcp_err(c->pcb, connection_error); }
            }
        }
    }
}
static err_t frame_output(struct netif *n, struct pbuf *p)
{
    unsigned char data[1518];
    (void)n;
    if (p->tot_len > sizeof(data)) return ERR_BUF;
    pbuf_copy_partial(p, data, p->tot_len, 0);
    emit_frame(data, p->tot_len);
    return ERR_OK;
}
static err_t interface_init(struct netif *n)
{
    const unsigned char mac[] = {0x52, 0x55, 0, 0x12, 0x34, 0x02};
    n->name[0] = 'b'; n->name[1] = 'r';
    n->output = etharp_output;
    n->linkoutput = frame_output;
    n->hwaddr_len = 6;
    memcpy(n->hwaddr, mac, 6);
    n->mtu = 1500;
    n->flags = NETIF_FLAG_BROADCAST | NETIF_FLAG_ETHARP | NETIF_FLAG_LINK_UP;
    return ERR_OK;
}
void net_input(const unsigned char *data, int len)
{
    if (len < 14 || len > 2048) return;
    struct pbuf *p = pbuf_alloc(PBUF_RAW, len, PBUF_RAM);
    if (p) {
        pbuf_take(p, data, len);
        if (interface.input(p, &interface) != ERR_OK) pbuf_free(p);
    }
}
static void datagram(void *arg, struct udp_pcb *pcb, struct pbuf *p,
                     const ip_addr_t *address, u16_t port)
{
    unsigned char data[1500];
    /* JS gets the service index and payload, and returns the response
     * synchronously through Module.udpReply. DHCP broadcasts. */
    int service = (int)(uintptr_t)arg;
    if (p->tot_len <= sizeof(data)) {
        pbuf_copy_partial(p, data, p->tot_len, 0);
        emit_udp(service, data, p->tot_len);
        int len = EM_ASM_INT({
            const data = Module.udpReply;
            if (!data || data.length > 1500) return 0;
            HEAPU8.set(data, $0);
            return data.length;
        }, data);
        if (len) {
            struct pbuf *reply = pbuf_alloc(PBUF_TRANSPORT, len, PBUF_RAM);
            if (reply) {
                pbuf_take(reply, data, len);
                udp_sendto(pcb, reply, service == 0 ? IP_ADDR_BROADCAST : address, service == 0 ? 68 : port);
                pbuf_free(reply);
            }
        }
    }
    pbuf_free(p);
}
int net_init(void)
{
    if (psa_crypto_init() != PSA_SUCCESS) return -1;
    mbedtls_entropy_init(&entropy);
    mbedtls_ctr_drbg_init(&random_state);
    mbedtls_ssl_config_init(&tls_config);
    mbedtls_x509_crt_init(&certificate);
    mbedtls_pk_init(&key);
    if (mbedtls_ctr_drbg_seed(&random_state, mbedtls_entropy_func, &entropy, NULL, 0) ||
        mbedtls_x509_crt_parse(&certificate, (const unsigned char *)local_cert, sizeof(local_cert)) ||
        mbedtls_pk_parse_key(&key, (const unsigned char *)local_key, sizeof(local_key), NULL, 0,
                             mbedtls_ctr_drbg_random, &random_state) ||
        mbedtls_ssl_config_defaults(&tls_config, MBEDTLS_SSL_IS_SERVER, MBEDTLS_SSL_TRANSPORT_STREAM,
                                    MBEDTLS_SSL_PRESET_DEFAULT) ||
        mbedtls_ssl_conf_own_cert(&tls_config, &certificate, &key)) return -1;
    mbedtls_ssl_conf_rng(&tls_config, mbedtls_ctr_drbg_random, &random_state);
    mbedtls_ssl_conf_authmode(&tls_config, MBEDTLS_SSL_VERIFY_NONE);
    lwip_init();
    ip4_addr_t ip, mask, gateway;
    IP4_ADDR(&ip, 10, 0, 2, 2);
    IP4_ADDR(&mask, 255, 255, 255, 0);
    IP4_ADDR(&gateway, 0, 0, 0, 0);
    if (!netif_add(&interface, &ip, &mask, &gateway, NULL, interface_init, ethernet_input)) return -1;
    netif_set_default(&interface);
    netif_set_up(&interface);
    for (int tls = 0; tls < 2; tls++) {
        struct tcp_pcb *pcb = tcp_new();
        if (!pcb || tcp_bind(pcb, IP_ANY_TYPE, tls ? 443 : 80) != ERR_OK) return -1;
        pcb = tcp_listen(pcb);
        if (!pcb) return -1;
        tcp_arg(pcb, (void *)(uintptr_t)tls);
        tcp_accept(pcb, accept_connection);
    }
    const int ports[] = {67, 53, 123};
    for (int i = 0; i < 3; i++) {
        services[i] = udp_new();
        if (!services[i] || udp_bind(services[i], IP_ANY_TYPE, ports[i]) != ERR_OK) return -1;
        ip_set_option(services[i], SOF_BROADCAST);
        udp_recv(services[i], datagram, (void *)(uintptr_t)i);
    }
    return 0;
}
