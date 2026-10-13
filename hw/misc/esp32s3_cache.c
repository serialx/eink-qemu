/*
 * ESP32-S3 ICache emulation
 *
 * Copyright (c) 2024 Espressif Systems (Shanghai) Co. Ltd.
 *
 * This program is free software; you can redistribute it and/or modify
 * it under the terms of the GNU General Public License version 2 or
 * (at your option) any later version.
 */


#include "qemu/osdep.h"
#include "hw/core/qdev-properties.h"
#include "qemu/log.h"
#include "qemu/module.h"
#include "qemu/timer.h"
#include "qapi/error.h"
#include "qemu/error-report.h"
#include "system/address-spaces.h"
#include "hw/core/sysbus.h"
#include "hw/core/irq.h"
#include "hw/core/qdev-properties.h"
#include "hw/misc/esp32s3_cache.h"
#include "hw/misc/esp32s3_xts_aes.h"
#include "system/block-backend-io.h"
#include "hw/misc/esp32s3_reg.h"


#define CACHE_DEBUG      0
#define CACHE_WARNING    0


/**
 * @brief Checks that the enable flag is enabled in the I/O register. If that's the case,
 *        `done` flag is returned and the `enable` flag is cleared from register.
 *        Else, 0 is returned.
*/
static inline uint32_t check_and_reset_ena(uint32_t* hwreg, uint32_t ena_mask, uint32_t done_mask)
{
    uint32_t regval = *hwreg;

    if (regval & ena_mask) {
        regval &= ~ena_mask;
        regval |= done_mask;
        *hwreg = regval;
    }

    return regval;
}


static inline uint32_t esp32s3_read_mmu_value(ESP32S3CacheState *s, hwaddr reg_addr)
{
    /* Make the assumption that the address is aligned on sizeof(uint32_t) */
    const uint32_t index = reg_addr / sizeof(uint32_t);
    return (uint32_t) s->mmu[index].val;
}

uint64_t esp32s3_cache_access_ns(void *opaque, uint32_t addr,
                                unsigned size, bool write, int64_t now, uint32_t cpu_hz)
{
    ESP32S3CacheState *s = opaque;
    uint64_t ns = 0;
    uint32_t ctrl = s->regs[ESP32S3_CACHE_REG_IDX(A_EXTMEM_DCACHE_CTRL)];
    unsigned line_size = 16 << MIN((ctrl >> 3) & 3, 2);
    unsigned sets = ((ctrl & 4) ? 65536 : 32768) / (4 * line_size);

    if (!s->psram_read_ns || !s->dcache_enable || !s->psram ||
        addr < ESP32S3_DCACHE_BASE ||
        addr >= ESP32S3_DCACHE_BASE + ESP32S3_EXTMEM_REGION_SIZE) {
        return 0;
    }
    for (uint64_t a = addr; a < (uint64_t)addr + size;) {
        uint32_t offset = a - ESP32S3_DCACHE_BASE;
        if (offset >= ESP32S3_EXTMEM_REGION_SIZE) {
            break;
        }
        ESP32S3MMUEntry e = s->mmu[offset / ESP32S3_PAGE_SIZE];
        if (!e.invalid && e.type == ESP32S3_MMU_TYPE_PSRAM) {
            uint32_t phys = e.page_number * ESP32S3_PAGE_SIZE +
                            offset % ESP32S3_PAGE_SIZE;
            uint32_t tag = phys / line_size;
            ESP32S3CacheLine *set = s->timing_lines[tag % sets];
            ESP32S3CacheLine *line = NULL, *victim = &set[0];
            for (unsigned way = 0; way < 4; way++) {
                if (set[way].valid && set[way].tag == tag) {
                    line = &set[way];
                    break;
                }
                if (!set[way].valid ||
                    (victim->valid && set[way].age < victim->age)) {
                    victim = &set[way];
                }
            }
            if (!line) {
                /* The requested word resumes the CPU before the refill tail
                 * releases the bus. Hits overlap that tail and any queued
                 * dirty writeback; the next miss waits for bus ownership. */
                ns += MAX(s->bus_until_ns - now - (int64_t)ns, 0);
                ns += s->psram_read_ns +
                      ((uint64_t)s->psram_read_cycles * 1000000000 + cpu_hz - 1) / cpu_hz;
                s->bus_until_ns = now + ns + s->psram_read_tail_ns;
                if (victim->valid && victim->dirty) {
                    s->bus_until_ns += s->psram_write_ns +
                        ((uint64_t)s->psram_write_cycles * 1000000000 + cpu_hz - 1) / cpu_hz;
                }
                line = victim;
                *line = (ESP32S3CacheLine) { .tag = tag, .valid = true };
            }
            line->dirty |= write;
            line->age = ++s->timing_age;
        }
        a = (a & ~(uint64_t)(line_size - 1)) + line_size;
    }
    return ns;
}

