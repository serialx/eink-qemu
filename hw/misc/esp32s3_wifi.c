#include "qemu/osdep.h"
#include "qemu/module.h"
#include "qemu/timer.h"
#include "qemu/log.h"
#include "qemu/error-report.h"
#include "qemu/guest-random.h"
#include "qapi/error.h"
#include "hw/core/irq.h"
#include "hw/core/sysbus.h"
#include "hw/misc/esp32s3_wifi.h"
#include "system/address-spaces.h"
#include "esp32_wlan_packet.h"
#include "hw/core/qdev-properties.h"

#define DEBUG 0

#if DEBUG

static const char * get_reg_name(hwaddr addr){
    switch (addr)
    {
        case A_S3_WIFI_DMA_IN_STATUS:
           return "S3_WIFI_DMA_IN_STATUS";
           break;
        case A_S3_WIFI_DMA_INLINK:
           return "S3_WIFI_DMA_INLINK";
           break;
        case A_S3_WIFI_NEXT_RX_DSCR:
           return "S3_WIFI_NEXT_RX_DSCR";
           break;
        case A_S3_WIFI_LAST_RX_DSCR:
           return "S3_WIFI_LAST_RX_DSCR";
           break;
        case A_S3_WIFI_DMA_INT_STATUS:
           return "S3_WIFI_DMA_INT_STATUS";
           break;
        case A_S3_WIFI_DMA_INT_CLR:
           return "S3_WIFI_DMA_INT_CLR";
           break;
        case A_S3_WIFI_STATUS:
           return "S3_WIFI_STATUS";
           break;
        case A_S3_WIFI_DMA_OUTLINK:
           return "S3_WIFI_DMA_OUTLINK";
           break;
        case A_S3_WIFI_DMA_OUT_STATUS:
           return "S3_WIFI_DMA_OUT_STATUS";
           break;
        case A_S3_WIFI_TX_CONFIG:
           return "S3_WIFI_TX_CONFIG";
           break;
        case A_S3_WIFI_TX_CLR:
           return "S3_WIFI_TX_CLR";
           break;
        case A_S3_WIFI_TX_DURATION:
           return "S3_WIFI_TX_DURATION";
           break;
        case A_S3_WIFI_OFFSET_REG:
           return "S3_WIFI_OFFSET_REG";
           break;
    }
    return "**************";
}

#endif

static uint64_t esp32s3_wifi_read(void *opaque, hwaddr addr, unsigned int size)
{

    Esp32WifiState *s = ESP32_WIFI(opaque);
    uint32_t r = s->mem[addr/4];

    switch(addr) {
        case A_S3_WIFI_DMA_INLINK:
            r=s->dma_inlink_address;
            break;
        case A_S3_WIFI_DMA_IN_STATUS:
            r= r & ~0x1;
            break;
        case A_S3_WIFI_DMA_INT_STATUS:
        case A_S3_WIFI_DMA_INT_CLR:
            r=s->raw_interrupt;
            break;
        case A_S3_WIFI_STATUS:
        case A_S3_WIFI_DMA_OUT_STATUS:
            r=1;
            break;
    }

#if DEBUG
    printf("esp32s3_wifi_read  %25s(0x%04lx)= 0x%08x\n",get_reg_name(addr),(unsigned long) addr,r);
#endif

    return r;
}
static void set_interrupt(Esp32WifiState *s,int e) {
    s->raw_interrupt |= e;
    qemu_set_irq(s->irq, 1);
}

void Esp32_WLAN_frame_delivered(Esp32WifiState *s){
    s->raw_interrupt |= 0x80;
    qemu_set_irq(s->irq, 1);
}