static void esp32s3_cache_sync(ESP32S3CacheState *s, uint32_t value)
{
    uint32_t addr = s->regs[ESP32S3_CACHE_REG_IDX(A_EXTMEM_DCACHE_SYNC_ADDR)];
    uint32_t size = s->regs[ESP32S3_CACHE_REG_IDX(A_EXTMEM_DCACHE_SYNC_SIZE)] & 0x7fffff;
    unsigned line_size = 16 << MIN((s->regs[0] >> 3) & 3, 2);

    /* ROM Cache_{Invalidate_DCache,Clean,WriteBack}_All use (0, 0).
     * Visit physical tags directly, including pages no longer mapped. */
    if (addr == 0 && size == 0) {
        for (unsigned set = 0; set < ARRAY_SIZE(s->timing_lines); set++) {
            for (unsigned way = 0; way < 4; way++) {
                ESP32S3CacheLine *line = &s->timing_lines[set][way];
                if (value & 6) {
                    line->dirty = false;
                }
                if (value & 1) {
                    line->valid = false;
                }
            }
        }
        return;
    }

    uint64_t begin = addr & ~(line_size - 1);
    uint64_t end = MIN(begin + (uint64_t)size * line_size,
                       ESP32S3_DCACHE_BASE + ESP32S3_EXTMEM_REGION_SIZE);
    for (uint64_t a = MAX(begin, ESP32S3_DCACHE_BASE); a < end; a += line_size) {
        uint32_t off = a - ESP32S3_DCACHE_BASE;
        ESP32S3MMUEntry e = s->mmu[off / ESP32S3_PAGE_SIZE];
        if (e.invalid || e.type != ESP32S3_MMU_TYPE_PSRAM) {
            continue;
        }
        uint32_t tag = (e.page_number * ESP32S3_PAGE_SIZE +
                        off % ESP32S3_PAGE_SIZE) / line_size;
        unsigned sets = ((s->regs[0] & 4) ? 65536 : 32768) / (4 * line_size);
        for (unsigned way = 0; way < 4; way++) {
            ESP32S3CacheLine *line = &s->timing_lines[tag % sets][way];
            if (line->valid && line->tag == tag) {
                if (value & 6) {
                    line->dirty = false;
                }
                if (value & 1) {
                    line->valid = false;
                }
            }
        }
    }
}


static void esp32s3_mmu_invalidate_page(ESP32S3CacheState *s, hwaddr virt_addr, hwaddr phys_addr, bool is_psram, bool clear_mr)
{
    IOMMUTLBEvent event = {
        .type = IOMMU_NOTIFIER_UNMAP,
        .entry = {
            .target_as = is_psram ? &s->psram_as : &s->flash_as,
            .iova = virt_addr,
            .translated_addr = phys_addr,
            .addr_mask = ESP32S3_PAGE_SIZE - 1,
        }
    };
    memory_region_notify_iommu(&s->iommu, 0, event);

    /* If the page was mapped to flash, clear the content */
    if (!is_psram && clear_mr) {
        const uint32_t invalid_value = 0xdeadbeef;
        uint32_t* cache_word_data = (void*) ((uintptr_t) memory_region_get_ram_ptr(&s->flash_mr) + phys_addr);

        for (int i = 0; i < ESP32S3_PAGE_SIZE / sizeof(invalid_value); i++) {
            cache_word_data[i] = invalid_value;
        }
    }
}


static inline void esp32s3_write_mmu_value(ESP32S3CacheState *s, hwaddr reg_addr, uint32_t value)
{
    ESP32S3XtsAesClass *xts_aes_class = ESP32S3_XTS_AES_GET_CLASS(s->xts_aes);
    /* Make the assumption that the address is aligned on sizeof(uint32_t) */
    const uint32_t index = reg_addr / sizeof(uint32_t);
    /* Reserved bits shall always be 0 */
    ESP32S3MMUEntry e = { .val = value };
    const ESP32S3MMUEntry former = s->mmu[index];
    /* Always keep reserved as 0 */
    e.reserved = 0;
#if CACHE_DEBUG
    info_report("[CACHE] esp32s3_write_mmu_value 0x%lx = %08x, index=%d", reg_addr, value, index);
#endif
    if (former.val != e.val) {
        /* The entry contains the index of the 64KB block from the flash memory */
        const uint32_t physical_address = e.page_number * ESP32S3_PAGE_SIZE;
        const uint32_t former_physaddr = former.page_number * ESP32S3_PAGE_SIZE;
        const uint32_t virtaddr = index * ESP32S3_PAGE_SIZE;
        /* Invalidate the former mapping and clear the MR if and only if this is an "invalidate" operation */
        esp32s3_mmu_invalidate_page(s, virtaddr, former_physaddr, former.type == ESP32S3_MMU_TYPE_PSRAM, e.invalid);

        if (!e.invalid) {
            if (e.type == ESP32S3_MMU_TYPE_FLASH && s->flash_blk != NULL) {
                uint8_t* cache_data = ((uint8_t*) memory_region_get_ram_ptr(&s->flash_mr)) + physical_address;
                blk_pread(s->flash_blk, physical_address, ESP32S3_PAGE_SIZE, cache_data, 0);

                if (xts_aes_class->is_flash_enc_enabled(s->xts_aes)) {
                    xts_aes_class->decrypt(s->xts_aes, physical_address, cache_data, ESP32S3_PAGE_SIZE);
                }
            }
        }
        s->mmu[index].val = e.val;
    }
}