static void esp32s3_wifi_write(void *opaque, hwaddr addr, uint64_t value,
                                 unsigned int size) {
    Esp32WifiState *s = ESP32_WIFI(opaque);
#if DEBUG
    printf("esp32s3_wifi_write %25s(0x%04lx)= 0x%08lx\n",get_reg_name(addr),(unsigned long) addr, (unsigned long) value);
#endif
    switch (addr) {
        case A_S3_WIFI_DMA_INLINK:
            s->dma_inlink_address = value;
            s->dma_inlink_ptr = value;
            s->mem[R_S3_WIFI_NEXT_RX_DSCR] = value;
            s->mem[R_S3_WIFI_LAST_RX_DSCR] = value;
            break;
        case A_S3_WIFI_DMA_INT_CLR:
            s->raw_interrupt &= ~value;
            if(s->raw_interrupt == 0)
                qemu_set_irq(s->irq, 0);
            break;
        case A_S3_WIFI_DMA_OUTLINK:
            if (value & 0xc0000000) {
                // do a DMA transfer to the hardware from esp32 memory
                mac80211_frame frame = { 0 };
                dma_list_item item = { 0 };
                unsigned memaddr = (0x3fc00000 | (value & 0xfffff));
                if (address_space_read(&address_space_memory, memaddr,
                        MEMTXATTRS_UNSPECIFIED, &item,
                        sizeof(item)) != MEMTX_OK) {
                    qemu_log_mask(LOG_GUEST_ERROR,
                                  "Wi-Fi TX descriptor read failed\n");
                    set_interrupt(s, 0x80);
                    break;
                }
                if (item.length > IEEE80211_FRAME_CAPACITY) {
                    qemu_log_mask(LOG_GUEST_ERROR,
                                  "Wi-Fi TX frame too long: %u\n", item.length);
                    set_interrupt(s, 0x80);
                    break;
                }
                if (address_space_read(&address_space_memory, item.address,
                        MEMTXATTRS_UNSPECIFIED, &frame,
                        item.length) != MEMTX_OK) {
                    qemu_log_mask(LOG_GUEST_ERROR,
                                  "Wi-Fi TX buffer read failed\n");
                    set_interrupt(s, 0x80);
                    break;
                }
                // frame from esp32 to ap
                frame.frame_length=item.length;
                frame.next_frame=0;
                Esp32_WLAN_handle_frame(s, &frame);
                set_interrupt(s,0x80);
            }
    }
    s->mem[addr/4]=value;
}

static int match_mac_address(uint8_t *a1,uint8_t *a2) {
    if(!memcmp(a1,a2,6)) return 1;
    if(!memcmp(a1,BROADCAST,6)) return 1;
    return 0;
}
// frame from ap to esp32
void Esp32_sendFrame(Esp32WifiState *s, mac80211_frame *frame,int length, int signal_strength) {
    dma_list_item item = { 0 };
    size_t total;

    if(s->dma_inlink_address==0) return;
    if (length < IEEE80211_HEADER_SIZE || length > IEEE80211_FRAME_CAPACITY) {
        qemu_log_mask(LOG_GUEST_ERROR, "Wi-Fi invalid RX frame length: %d\n",
                      length);
        return;
    }
    total = sizeof(wifi_pkt_rx_ctrl_c3_t) + length;
    if (address_space_read(&address_space_memory, s->dma_inlink_ptr,
            MEMTXATTRS_UNSPECIFIED, &item, sizeof(item)) != MEMTX_OK) {
        qemu_log_mask(LOG_GUEST_ERROR, "Wi-Fi RX descriptor read failed\n");
        return;
    }
    if (total > item.size) {
        qemu_log_mask(LOG_GUEST_ERROR, "Wi-Fi RX buffer too small: %u < %zu\n",
                      item.size, total);
        return;
    }
    uint8_t *header = g_malloc0(total);
    wifi_pkt_rx_ctrl_c3_t *pkt=(wifi_pkt_rx_ctrl_c3_t *)header;
    *pkt=(wifi_pkt_rx_ctrl_c3_t){
        .rssi=(signal_strength+(rand()%10)+96),
        .rate=11,
        .sig_len=length,
        .sig_len_copy=length,
        .legacy_length=length,
        .noise_floor=-97,
        .channel=esp32_wifi_channel,
        .timestamp=qemu_clock_get_ns(QEMU_CLOCK_VIRTUAL)/1000,
    };
    // These 4 bits are set if the mac addresses previously stored at 0x40 and 0x48
    // match the destination or bssid addresses in the frame
    if(match_mac_address(frame->destination_address,(uint8_t *)s->mem+A_WIFI_MAC_ADDR_FST_0))
        pkt->damatch0=1;
    if(match_mac_address(frame->destination_address,(uint8_t *)s->mem+A_WIFI_MAC_ADDR_FST_1))
        pkt->damatch1=1;
    if(match_mac_address(frame->bssid_address,(uint8_t *)s->mem+A_WIFI_MAC_ADDR_FST_0))
        pkt->bssidmatch0=1;
    if(match_mac_address(frame->bssid_address,(uint8_t *)s->mem+A_WIFI_MAC_ADDR_FST_1))
        pkt->bssidmatch1=1;
    //printf("...%x %x\n",header[3],frame->destination_address[0]);

    memcpy(header+sizeof(wifi_pkt_rx_ctrl_c3_t),frame,length);
    length+=sizeof(wifi_pkt_rx_ctrl_c3_t);
    // do a DMA transfer from the hardware to esp32 memory
    if (address_space_write(&address_space_memory, item.address,
            MEMTXATTRS_UNSPECIFIED, header, length) != MEMTX_OK) {
        qemu_log_mask(LOG_GUEST_ERROR, "Wi-Fi RX buffer write failed\n");
        g_free(header);
        return;
    }
    item.length=length;
    item.eof=1;
    address_space_write(&address_space_memory, s->dma_inlink_ptr, MEMTXATTRS_UNSPECIFIED,&item,4);
    s->dma_inlink_ptr=item.next;
    if(s->dma_inlink_ptr == 0) s->dma_inlink_ptr = s->dma_inlink_address;
    set_interrupt(s, 0x1004000);
    g_free(header);
}