static uint64_t esp32s3_cache_read(void *opaque, hwaddr addr, unsigned int size)
{
    ESP32S3CacheState *s = ESP32S3_CACHE(opaque);
    const hwaddr index = ESP32S3_CACHE_REG_IDX(addr);
    uint64_t r = 0;

    if (addr & 0x3) {
        /* Unaligned access, should we fail? */
        error_report("[QEMU] unaligned access to the cache registers");
    }

    switch(addr) {
        case A_EXTMEM_DCACHE_CTRL:
            r = s->regs[index];
            break;
        case A_EXTMEM_DCACHE_CTRL1:
            r = s->regs[index];
            break;
        case A_EXTMEM_ICACHE_CTRL:
            r = s->icache_enable;
            break;
        case A_EXTMEM_ICACHE_CTRL1:
            r = s->regs[index];
            break;
        /* For the following registers, mark the bit as done only if the feature was enabled */
        case A_EXTMEM_DCACHE_OCCUPY_CTRL:
            r = check_and_reset_ena(&s->regs[index],
                                    R_EXTMEM_DCACHE_OCCUPY_CTRL_OCCUPY_ENA_MASK,
                                    R_EXTMEM_DCACHE_OCCUPY_CTRL_OCCUPY_DONE_MASK);
            break;
        case A_EXTMEM_DCACHE_SYNC_CTRL:
            s->regs[index] |= 1<<3;
            r = check_and_reset_ena(&s->regs[index],
                                    R_EXTMEM_DCACHE_SYNC_CTRL_INVALIDATE_ENA_MASK,
                                    R_EXTMEM_DCACHE_SYNC_CTRL_SYNC_DONE_MASK);
            break;
        case A_EXTMEM_ICACHE_SYNC_CTRL:
            r = check_and_reset_ena(&s->regs[index],
                                    R_EXTMEM_ICACHE_SYNC_CTRL_INVALIDATE_ENA_MASK,
                                    R_EXTMEM_ICACHE_SYNC_CTRL_SYNC_DONE_MASK);
            break;
        case A_EXTMEM_DCACHE_AUTOLOAD_CTRL:
            r = check_and_reset_ena(&s->regs[index],
                                    R_EXTMEM_DCACHE_AUTOLOAD_CTRL_AUTOLOAD_ENA_MASK,
                                    R_EXTMEM_DCACHE_AUTOLOAD_CTRL_AUTOLOAD_DONE_MASK);
            break;
        case A_EXTMEM_ICACHE_AUTOLOAD_CTRL:
            r = check_and_reset_ena(&s->regs[index],
                                    R_EXTMEM_ICACHE_AUTOLOAD_CTRL_AUTOLOAD_ENA_MASK,
                                    R_EXTMEM_ICACHE_AUTOLOAD_CTRL_AUTOLOAD_DONE_MASK);
            break;
        case A_EXTMEM_DCACHE_PRELOAD_CTRL:
            r = check_and_reset_ena(&s->regs[index],
                                    R_EXTMEM_DCACHE_PRELOAD_CTRL_PRELOAD_ENA_MASK,
                                    R_EXTMEM_DCACHE_PRELOAD_CTRL_PRELOAD_DONE_MASK);
            break;
        case A_EXTMEM_ICACHE_PRELOAD_CTRL:
            r = check_and_reset_ena(&s->regs[index],
                                    R_EXTMEM_ICACHE_PRELOAD_CTRL_PRELOAD_ENA_MASK,
                                    R_EXTMEM_ICACHE_PRELOAD_CTRL_PRELOAD_DONE_MASK);
            break;
        case A_EXTMEM_DCACHE_FREEZE:
            r = s->regs[index];
            break;
        case A_EXTMEM_ICACHE_FREEZE:
            r = s->regs[index];
            break;
        case A_EXTMEM_CACHE_STATE:
            /* Return the state of ICache as idle:
             * 1: Idle
             * 0: Busy/Not idle */
            r = 1 << R_EXTMEM_CACHE_STATE_DCACHE_STATE_SHIFT;
            r |= 1 << R_EXTMEM_CACHE_STATE_ICACHE_STATE_SHIFT;
            break;
        case A_EXTMEM_DCACHE_SYNC_SIZE:
            break;

        case ESP32S3_MMU_TABLE_OFFSET ... (ESP32S3_MMU_TABLE_OFFSET + ESP32S3_MMU_SIZE - 1):
#if CACHE_WARNING
            info_report("[CACHE] Reading 0x%lx (0x%lx)", addr, r);
#endif
            r = esp32s3_read_mmu_value(s, addr - ESP32S3_MMU_TABLE_OFFSET);
            break;
        default:
#if CACHE_WARNING
            warn_report("[CACHE] Unsupported read to 0x%lx", addr);
#endif
            break;
    }

#if CACHE_DEBUG
    info_report("[CACHE] Reading 0x%lx (0x%lx)", addr, r);
#endif

    return r;
}

static void esp32s3_cache_write(void *opaque, hwaddr addr, uint64_t value,
                                unsigned int size)
{
    ESP32S3CacheState *s = ESP32S3_CACHE(opaque);

    const hwaddr index = ESP32S3_CACHE_REG_IDX(addr);

    if (index < ESP32S3_CACHE_REG_COUNT) {
        switch (addr) {
            case A_EXTMEM_DCACHE_CTRL:
                if ((s->regs[index] ^ value) & 0x1c) {
                    memset(s->timing_lines, 0, sizeof(s->timing_lines));
                }
                s->regs[index] = value & 0x1d;
                s->dcache_enable = value & 1;
                break;
            case A_EXTMEM_DCACHE_SYNC_CTRL:
                esp32s3_cache_sync(s, value);
                /* Invalidate/writeback commands complete synchronously. */
                s->regs[index] = 8;
                break;
            case A_EXTMEM_DCACHE_CTRL1:
                /* CTRL1 gates each core's bus; it does not disable cache. */
                s->regs[index] = value & 3;
                break;
            case A_EXTMEM_ICACHE_CTRL:
                s->icache_enable = value & 1;
                break;
            case A_EXTMEM_ICACHE_CTRL1:
                s->regs[index] = value & 3;
                break;
            case A_EXTMEM_ICACHE_FREEZE:
                if (value & R_EXTMEM_ICACHE_FREEZE_ICACHE_FREEZE_ENA_MASK) {
                    /* Enable freeze, set DONE bit */
                    s->regs[index] |= R_EXTMEM_ICACHE_FREEZE_ICACHE_FREEZE_DONE_MASK;
                } else {
                    /* Disable freeze, clear DONE bit */
                    s->regs[index] &= ~R_EXTMEM_ICACHE_FREEZE_ICACHE_FREEZE_DONE_MASK;
                }
                break;
            case A_EXTMEM_DCACHE_FREEZE:
                if (value & R_EXTMEM_DCACHE_FREEZE_DCACHE_FREEZE_ENA_MASK) {
                    /* Enable freeze, set DONE bit */
                    s->regs[index] |= R_EXTMEM_DCACHE_FREEZE_DCACHE_FREEZE_DONE_MASK;
                } else {
                    /* Disable freeze, clear DONE bit */
                    s->regs[index] &= ~R_EXTMEM_DCACHE_FREEZE_DCACHE_FREEZE_DONE_MASK;
                }
                break;
            default:
                s->regs[index] = value;
                break;
        }
    } else if (addr >= ESP32S3_MMU_TABLE_OFFSET &&
               addr < ESP32S3_MMU_TABLE_OFFSET + ESP32S3_MMU_SIZE) {
        esp32s3_write_mmu_value(s, addr - ESP32S3_MMU_TABLE_OFFSET, value);
    }

#if CACHE_DEBUG
    info_report("[CACHE] Writing 0x%lx = %08lx, size=%i", addr, value, size);
#endif

}

static const MemoryRegionOps esp32s3_cache_ops = {
    .read =  esp32s3_cache_read,
    .write = esp32s3_cache_write,
    .endianness = DEVICE_LITTLE_ENDIAN,
};

static void esp32s3_cache_reset_hold(Object *obj, ResetType type)
{
    ESP32S3CacheState *s = ESP32S3_CACHE(obj);
    memset(s->regs, 0, ESP32S3_CACHE_REG_COUNT * sizeof(*s->regs));
    memset(s->timing_lines, 0, sizeof(s->timing_lines));
    s->timing_age = 0;
    s->bus_until_ns = 0;
    s->dcache_enable = false;
    s->icache_enable = false;
    s->regs[ESP32S3_CACHE_REG_IDX(A_EXTMEM_DCACHE_CTRL1)] = 3;
    s->regs[ESP32S3_CACHE_REG_IDX(A_EXTMEM_ICACHE_CTRL1)] = 3;
    s->regs[ESP32S3_CACHE_REG_IDX(A_EXTMEM_DCACHE_OCCUPY_CTRL)] =
        R_EXTMEM_DCACHE_OCCUPY_CTRL_OCCUPY_DONE_MASK;

    /* Initialize the MMU with invalid entries */
    for (int i = 0; i < ESP32S3_MMU_TABLE_ENTRY_COUNT; i++) {
        s->mmu[i].invalid = 1;
    }

    /* On reset, autoload must be set to done (ready) */
    s->regs[ESP32S3_CACHE_REG_IDX(A_EXTMEM_ICACHE_AUTOLOAD_CTRL)] = R_EXTMEM_ICACHE_AUTOLOAD_CTRL_AUTOLOAD_DONE_MASK;
    /* Same goes for the manual preload */
    s->regs[ESP32S3_CACHE_REG_IDX(A_EXTMEM_ICACHE_PRELOAD_CTRL)] = R_EXTMEM_ICACHE_PRELOAD_CTRL_PRELOAD_DONE_MASK;

    /* On reset, autoload must be set to done (ready) */
    s->regs[ESP32S3_CACHE_REG_IDX(A_EXTMEM_DCACHE_AUTOLOAD_CTRL)] = R_EXTMEM_DCACHE_AUTOLOAD_CTRL_AUTOLOAD_DONE_MASK;
    /* Same goes for the manual preload */
    s->regs[ESP32S3_CACHE_REG_IDX(A_EXTMEM_DCACHE_PRELOAD_CTRL)] = R_EXTMEM_DCACHE_PRELOAD_CTRL_PRELOAD_DONE_MASK;
}