static const MemoryRegionOps esp32s3_wifi_ops = {
    .read =  esp32s3_wifi_read,
    .write = esp32s3_wifi_write,
    .endianness = DEVICE_LITTLE_ENDIAN,
};

static void esp32s3_wifi_reset_enter(Object *obj, ResetType type)
{
    Esp32WifiState *s = ESP32_WIFI(obj);

    s->dma_inlink_address=0;
    s->dma_inlink_ptr=0;
    memset(s->mem,0,sizeof(s->mem));
    Esp32_WLAN_reset_ap(s);
}

static void esp32s3_wifi_realize(DeviceState *dev, Error **errp)
{
    Esp32WifiState *s = ESP32_WIFI(dev);
    SysBusDevice *sbd = SYS_BUS_DEVICE(dev);
    s->dma_inlink_address=0;
    s->dma_inlink_ptr=0;

    memory_region_init_io(&s->iomem, OBJECT(dev), &esp32s3_wifi_ops, s,
                          TYPE_ESP32_WIFI, 0x1000);
    sysbus_init_mmio(sbd, &s->iomem);
    sysbus_init_irq(sbd, &s->irq);
    memset(s->mem,0,sizeof(s->mem));
    Esp32_WLAN_setup_ap(dev, s);

}
static const Property esp32s3_wifi_properties[] = {
    DEFINE_NIC_PROPERTIES(Esp32WifiState, conf),
    DEFINE_PROP_UINT32("rx-frame-us", Esp32WifiState, rx_frame_us, 5000),
    DEFINE_PROP_BOOL("access-points", Esp32WifiState, access_points, true),
};

static void esp32s3_wifi_class_init(ObjectClass *klass, const void *data)
{
    DeviceClass *dc = DEVICE_CLASS(klass);

    dc->realize = esp32s3_wifi_realize;
    set_bit(DEVICE_CATEGORY_NETWORK, dc->categories);
    dc->desc = "ESP32-S3 Wi-Fi MAC";
    device_class_set_props(dc, esp32s3_wifi_properties);
    ResettableClass *rc = RESETTABLE_CLASS(klass);
    rc->phases.enter = esp32s3_wifi_reset_enter;
}


static const TypeInfo esp32s3_wifi_info = {
    .name = TYPE_ESP32_WIFI,
    .parent = TYPE_SYS_BUS_DEVICE,
    .instance_size = sizeof(Esp32WifiState),
    .class_init    = esp32s3_wifi_class_init,
};

static void esp32s3_wifi_register_types(void)
{
    type_register_static(&esp32s3_wifi_info);
}

type_init(esp32s3_wifi_register_types)