static void esp32s3_cache_realize(DeviceState *dev, Error **errp)
{
    /* Initialize the registers */
    esp32s3_cache_reset_hold(OBJECT(dev), RESET_TYPE_COLD);
    ESP32S3CacheState *s = ESP32S3_CACHE(dev);

    /* Make sure XTS_AES was set or issue an error */
    if (s->xts_aes == NULL) {
        error_report("[CACHE] XTS_AES controller must be set!");
    }

    if (s->flash_blk != NULL) {
        /* There is no way to have a MemoryRegion bound to a block device, nor a protable way to have a MemoryRegion
        * region mmap-ed to a file (POSIX systems only). So workaround this by defining some RAM that will be filled
        * with the flash block content every time a map is requested */
        memory_region_init_ram(&s->flash_mr, OBJECT(s), "esp32s3.cache.flash_mr",
                            blk_getlength(s->flash_blk), &error_fatal);

        /* Initialize the address space that will contain the flash MemoryRegion */
        address_space_init(&s->flash_as, &s->flash_mr, "esp32s3.cache.flash_as");
    }

    if (s->psram != NULL) {
        /* Initialize the physical address space for the PSRAM, this will be referenced by the IOMMU. */
        address_space_init(&s->psram_as, &s->psram->data_mr, "esp32s3.cache.psram_as");
    }
}

static void esp32s3_cache_init(Object *obj)
{
    ESP32S3CacheState *s = ESP32S3_CACHE(obj);
    SysBusDevice *sbd = SYS_BUS_DEVICE(obj);

    /* Since the cache I/O region and the MMU I/O region are adjacent, let's use the same MemoryRegion object
     * for both, this will simplify the machine architecture. */
    memory_region_init_io(&s->iomem, obj, &esp32s3_cache_ops, s,
                          TYPE_ESP32S3_CACHE, TYPE_ESP32S3_CACHE_IO_SIZE + ESP32S3_MMU_SIZE);

    /* Initialize the dcache and icache cache areas, they are aliases of eachother */
    memory_region_init_iommu(&s->iommu,
                            sizeof(s->iommu),
                            TYPE_ESP32S3_MMU_REGION,
                            OBJECT(s),
                            "esp32s3_iommu", ESP32S3_EXTMEM_REGION_SIZE);

    /* The Dcache and the Icache are just aliases to the iommu memory region since all the accesses will require
     * to go through a translation. */
    memory_region_init_alias(&s->dcache, OBJECT(s), "esp32s3.dcache",
                           MEMORY_REGION(&s->iommu), 0, ESP32S3_EXTMEM_REGION_SIZE);
    memory_region_init_alias(&s->icache, OBJECT(s), "esp32s3.icache",
                           &s->dcache, 0, ESP32S3_EXTMEM_REGION_SIZE);

    sysbus_init_mmio(sbd, &s->iomem);
}

static void esp32s3_cache_class_init(ObjectClass *klass, const void *data)
{
    static const Property props[] = {
        DEFINE_PROP_UINT32("psram-read-ns", ESP32S3CacheState, psram_read_ns, 0),
        DEFINE_PROP_UINT32("psram-write-ns", ESP32S3CacheState, psram_write_ns, 0),
        DEFINE_PROP_UINT32("psram-read-tail-ns", ESP32S3CacheState, psram_read_tail_ns, 0),
        DEFINE_PROP_UINT32("psram-read-cycles", ESP32S3CacheState, psram_read_cycles, 0),
        DEFINE_PROP_UINT32("psram-write-cycles", ESP32S3CacheState, psram_write_cycles, 0),
    };
    DeviceClass *dc = DEVICE_CLASS(klass);
    ResettableClass *rc = RESETTABLE_CLASS(klass);

    rc->phases.hold = esp32s3_cache_reset_hold;
    dc->realize = esp32s3_cache_realize;
    device_class_set_props(dc, props);
}

static const TypeInfo esp32s3_cache_info = {
    .name = TYPE_ESP32S3_CACHE,
    .parent = TYPE_SYS_BUS_DEVICE,
    .instance_size = sizeof(ESP32S3CacheState),
    .instance_init = esp32s3_cache_init,
    .class_init = esp32s3_cache_class_init
};


static uint64_t esp32s3_mmu_region_page_size(IOMMUMemoryRegion *iommu)
{
    return ESP32S3_PAGE_SIZE;
}


/**
 * @brief Function called by the virtual machine when it needs to translate a virtual address (MMU page) into a physical
 * address. Even though we use 64KB pages for the ESP32-S3, this function can still be called several times for the same
 * 64KB page since the host computer's MMU pages are most likely smaller (4KB).
 */
static IOMMUTLBEntry esp32s3_mmu_region_translate(IOMMUMemoryRegion *iommu, hwaddr addr,
                                                  IOMMUAccessFlags flag, int iommu_idx)
{
    ESP32S3CacheState *s = container_of(iommu, ESP32S3CacheState, iommu);

    IOMMUTLBEntry ret = {
        /* Flash address space by default (most likely) */
        .target_as = &s->flash_as,
        .addr_mask = ESP32S3_PAGE_SIZE - 1,
    };

    /* Check which page is being written */
    const uint32_t index = addr / ESP32S3_PAGE_SIZE;
    const uint32_t offset = addr % ESP32S3_PAGE_SIZE;
    const ESP32S3MMUEntry entry = s->mmu[index];

    /* Make sure the virtual and physical addresses are both aligned on ESP32S3_PAGE_SIZE when returned to the caller */
    ret.translated_addr = entry.page_number * ESP32S3_PAGE_SIZE;
    ret.iova = addr - offset;

    if (entry.type == ESP32S3_MMU_TYPE_PSRAM) {
        ret.target_as = &s->psram_as;
        /* If there is no PSRAM connected to the machine, give no permission to the address space */
        ret.perm = (s->psram == NULL) ? IOMMU_NONE : IOMMU_RW;
    } else {
        ret.perm = (s->flash_blk == NULL) ? IOMMU_NONE : IOMMU_RO;
    }

#if CACHE_DEBUG
    info_report("[Cache] Translate virtual address %08lx to %08lx (idx: %d, val: %x, offset: %08x)\x1b[0m\n", ret.iova, ret.translated_addr, index, entry.val, offset);
#endif
    return ret;
}

static int esp32s3_mmu_region_attrs_to_index(IOMMUMemoryRegion *iommu, MemTxAttrs attrs)
{
    return 0;
}


static int esp32s3_mmu_region_notify_flag_changed(IOMMUMemoryRegion *iommu,
                                                  IOMMUNotifierFlag old,
                                                  IOMMUNotifierFlag new,
                                                  Error **errp)
{
    return 0;
}


static void esp32s3_mmu_region_class_init(ObjectClass *klass, const void *data)
{
    IOMMUMemoryRegionClass *imrc = IOMMU_MEMORY_REGION_CLASS(klass);

    imrc->translate = esp32s3_mmu_region_translate;
    imrc->attrs_to_index = esp32s3_mmu_region_attrs_to_index;
    imrc->get_min_page_size = esp32s3_mmu_region_page_size;
    imrc->notify_flag_changed = esp32s3_mmu_region_notify_flag_changed;
}

static const TypeInfo esp32s3_mmu_region_info = {
    .parent = TYPE_IOMMU_MEMORY_REGION,
    .name = TYPE_ESP32S3_MMU_REGION,
    .class_init = esp32s3_mmu_region_class_init,
};


static void esp32s3_cache_register_types(void)
{
    type_register_static(&esp32s3_cache_info);
    type_register_static(&esp32s3_mmu_region_info);
}

type_init(esp32s3_cache_register_types)
