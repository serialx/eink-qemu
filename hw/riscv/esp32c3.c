/*
 * ESP32-C3 SoC and machine
 *
 * Copyright (c) 2019-2022 Espressif Systems (Shanghai) Co. Ltd.
 *
 * This program is free software; you can redistribute it and/or modify
 * it under the terms of the GNU General Public License version 2 or
 * (at your option) any later version.
 */
#include "qemu/osdep.h"
#include "qemu/log.h"
#include "qemu/error-report.h"
#include "hw/core/qdev-properties.h"
#include "qemu/units.h"
#include "qemu/datadir.h"
#include "qapi/error.h"
#include "hw/core/boards.h"
#include "hw/core/loader.h"
#include "hw/riscv/riscv_hart.h"
#include "target/riscv/esp_cpu.h"
#include "hw/riscv/boot.h"
#include "hw/riscv/machines-qom.h"
#include "hw/riscv/numa.h"
#include "system/device_tree.h"
#include "system/kvm.h"
#include "system/runstate.h"
#include "system/reset.h"
#include "net/net.h"
#include "elf.h"
#include "system/system.h"
#include "system/physmem.h"
#include "system/blockdev.h"
#include "system/block-backend.h"
#include "hw/misc/esp32c3_reg.h"
#include "hw/misc/esp32c3_rtc_cntl.h"
#include "hw/misc/esp32c3_cache.h"
#include "hw/char/esp32c3_uart.h"
#include "hw/gpio/esp32c3_gpio.h"
#include "hw/gpio/esp32s3_gpio.h"
#include "hw/i2c/i2c.h"
#include "hw/sd/sd.h"
#include "hw/ssi/ssi.h"
#include "monitor/qdev.h"
#include "qobject/qlist.h"
#include "hw/misc/esp32s3_ana.h"
#include "hw/misc/esp32_fe.h"
#include "hw/misc/esp32_wifi.h"
#include "hw/nvram/esp32c3_efuse.h"
#include "hw/riscv/esp32c3_clk.h"
#include "hw/riscv/esp32c3_intmatrix.h"
#include "hw/misc/esp32c3_sha.h"
#include "hw/timer/esp32c3_timg.h"
#include "hw/timer/esp32c3_systimer.h"
#include "hw/ssi/esp32c3_spi.h"
#include "hw/misc/esp32c3_aes.h"
#include "hw/misc/esp32c3_rsa.h"
#include "hw/misc/esp32c3_hmac.h"
#include "hw/misc/esp32c3_ds.h"
#include "hw/misc/esp32c3_xts_aes.h"
#include "hw/misc/esp32c3_jtag.h"
#include "hw/dma/esp32c3_gdma.h"
#include "hw/display/esp_rgb.h"
#include "hw/net/can/esp32c3_twai.h"

#define ESP32C3_IO_WARNING          0

#define ESP32C3_RESET_ADDRESS       0x40000000
#define ESP32C3_RESET_GPIO_NAME     "esp32c3.machine.reset_gpio"
#define MB (1024*1024)


/* Define a new "class" which derivates from "MachineState" */
struct Esp32C3MachineState {
    MachineState parent;

    /* Attributes specific to our class */
    EspRISCVCPU soc;
    BusState periph_bus;
    MemoryRegion iomem;

    qemu_irq cpu_reset;

    DeviceState *eth; /* Ethernet controller */
    ESP32C3IntMatrixState intmatrix;
    ESP32C3UARTState uart[ESP32C3_UART_COUNT];
    ESP32S3GPIOState gpio;      /* eink-qemu: the S3 model, same register map, real pins */
    ESP32C3CacheState cache;
    ESP32C3EfuseState efuse;
    ESP32C3ClockState clock;
    ESP32C3GdmaState gdma;
    ESP32C3AesState aes;
    ESP32C3ShaState sha;
    ESP32C3RsaState rsa;
    ESP32C3HmacState hmac;
    ESP32C3DsState ds;
    ESP32C3XtsAesState xts_aes;
    ESP32C3TimgState timg[2];
    ESP32C3SysTimerState systimer;
    ESP32C3SpiState spi1;
    ESP32C3RtcCntlState rtccntl;
    DeviceState *jtag;          /* eink-qemu: the S3 model, a CDC console on a chardev */
    DeviceState *spi2;
    DeviceState *i2c0;
    DeviceState *saradc;
    ESPRgbState rgb;
    Esp32C3TWAIState twai;
};

/* Fake register used by ESP-IDF application to determine whether the code is running on real hardware or on QEMU */
#define A_SYSCON_ORIGIN_REG     0x3F8
/* Temporary macro for generating a random value from register SYSCON_RND_DATA_REG */
#define A_SYSCON_RND_DATA_REG   0x0B0

/* Temporary macro to mark the CPU as in non-debugging mode */
#define A_ASSIST_DEBUG_CORE_0_DEBUG_MODE_REG    0x098

/* Create a macro which defines the name of our new machine class */
#define TYPE_ESP32C3_MACHINE MACHINE_TYPE_NAME("esp32c3")
#define TYPE_X3_MACHINE MACHINE_TYPE_NAME("x3")
#define TYPE_X4_MACHINE MACHINE_TYPE_NAME("x4")

/* This will create a macro ESP32_MACHINE, which can be used to check and cast a generic MachineClass
 * to the specific class we defined above: Esp32C3MachineState. */
OBJECT_DECLARE_SIMPLE_TYPE(Esp32C3MachineState, ESP32C3_MACHINE)

/* Memory entries for ESP32-C3 */
enum MemoryRegions {
    ESP32C3_MEMREGION_IROM,
    ESP32C3_MEMREGION_DROM,
    ESP32C3_MEMREGION_DRAM,
    ESP32C3_MEMREGION_IRAM,
    ESP32C3_MEMREGION_RTCFAST,
    ESP32C3_MEMREGION_DCACHE,
    ESP32C3_MEMREGION_ICACHE,
    ESP32C3_MEMREGION_FRAMEBUF,
};

#define ESP32C3_INTERNAL_SRAM0_SIZE (16*1024)

static const struct MemmapEntry {
    hwaddr base;
    hwaddr size;
} esp32c3_memmap[] = {
    [ESP32C3_MEMREGION_IROM]    = { 0x40000000,  0x60000 },
    [ESP32C3_MEMREGION_DROM]    = { 0x3ff00000,  0x20000 },
    [ESP32C3_MEMREGION_DRAM]    = { 0x3fc80000,  0x60000 },
    /* Merge SRAM0 and SRAM1 into a single entry */
    [ESP32C3_MEMREGION_IRAM]    = { 0x4037c000,  0x60000 + ESP32C3_INTERNAL_SRAM0_SIZE },
    [ESP32C3_MEMREGION_RTCFAST] = { 0x50000000,   0x2000 },
    [ESP32C3_MEMREGION_DCACHE]  = { 0x3c000000, 0x800000 },
    [ESP32C3_MEMREGION_ICACHE]  = { 0x42000000, 0x800000 },
    /* Virtual Framebuffer, used for the graphical interface */
    [ESP32C3_MEMREGION_FRAMEBUF] = { 0x20000000, ESP_RGB_MAX_VRAM_SIZE }
};


static bool addr_in_range(hwaddr addr, hwaddr start, hwaddr end)
{
    return addr >= start && addr < end;
}

static uint64_t esp32c3_io_read(void *opaque, hwaddr addr, unsigned int size)
{
    if (addr_in_range(addr + ESP32C3_IO_START_ADDR, DR_REG_RTC_I2C_BASE, DR_REG_RTC_I2C_BASE + 0x100)) {
        return (uint32_t) 0xffffff;
    } else if (addr + ESP32C3_IO_START_ADDR == DR_REG_SYSCON_BASE + A_SYSCON_ORIGIN_REG) {
        /* Return "QEMU" as a 32-bit value */
        return 0x51454d55;
    } else if (addr + ESP32C3_IO_START_ADDR == DR_REG_SYSCON_BASE + A_SYSCON_RND_DATA_REG) {
        /* Return a random 32-bit value */
        static bool init = false;
        if (!init) {
            srand(time(NULL));
            init = true;
        }
        return rand();
    } else if (addr + ESP32C3_IO_START_ADDR == DR_REG_ASSIST_DEBUG_BASE + A_ASSIST_DEBUG_CORE_0_DEBUG_MODE_REG) {
        return 0;
    } else {
#if ESP32C3_IO_WARNING
        warn_report("[ESP32-C3] Unsupported read to $%08lx\n", ESP32C3_IO_START_ADDR + addr);
#endif
    }
    return 0;
}

static void esp32c3_io_write(void *opaque, hwaddr addr, uint64_t value, unsigned int size)
{
#if ESP32C3_IO_WARNING
        warn_report("[ESP32-C3] Unsupported write $%08lx = %08lx\n", ESP32C3_IO_START_ADDR + addr, value);
#endif
}


/* Define operations for I/OS */
static const MemoryRegionOps esp32c3_io_ops = {
    .read =  esp32c3_io_read,
    .write = esp32c3_io_write,
    .endianness = DEVICE_LITTLE_ENDIAN,
};


/**
 * @brief Callback invoked when SoC's ESP32C3_RESET_GPIO_NAME pin is toggled
 */
static void esp32c3_reset_request(void* opaque, int n, int level)
{
    if (level) {
        ShutdownCause cause = SHUTDOWN_CAUSE_GUEST_RESET;
        qemu_system_reset_request(cause);
    }
}


static void esp32c3_init_spi_flash(Esp32C3MachineState *ms, BlockBackend* blk)
{
    DeviceState *spi_master = DEVICE(&ms->spi1);
    BusState* spi_bus = qdev_get_child_bus(spi_master, "spi");
    const char* flash_model = NULL;
    int64_t image_size = blk_getlength(blk);

    switch (image_size) {
        case 2 * MB:
            flash_model = "w25x16";
            break;
        case 4 * MB:
            flash_model = "gd25q32";
            break;
        case 8 * MB:
            flash_model = "gd25q64";
            break;
        case 16 * MB:
            flash_model = "is25lp128";
            break;
        default:
            error_report("Drive size error: only 2, 4, 8, and 16MB images are supported");
            return;
    }

    /* Create the SPI flash model */
    DeviceState *flash_dev = qdev_new(flash_model);
    qdev_prop_set_drive(flash_dev, "drive", blk);
    qdev_prop_set_uint8(flash_dev, "cs", 1);

    /* Realize the SPI flash, its "drive" (blk) property must already be set! */
    qdev_realize(flash_dev, spi_bus, &error_fatal);
    qdev_connect_gpio_out_named(spi_master, SSI_GPIO_CS, 0,
                                qdev_get_gpio_in_named(flash_dev, SSI_GPIO_CS, 0));
}


static void esp32c3_init_openeth(Esp32C3MachineState *ms)
{
    MemoryRegion* mr = NULL;
    SysBusDevice* sbd = NULL;

    MemoryRegion* sys_mem = get_system_memory();

    /* Create a new OpenCores Ethernet component */
    DeviceState* open_eth_dev = qemu_create_nic_device("open_eth", true, NULL);
    if (!open_eth_dev) {
        return;
    }

    ms->eth = open_eth_dev;

    sbd = SYS_BUS_DEVICE(open_eth_dev);
    sysbus_realize(sbd, &error_fatal);

    /* OpenCores Ethernet has two memory regions: one for registers and one for descriptors,
        * we need to provide one I/O range for each of them */
    mr = sysbus_mmio_get_region(sbd, 0);
    memory_region_add_subregion_overlap(sys_mem, DR_REG_EMAC_BASE, mr, 0);
    mr = sysbus_mmio_get_region(sbd, 1);
    memory_region_add_subregion_overlap(sys_mem, DR_REG_EMAC_BASE + 0x400, mr, 0);

    sysbus_connect_irq(sbd, 0,
                        qdev_get_gpio_in(DEVICE(&ms->intmatrix), ETS_ETH_MAC_INTR_SOURCE));

}


static void esp32c3_load_firmware(MachineState *machine)
{
    Esp32C3MachineState *ms = ESP32C3_MACHINE(machine);
    const char *bios_filename = NULL;

    if (machine->firmware) {
        bios_filename = machine->firmware;
    }

    if (machine->kernel_filename) {
        if (bios_filename) {
            qemu_log("Warning: both -bios and -kernel arguments specified. Only loading the the -kernel file.\n");
        }
        bios_filename = machine->kernel_filename;
    }

    if (bios_filename) {
        /* Since EspRISCVCPU doens't have a RISCVHartArrayState field, let's bake one on the stack. It will only be
         * used to get the type of the RISC-V CPU (32 or 64 bits) in `riscv_load_kernel` */
        RISCVHartArrayState hart = {
            .harts = &ms->soc.parent_obj,
            .num_harts = 1,
        };

        /* The function `riscv_load_kernel` won't load the ELF file at its entry point, so we have to look
         * for the ELF entry point manually here */
        RISCVBootInfo boot_info;
        riscv_boot_info_init(&boot_info, &hart);

        uint64_t elf_entry = ESP32C3_RESET_ADDRESS;

        /* The entry point address should be populated regardless of the return value */
        load_elf_ram_sym(bios_filename, NULL, NULL, NULL,
                        &elf_entry, NULL, NULL, NULL, 0,
                        EM_RISCV, 1, 0, NULL, false, NULL);

        /* On failure, riscv_load_kernel exits the program */
        qemu_log("Loading kernel at address 0x%08" PRIx64 "\n", elf_entry);
        riscv_load_kernel(machine, &boot_info, elf_entry, false, NULL);
        if (elf_entry != ESP32C3_RESET_ADDRESS) {
            qdev_prop_set_uint64(DEVICE(&ms->soc), "resetvec", elf_entry);
        }
    } else {
        /* Open and load the "bios", which is the ROM binary, also named "first stage bootloader" */
        char *rom_binary = qemu_find_file(QEMU_FILE_TYPE_BIOS, "esp32c3-rom.bin");
        if (rom_binary == NULL) {
            error_report("Error: -bios argument not set, and ROM code binary not found (1)");
            exit(1);
        }

        /* Load ROM file at the reset address */
        int size = load_image_targphys_as(rom_binary, ESP32C3_RESET_ADDRESS, 0x60000, CPU(&ms->soc)->as, NULL);
        if (size < 0) {
            error_report("Error: could not load ROM binary '%s'", rom_binary);
            exit(1);
        }

        g_free(rom_binary);
    }
}


static void x3_board_init(Esp32C3MachineState *ms);

static void esp32c3_machine_init(MachineState *machine)
{
    /* First thing to do is to check if a drive format and a file ahve been passed through the command line.
     * In fact, we will emulate the SPI flash if `if=mtd` was given. To know this, we will need to use the
     * Global API's function `driver_get`. */
    BlockBackend* blk = NULL;
    DriveInfo *dinfo = drive_get(IF_MTD, 0, 0);
    if (dinfo) {
        /* MTD was given! We need to initialize and emulate SPI flash */
        qemu_log("Adding SPI flash device\n");
        blk = blk_by_legacy_dinfo(dinfo);
    } else {
        qemu_log("Not initializing SPI Flash\n");
    }

    /* Re-use the macro that checks and casts any generic/parent class to the real child instance */
    Esp32C3MachineState *ms = ESP32C3_MACHINE(machine);

    /* Initialize SoC */
    object_initialize_child(OBJECT(ms), "soc", &ms->soc, TYPE_ESP_RISCV_CPU);
    if (object_dynamic_cast(OBJECT(machine), TYPE_X3_MACHINE)) {
        const struct {
            const char *name;
            uint8_t ns;
        } cpu_costs[] = {
            { "cost-rom-ns", 6 },
            { "cost-sram-ns", 8 },
            { "cost-flash-ns", 10 },
            { "cost-load", 0 },
            { "cost-store", 3 },
            { "cost-mul", 0 },
            { "cost-div", 160 },
            { "cost-branch", 2 },
        };

        /* X3 uses -icount shift=0: costs are nanoseconds at 160 MHz. */
        for (size_t i = 0; i < ARRAY_SIZE(cpu_costs); i++) {
            if (!qdev_find_global_prop(OBJECT(&ms->soc), cpu_costs[i].name)) {
                qdev_prop_set_uint8(DEVICE(&ms->soc), cpu_costs[i].name,
                                    cpu_costs[i].ns);
            }
        }
    }
    qdev_prop_set_uint64(DEVICE(&ms->soc), "resetvec", ESP32C3_RESET_ADDRESS);

    /* Initialize the memory mapping */
    const struct MemmapEntry *memmap = esp32c3_memmap;
    MemoryRegion *sys_mem = get_system_memory();

    /* Initialize the IROM */
    MemoryRegion *irom = g_new(MemoryRegion, 1);
    memory_region_init_rom(irom, NULL, "esp32c3.irom", memmap[ESP32C3_MEMREGION_IROM].size, &error_fatal);
    memory_region_add_subregion(sys_mem, memmap[ESP32C3_MEMREGION_IROM].base, irom);

    /* Initialize the DROM as an alias to IROM. */
    MemoryRegion *drom = g_new(MemoryRegion, 1);
    const hwaddr offset_in_orig = 0x40000;
    memory_region_init_alias(drom, NULL, "esp32c3.drom", irom, offset_in_orig, memmap[ESP32C3_MEMREGION_DROM].size);
    memory_region_add_subregion(sys_mem, memmap[ESP32C3_MEMREGION_DROM].base, drom);

    /* Initialize the IRAM */
    MemoryRegion *iram = g_new(MemoryRegion, 1);
    memory_region_init_ram(iram, NULL, "esp32c3.iram", memmap[ESP32C3_MEMREGION_IRAM].size, &error_fatal);
    memory_region_add_subregion(sys_mem, memmap[ESP32C3_MEMREGION_IRAM].base, iram);

    /* Initialize DRAM as an alias to IRAM (not including Internal SRAM 0) */
    MemoryRegion *dram = g_new(MemoryRegion, 1);
    /* DRAM mirrors IRAM for SRAM 1, skip the SRAM 0 area */
    memory_region_init_alias(dram, NULL, "esp32c3.dram", iram,
                             ESP32C3_INTERNAL_SRAM0_SIZE, memmap[ESP32C3_MEMREGION_DRAM].size);
    memory_region_add_subregion(sys_mem, memmap[ESP32C3_MEMREGION_DRAM].base, dram);

    /* Initialize RTC Fast Memory as regular RAM */
    MemoryRegion *rtcram = g_new(MemoryRegion, 1);
    memory_region_init_ram(rtcram, NULL, "esp32c3.rtcram", memmap[ESP32C3_MEMREGION_RTCFAST].size, &error_fatal);
    memory_region_add_subregion(sys_mem, memmap[ESP32C3_MEMREGION_RTCFAST].base, rtcram);

    esp32c3_load_firmware(machine);

    qdev_realize(DEVICE(&ms->soc), NULL, &error_fatal);

    memory_region_init_io(&ms->iomem, OBJECT(&ms->soc), &esp32c3_io_ops,
                          NULL, "esp32c3.iomem", 0xd1000);
    memory_region_add_subregion(sys_mem, ESP32C3_IO_START_ADDR, &ms->iomem);


    /* Initialize the peripheral bus */
    qbus_init(&ms->periph_bus, sizeof(ms->periph_bus),
              TYPE_SYSTEM_BUS, DEVICE(&ms->soc), "esp32c3-periph-bus");

    /* Initialize the main I/O of the CPU that waits for "reset" requests */
    qdev_init_gpio_in_named(DEVICE(&ms->soc), esp32c3_reset_request, ESP32C3_RESET_GPIO_NAME, 1);

    /* Initialize the I/O peripherals */
    for (int i = 0; i < ESP32C3_UART_COUNT; ++i) {
        char name[16];
        snprintf(name, sizeof(name), "uart%d", i);
        object_initialize_child(OBJECT(machine), name, &ms->uart[i], TYPE_ESP32C3_UART);

        snprintf(name, sizeof(name), "serial%d", i);
        object_property_add_alias(OBJECT(machine), name, OBJECT(&ms->uart[i]), "chardev");
        qdev_prop_set_chr(DEVICE(&ms->uart[i]), "chardev", serial_hd(i));
    }

    object_initialize_child(OBJECT(machine), "intmatrix", &ms->intmatrix, TYPE_ESP32C3_INTMATRIX);
    object_initialize_child(OBJECT(machine), "gpio", &ms->gpio, TYPE_ESP32S3_GPIO);
    qdev_prop_set_uint32(DEVICE(&ms->gpio), "strap_mode", ESP32C3_STRAP_MODE_FLASH_BOOT);
    object_initialize_child(OBJECT(machine), "extmem", &ms->cache, TYPE_ESP32C3_CACHE);
    object_initialize_child(OBJECT(machine), "efuse", &ms->efuse, TYPE_ESP32C3_EFUSE);
    object_initialize_child(OBJECT(machine), "clock", &ms->clock, TYPE_ESP32C3_CLOCK);
    object_initialize_child(OBJECT(machine), "sha", &ms->sha, TYPE_ESP32C3_SHA);
    object_initialize_child(OBJECT(machine), "aes", &ms->aes, TYPE_ESP32C3_AES);
    object_initialize_child(OBJECT(machine), "gdma", &ms->gdma, TYPE_ESP32C3_GDMA);
    object_initialize_child(OBJECT(machine), "rsa", &ms->rsa, TYPE_ESP32C3_RSA);
    object_initialize_child(OBJECT(machine), "hmac", &ms->hmac, TYPE_ESP32C3_HMAC);
    object_initialize_child(OBJECT(machine), "ds", &ms->ds, TYPE_ESP32C3_DS);
    object_initialize_child(OBJECT(machine), "xts_aes", &ms->xts_aes, TYPE_ESP32C3_XTS_AES);
    object_initialize_child(OBJECT(machine), "timg0", &ms->timg[0], TYPE_ESP32C3_TIMG);
    object_initialize_child(OBJECT(machine), "timg1", &ms->timg[1], TYPE_ESP32C3_TIMG);
    object_initialize_child(OBJECT(machine), "systimer", &ms->systimer, TYPE_ESP32C3_SYSTIMER);
    object_initialize_child(OBJECT(machine), "spi1", &ms->spi1, TYPE_ESP32C3_SPI);
    object_initialize_child(OBJECT(machine), "rtccntl", &ms->rtccntl, TYPE_ESP32C3_RTC_CNTL);
    ms->jtag = qdev_new("misc.esp32s3.usb_serial_jtag");
    object_property_add_child(OBJECT(machine), "jtag", OBJECT(ms->jtag));
    object_initialize_child(OBJECT(machine), "rgb", &ms->rgb, TYPE_ESP_RGB);
    object_initialize_child(OBJECT(machine), "twai", &ms->twai, TYPE_ESP32C3_TWAI);

    /* Realize all the I/O peripherals we depend on */

    /* Interrupt matrix realization */
    DeviceState* intmatrix_dev = DEVICE(&ms->intmatrix);
    {
        /* Store the current Machine CPU in the interrupt matrix */
        object_property_set_link(OBJECT(&ms->intmatrix), "cpu", OBJECT(&ms->soc), &error_abort);
        sysbus_realize(SYS_BUS_DEVICE(&ms->intmatrix), &error_fatal);
        MemoryRegion *mr = sysbus_mmio_get_region(SYS_BUS_DEVICE(&ms->intmatrix), 0);
        memory_region_add_subregion_overlap(sys_mem, DR_REG_INTERRUPT_BASE, mr, 0);

        /* Connect all the interrupt matrix 31 output lines to the CPU 31 input IRQ lines.
         * The lines are indexed starting at 1.
         */
        for (int i = 0; i <= ESP32C3_CPU_INT_COUNT; i++) {
            qemu_irq cpu_input = qdev_get_gpio_in_named(DEVICE(&ms->soc), ESP_CPU_IRQ_LINES_NAME, i);
            qdev_connect_gpio_out_named(intmatrix_dev, ESP32C3_INT_MATRIX_OUTPUT_NAME, i, cpu_input);
        }
    }

    /* Initialize OpenCores Ethernet controller now sicne it requires the interrupt matrix */
    esp32c3_init_openeth(ms);

    /* USB Serial JTAG realization */
    {
        sysbus_realize_and_unref(SYS_BUS_DEVICE(ms->jtag), &error_fatal);
        MemoryRegion *mr = sysbus_mmio_get_region(SYS_BUS_DEVICE(ms->jtag), 0);
        memory_region_add_subregion_overlap(sys_mem, DR_REG_USB_SERIAL_JTAG_BASE, mr, 1);
        sysbus_connect_irq(SYS_BUS_DEVICE(ms->jtag), 0,
                           qdev_get_gpio_in(intmatrix_dev, ETS_USB_SERIAL_JTAG_INTR_SOURCE));
    }

    /* RTC CNTL realization */
    {
        sysbus_realize(SYS_BUS_DEVICE(&ms->rtccntl), &error_fatal);
        MemoryRegion *mr = sysbus_mmio_get_region(SYS_BUS_DEVICE(&ms->rtccntl), 0);
        memory_region_add_subregion_overlap(sys_mem, DR_REG_RTCCNTL_BASE, mr, 0);
        /* Connect CNTL's reset-request GPIO to the SoC's reset GPIO */
        qdev_connect_gpio_out_named(DEVICE(&ms->rtccntl), ESP32C3_RTC_CPU_RESET_GPIO, 0,
                                    qdev_get_gpio_in_named(DEVICE(&ms->soc), ESP32C3_RESET_GPIO_NAME, 0));
        sysbus_connect_irq(SYS_BUS_DEVICE(&ms->rtccntl), 0,
                           qdev_get_gpio_in(intmatrix_dev, ETS_RTC_CORE_INTR_SOURCE));
    }

    /* SPI1 controller (SPI Flash) */
    {
        ms->spi1.xts_aes = &ms->xts_aes;
        sysbus_realize(SYS_BUS_DEVICE(&ms->spi1), &error_fatal);
        MemoryRegion *mr = sysbus_mmio_get_region(SYS_BUS_DEVICE(&ms->spi1), 0);
        memory_region_add_subregion_overlap(sys_mem, DR_REG_SPI1_BASE, mr, 0);
        if (blk) {
            esp32c3_init_spi_flash(ms, blk);
        }
    }

    for (int i = 0; i < ESP32C3_UART_COUNT; ++i) {
        const hwaddr uart_base[] = { DR_REG_UART_BASE, DR_REG_UART1_BASE };
        sysbus_realize(SYS_BUS_DEVICE(&ms->uart[i]), &error_fatal);
        MemoryRegion *mr = sysbus_mmio_get_region(SYS_BUS_DEVICE(&ms->uart[i]), 0);
        memory_region_add_subregion_overlap(sys_mem, uart_base[i], mr, 0);
        sysbus_connect_irq(SYS_BUS_DEVICE(&ms->uart[i]), 0,
                           qdev_get_gpio_in(intmatrix_dev, ETS_UART0_INTR_SOURCE + i));
    }

    /* GPIO realization */
    {
        sysbus_realize(SYS_BUS_DEVICE(&ms->gpio), &error_fatal);
        MemoryRegion *mr = sysbus_mmio_get_region(SYS_BUS_DEVICE(&ms->gpio), 0);
        memory_region_add_subregion_overlap(sys_mem, DR_REG_GPIO_BASE, mr, 1);
        sysbus_connect_irq(SYS_BUS_DEVICE(&ms->gpio), 0,
                           qdev_get_gpio_in(intmatrix_dev, ETS_GPIO_INTR_SOURCE));
        qdev_connect_gpio_out_named(DEVICE(&ms->gpio), ESP32S3_GPIO_WAKE, 0,
                                    qdev_get_gpio_in_named(DEVICE(&ms->rtccntl), ESP32C3_RTC_GPIO_WAKE, 0));
    }

    /* eink-qemu: GPSPI2, I2C0 and the SAR ADC (the S3 models fit the C3's register maps) */
    {
        ms->spi2 = qdev_new("ssi.esp32s3.gpspi");
        object_property_set_link(OBJECT(ms->spi2), "gdma", OBJECT(&ms->gdma),
                                 &error_fatal);
        if (object_dynamic_cast(OBJECT(machine), TYPE_X3_MACHINE)) {
            Object *spi = OBJECT(ms->spi2);

            /* Residual setup after charging the calibrated CPU driver time. */
            if (!qdev_find_global_prop(spi, "transaction-overhead-us") &&
                !qdev_find_global_prop(spi, "transaction-overhead-ns")) {
                qdev_prop_set_uint32(ms->spi2, "transaction-overhead-ns", 700);
            }
            if (!qdev_find_global_prop(spi, "buffer-overhead-ns")) {
                qdev_prop_set_uint32(ms->spi2, "buffer-overhead-ns", 300);
            }
        }
        object_property_add_child(OBJECT(machine), "spi2", OBJECT(ms->spi2));
        sysbus_realize_and_unref(SYS_BUS_DEVICE(ms->spi2), &error_fatal);
        memory_region_add_subregion_overlap(sys_mem, DR_REG_SPI2_BASE,
                                            sysbus_mmio_get_region(SYS_BUS_DEVICE(ms->spi2), 0), 1);
        sysbus_connect_irq(SYS_BUS_DEVICE(ms->spi2), 0,
                           qdev_get_gpio_in(intmatrix_dev, ETS_SPI2_INTR_SOURCE));

        ms->i2c0 = qdev_new("esp32s3.i2c");
        object_property_add_child(OBJECT(machine), "i2c0", OBJECT(ms->i2c0));
        sysbus_realize_and_unref(SYS_BUS_DEVICE(ms->i2c0), &error_fatal);
        memory_region_add_subregion_overlap(sys_mem, DR_REG_I2C_EXT_BASE,
                                            sysbus_mmio_get_region(SYS_BUS_DEVICE(ms->i2c0), 0), 1);
        sysbus_connect_irq(SYS_BUS_DEVICE(ms->i2c0), 0,
                           qdev_get_gpio_in(intmatrix_dev, ETS_I2C_EXT0_INTR_SOURCE));

        ms->saradc = qdev_new("misc.esp32c3.saradc");
        object_property_add_child(OBJECT(machine), "saradc", OBJECT(ms->saradc));
        sysbus_realize_and_unref(SYS_BUS_DEVICE(ms->saradc), &error_fatal);
        memory_region_add_subregion_overlap(sys_mem, DR_REG_APB_SARADC_BASE,
                                            sysbus_mmio_get_region(SYS_BUS_DEVICE(ms->saradc), 0), 1);
        sysbus_connect_irq(SYS_BUS_DEVICE(ms->saradc), 0,
                           qdev_get_gpio_in(intmatrix_dev, ETS_APB_ADC_INTR_SOURCE));
    }

    /* (Extmem) Cache realization */
    {
        if (blk) {
            ms->cache.flash_blk = blk;
        }
        ms->cache.xts_aes = &ms->xts_aes;
        sysbus_realize(SYS_BUS_DEVICE(&ms->cache), &error_fatal);
        MemoryRegion *mr = sysbus_mmio_get_region(SYS_BUS_DEVICE(&ms->cache), 0);
        memory_region_add_subregion_overlap(sys_mem, DR_REG_EXTMEM_BASE, mr, 0);

        memory_region_add_subregion_overlap(sys_mem, ms->cache.dcache_base, &ms->cache.dcache, 0);
        memory_region_add_subregion_overlap(sys_mem, ms->cache.icache_base, &ms->cache.icache, 0);
    }

    /* eFuses realization */
    {
        sysbus_realize(SYS_BUS_DEVICE(&ms->efuse), &error_fatal);
        MemoryRegion *mr = sysbus_mmio_get_region(SYS_BUS_DEVICE(&ms->efuse), 0);
        memory_region_add_subregion_overlap(sys_mem, DR_REG_EFUSE_BASE, mr, 0);
        sysbus_connect_irq(SYS_BUS_DEVICE(&ms->efuse), 0,
                       qdev_get_gpio_in(intmatrix_dev, ETS_EFUSE_INTR_SOURCE));
    }

    /* System clock realization */
    {
        object_property_set_link(OBJECT(&ms->clock), "cpu",
                                 OBJECT(&ms->soc), &error_abort);
        sysbus_realize(SYS_BUS_DEVICE(&ms->clock), &error_fatal);
        MemoryRegion *mr = sysbus_mmio_get_region(SYS_BUS_DEVICE(&ms->clock), 0);
        memory_region_add_subregion_overlap(sys_mem, DR_REG_SYSTEM_BASE, mr, 0);
        /* Connect the IRQ lines to the interrupt matrix */
        for (int i = 0; i < ESP32C3_SYSTEM_CPU_INTR_COUNT; i++) {
            sysbus_connect_irq(SYS_BUS_DEVICE(&ms->clock), i,
                           qdev_get_gpio_in(intmatrix_dev, ETS_FROM_CPU_INTR0_SOURCE + i));
        }
    }

    /* Timer Groups realization */
    {
        sysbus_realize(SYS_BUS_DEVICE(&ms->timg[0]), &error_fatal);
        MemoryRegion *mr = sysbus_mmio_get_region(SYS_BUS_DEVICE(&ms->timg[0]), 0);
        memory_region_add_subregion_overlap(sys_mem, DR_REG_TIMERGROUP0_BASE, mr, 0);
        /* Connect the T0 interrupt line to the interrupt matrix */
        qdev_connect_gpio_out_named(DEVICE(&ms->timg[0]), ESP32C3_T0_IRQ_INTERRUPT, 0,
                                    qdev_get_gpio_in(intmatrix_dev, ETS_TG0_T0_LEVEL_INTR_SOURCE));
        /* Connect the Watchdog interrupt line to the interrupt matrix */
        qdev_connect_gpio_out_named(DEVICE(&ms->timg[0]), ESP32C3_WDT_IRQ_INTERRUPT, 0,
                                    qdev_get_gpio_in(intmatrix_dev, ETS_TG0_WDT_LEVEL_INTR_SOURCE));
        /* Connect the Watchdog reset request to the CNTL's WDT0 line */
        qdev_connect_gpio_out_named(DEVICE(&ms->timg[0]), ESP32C3_WDT_IRQ_RESET, 0,
                                    qdev_get_gpio_in(DEVICE(&ms->rtccntl), ESP32C3_TG0WDT_SYS_RESET));

    }
    {
        sysbus_realize(SYS_BUS_DEVICE(&ms->timg[1]), &error_fatal);
        MemoryRegion *mr = sysbus_mmio_get_region(SYS_BUS_DEVICE(&ms->timg[1]), 0);
        memory_region_add_subregion_overlap(sys_mem, DR_REG_TIMERGROUP1_BASE, mr, 0);
        /* Connect the T0 interrupt line to the interrupt matrix */
        qdev_connect_gpio_out_named(DEVICE(&ms->timg[1]), ESP32C3_T0_IRQ_INTERRUPT, 0,
                                    qdev_get_gpio_in(intmatrix_dev, ETS_TG1_T0_LEVEL_INTR_SOURCE));
        qdev_connect_gpio_out_named(DEVICE(&ms->timg[1]), ESP32C3_WDT_IRQ_INTERRUPT, 0,
                                    qdev_get_gpio_in(intmatrix_dev, ETS_TG1_WDT_LEVEL_INTR_SOURCE));
        qdev_connect_gpio_out_named(DEVICE(&ms->timg[1]), ESP32C3_WDT_IRQ_RESET, 0,
                                    qdev_get_gpio_in(DEVICE(&ms->rtccntl), ESP32C3_TG1WDT_SYS_RESET));
    }

    /* System timer */
    {
        sysbus_realize(SYS_BUS_DEVICE(&ms->systimer), &error_fatal);
        MemoryRegion *mr = sysbus_mmio_get_region(SYS_BUS_DEVICE(&ms->systimer), 0);
        memory_region_add_subregion_overlap(sys_mem, DR_REG_SYSTIMER_BASE, mr, 0);
        for (int i = 0; i < ESP_SYSTIMER_IRQ_COUNT; i++) {
            sysbus_connect_irq(SYS_BUS_DEVICE(&ms->systimer), i,
                           qdev_get_gpio_in(intmatrix_dev, ETS_SYSTIMER_TARGET0_EDGE_INTR_SOURCE + i));
        }
    }

    /* GDMA Realization */
    {
        object_property_set_link(OBJECT(&ms->gdma), "soc_mr", OBJECT(dram), &error_abort);
        sysbus_realize(SYS_BUS_DEVICE(&ms->gdma), &error_fatal);
        MemoryRegion *mr = sysbus_mmio_get_region(SYS_BUS_DEVICE(&ms->gdma), 0);
        memory_region_add_subregion_overlap(sys_mem, DR_REG_GDMA_BASE, mr, 0);
        /* On the ESP32-C3, both IN and OUT channels are connected to the same Connect the IRQs to the Interrupt Matrix */
        for (int i = 0; i < ESP32C3_GDMA_CHANNEL_COUNT; i++) {
            qdev_connect_gpio_out_named(DEVICE(&ms->gdma), ESP_GDMA_IRQ_IN_NAME, i,
                                        qdev_get_gpio_in(intmatrix_dev, ETS_DMA_CH0_INTR_SOURCE + i));
            qdev_connect_gpio_out_named(DEVICE(&ms->gdma), ESP_GDMA_IRQ_OUT_NAME, i,
                                        qdev_get_gpio_in(intmatrix_dev, ETS_DMA_CH0_INTR_SOURCE + i));
        }

    }

    /* SHA realization */
    {
        ms->sha.parent.gdma = ESP_GDMA(&ms->gdma);
        sysbus_realize(SYS_BUS_DEVICE(&ms->sha), &error_fatal);
        MemoryRegion *mr = sysbus_mmio_get_region(SYS_BUS_DEVICE(&ms->sha), 0);
        memory_region_add_subregion_overlap(sys_mem, DR_REG_SHA_BASE, mr, 0);
        sysbus_connect_irq(SYS_BUS_DEVICE(&ms->sha), 0,
                           qdev_get_gpio_in(intmatrix_dev, ETS_SHA_INTR_SOURCE));
    }

    /* AES realization */
    {
        ms->aes.parent.gdma = ESP_GDMA(&ms->gdma);
        sysbus_realize(SYS_BUS_DEVICE(&ms->aes), &error_fatal);
        MemoryRegion *mr = sysbus_mmio_get_region(SYS_BUS_DEVICE(&ms->aes), 0);
        memory_region_add_subregion_overlap(sys_mem, DR_REG_AES_BASE, mr, 0);
        sysbus_connect_irq(SYS_BUS_DEVICE(&ms->aes), 0,
                           qdev_get_gpio_in(intmatrix_dev, ETS_AES_INTR_SOURCE));
    }

    /* RSA realization */
    {
        sysbus_realize(SYS_BUS_DEVICE(&ms->rsa), &error_fatal);
        MemoryRegion *mr = sysbus_mmio_get_region(SYS_BUS_DEVICE(&ms->rsa), 0);
        memory_region_add_subregion_overlap(sys_mem, DR_REG_RSA_BASE, mr, 0);
        sysbus_connect_irq(SYS_BUS_DEVICE(&ms->rsa), 0,
                           qdev_get_gpio_in(intmatrix_dev, ETS_RSA_INTR_SOURCE));
    }

    /* HMAC realization */
    {
        ms->hmac.parent.efuse = ESP_EFUSE(&ms->efuse);
        qdev_realize(DEVICE(&ms->hmac), &ms->periph_bus, &error_fatal);
        MemoryRegion *mr = sysbus_mmio_get_region(SYS_BUS_DEVICE(&ms->hmac), 0);
        memory_region_add_subregion_overlap(sys_mem, DR_REG_HMAC_BASE, mr, 0);
    }

    /* Digital Signature realization */
    {
        ms->ds.parent.hmac = ESP_HMAC(&ms->hmac);
        ms->ds.parent.aes = ESP_AES(&ms->aes);
        ms->ds.parent.rsa = ESP_RSA(&ms->rsa);
        ms->ds.parent.sha = ESP_SHA(&ms->sha);
        qdev_realize(DEVICE(&ms->ds), &ms->periph_bus, &error_fatal);
        MemoryRegion *mr = sysbus_mmio_get_region(SYS_BUS_DEVICE(&ms->ds), 0);
        memory_region_add_subregion_overlap(sys_mem, DR_REG_DIGITAL_SIGNATURE_BASE, mr, 0);
    }

    /* XTS-AES realization */
    {
        ms->xts_aes.efuse = ESP_EFUSE(&ms->efuse);
        ms->xts_aes.clock = &ms->clock;
        qdev_realize(DEVICE(&ms->xts_aes), &ms->periph_bus, &error_fatal);
        MemoryRegion *mr = sysbus_mmio_get_region(SYS_BUS_DEVICE(&ms->xts_aes), 0);
        memory_region_add_subregion_overlap(sys_mem, DR_REG_AES_XTS_BASE, mr, 0);
    }

    /* RGB display realization */
    {
        /* Give the internal RAM memory region to the display */
        ms->rgb.intram = dram;
        sysbus_realize(SYS_BUS_DEVICE(&ms->rgb), &error_fatal);
        MemoryRegion *mr = sysbus_mmio_get_region(SYS_BUS_DEVICE(&ms->rgb), 0);
        memory_region_add_subregion_overlap(sys_mem, DR_REG_FRAMEBUF_BASE, mr, 0);
        memory_region_add_subregion_overlap(sys_mem, esp32c3_memmap[ESP32C3_MEMREGION_FRAMEBUF].base, &ms->rgb.vram, 0);
    }

    /* TWAI peripheral realization */
    sysbus_realize(SYS_BUS_DEVICE(&ms->twai), &error_fatal);
    MemoryRegion *twai_mr = sysbus_mmio_get_region(SYS_BUS_DEVICE(&ms->twai), 0);
    memory_region_add_subregion_overlap(sys_mem, DR_REG_TWAI_BASE, twai_mr, 0);
    sysbus_connect_irq(SYS_BUS_DEVICE(&ms->twai), 0,
                       qdev_get_gpio_in(DEVICE(&ms->intmatrix), ETS_TWAI_INTR_SOURCE));

    /*
     * eink-qemu: radio, as on the S3 machine (the models came from an ESP32-C3
     * port, so the register bases are the C3's). SYSCON's clock/reset enables
     * read back (the PHY asserts on them; the stub stops before the RNG
     * register at 0xB0 and the "QEMU" origin register); the Wi-Fi MAC status
     * bit lets hal_init through; ana (regi2c) and fe answer the PHY
     * calibration. With -nic user,model=esp32_wifi the MAC is emulated with
     * the fake AP "PICSimLabWifi" bridged to that NIC, over the stub.
     */
    {
        static const struct {
            hwaddr base;
            uint32_t size;
            uint32_t off[4], mask[4];
            int n;
        } stubs[] = {
            { DR_REG_SYSCON_BASE, 0x20, { 0 }, { 0 }, 0 },
            { 0x60033000, 0x1000, { 0xD14 }, { 1u << 0 }, 1 },
        };
        for (int i = 0; i < ARRAY_SIZE(stubs); i++) {
            DeviceState *d = qdev_new("misc.esp32s3.regstub");
            QList *offs = qlist_new(), *masks = qlist_new();
            for (int j = 0; j < stubs[i].n; j++) {
                qlist_append_int(offs, stubs[i].off[j]);
                qlist_append_int(masks, stubs[i].mask[j]);
            }
            qdev_prop_set_uint32(d, "size", stubs[i].size);
            qdev_prop_set_array(d, "or-offsets", offs);
            qdev_prop_set_array(d, "or-masks", masks);
            sysbus_realize_and_unref(SYS_BUS_DEVICE(d), &error_fatal);
            memory_region_add_subregion_overlap(sys_mem, stubs[i].base,
                                                sysbus_mmio_get_region(SYS_BUS_DEVICE(d), 0), 1);
        }

        DeviceState *d = qdev_new(TYPE_ESP32S3_ANA);
        sysbus_realize_and_unref(SYS_BUS_DEVICE(d), &error_fatal);
        memory_region_add_subregion_overlap(sys_mem, DR_REG_RTC_I2C_BASE,
                                            sysbus_mmio_get_region(SYS_BUS_DEVICE(d), 0), 1);
        d = qdev_new(TYPE_ESP32_FE);
        sysbus_realize_and_unref(SYS_BUS_DEVICE(d), &error_fatal);
        memory_region_add_subregion_overlap(sys_mem, DR_REG_FE_BASE,
                                            sysbus_mmio_get_region(SYS_BUS_DEVICE(d), 0), 1);

        /*
         * Bluetooth link-layer registers (same block as the S3's) and plain
         * RAM at the exchange-memory window the ROM reserves. The controller
         * keeps its buffers in heap DRAM and tells the hardware where through
         * the mapping table at 0x60031204; nothing translates that yet.
         */
        d = qdev_new("misc.esp32s3.ble");
        sysbus_realize_and_unref(SYS_BUS_DEVICE(d), &error_fatal);
        memory_region_add_subregion_overlap(sys_mem, 0x60031000,
                                            sysbus_mmio_get_region(SYS_BUS_DEVICE(d), 0), 1);
        MemoryRegion *bt_em = g_new(MemoryRegion, 1);
        memory_region_init_ram(bt_em, NULL, "esp32c3.bt-em", 64 * KiB, &error_fatal);
        memory_region_add_subregion(sys_mem, 0x3fc00000, bt_em);

        NICInfo *nd = qemu_find_nic_info(TYPE_ESP32_WIFI, false, NULL);
        if (nd) {
            d = qdev_new(TYPE_ESP32_WIFI);
            /* Station MAC: with no efuse file, burn the NIC's MAC into the in-RAM efuse. */
            ESPEfuseState *ef = &ms->efuse.parent;
            if (ef->mirror) {
                uint8_t *mm = (uint8_t *)&((ESPEfuseBlocks *)ef->mirror)->rd_mac_spi_sys_0;
                for (int i = 0; i < 6; i++) {
                    mm[5 - i] = nd->macaddr.a[i];
                }
            }
            device_cold_reset(DEVICE(&ms->efuse));
            const uint8_t *m = (const uint8_t *)&ef->efuses.blocks.rd_mac_spi_sys_0;
            for (int i = 0; i < 6; i++) {
                ESP32_WIFI(d)->macaddr[i] = m[5 - i];
            }
            qdev_set_nic_properties(d, nd);
            sysbus_realize_and_unref(SYS_BUS_DEVICE(d), &error_fatal);
            memory_region_add_subregion_overlap(sys_mem, 0x60033000,
                                                sysbus_mmio_get_region(SYS_BUS_DEVICE(d), 0), 2);
            sysbus_connect_irq(SYS_BUS_DEVICE(d), 0,
                               qdev_get_gpio_in(intmatrix_dev, ETS_WIFI_MAC_INTR_SOURCE));
        }
    }

    if (object_dynamic_cast(OBJECT(machine), TYPE_X3_MACHINE)) {
        x3_board_init(ms);
    }
}

static DeviceState *x3_add_i2c(I2CBus *bus, const char *type, uint8_t addr)
{
    DeviceState *dev = qdev_new(type);
    qdev_prop_set_uint8(dev, "address", addr);
    qdev_realize_and_unref(dev, BUS(bus), &error_fatal);
    return dev;
}

/*
 * Xteink X3 (freeink-sdk BoardConfig XTEINK_X3): UC8279d/UC8253 panel on
 * GPSPI2 (SCLK 8, SDA 10, CS 21, DC 4, RST 5, BUSY 6), SD card in SPI mode
 * on the same bus (MISO 7, CS 12), keys on an ADC ladder (GPIO1, GPIO2) and
 * Power on GPIO3, BQ27220 / DS3231 / QMI8658 on I2C0 (SDA 20, SCL 0).
 */
static void x3_board_init(Esp32C3MachineState *ms)
{
    DeviceState *gpio = DEVICE(&ms->gpio);
    DeviceState *rtc = DEVICE(&ms->rtccntl);
    SSIBus *bus = (SSIBus *)qdev_get_child_bus(ms->spi2, "spi");
    bool x4 = object_dynamic_cast(OBJECT(ms), TYPE_X4_MACHINE);
    DeviceState *panel = qdev_new(x4 ? "ssd1677" : "uc8279");

    if (!x4 && !qdev_find_global_prop(OBJECT(panel), "uc8253")) {
        qdev_prop_set_bit(panel, "uc8253", true);
    }
    if (!x4 &&
        object_property_get_bool(OBJECT(panel), "uc8253", &error_fatal)) {
        /* Measured UC8253 X3; see eink-qemu/sdcal/panel.md. */
        if (!qdev_find_global_prop(OBJECT(panel), "frame-us")) {
            qdev_prop_set_uint32(panel, "frame-us", 12850);
        }
        if (!qdev_find_global_prop(OBJECT(panel), "refresh-overhead-us")) {
            qdev_prop_set_uint32(panel, "refresh-overhead-us", 138000);
        }
        if (!qdev_find_global_prop(OBJECT(panel), "pon-ms")) {
            qdev_prop_set_uint32(panel, "pon-ms", 127);
        }
    }
    qdev_set_id(panel, g_strdup("panel"), &error_fatal);
    ssi_realize_and_unref(panel, bus, &error_fatal);

    qdev_connect_gpio_out_named(gpio, ESP32S3_GPIO_OUT, 21, qdev_get_gpio_in_named(panel, SSI_GPIO_CS, 0));
    qdev_connect_gpio_out_named(gpio, ESP32S3_GPIO_OUT, 4, qdev_get_gpio_in_named(panel, "dc", 0));
    qdev_connect_gpio_out_named(gpio, ESP32S3_GPIO_OUT, 5, qdev_get_gpio_in_named(panel, "rst", 0));
    qdev_connect_gpio_out_named(panel, "busy", 0, qdev_get_gpio_in_named(gpio, ESP32S3_GPIO_IN, 6));
    qemu_set_irq(qdev_get_gpio_in_named(gpio, ESP32S3_GPIO_IN, 6), !x4);
    if (!x4) {
        qdev_connect_gpio_out_named(gpio, ESP32S3_GPIO_OUT, 8,
                                   qdev_get_gpio_in_named(panel, "sclk", 0));
        qdev_connect_gpio_out_named(gpio, ESP32S3_GPIO_OUT, 10,
                                   qdev_get_gpio_in_named(panel, "sda", 0));
        qdev_connect_gpio_out_named(panel, "sda-out", 0,
                                   qdev_get_gpio_in_named(gpio,
                                                         ESP32S3_GPIO_IN, 10));
        qemu_set_irq(qdev_get_gpio_in_named(gpio, ESP32S3_GPIO_IN, 10), 1);
    }

    DeviceState *sd = qdev_new("ssi-sd");
    /* X3 card probe medians: see eink-qemu/sdcal/cpu.md. */
    const struct {
        const char *name;
        int32_t us;
    } sd_timings[] = {
        { "read-access-us", 207 },
        { "read-seq-access-us", 136 },
        { "read-repeat-us", 108 },
        { "read-next-us", 9 },
        { "write-busy-us", 554 },
        { "write-random-busy-us", 621 },
        { "write-repeat-busy-us", 479 },
        { "write-block-busy-us", 10 },
        { "write-stop-busy-us", 480 },
        { "write-stop-decrement-us", -40 },
        { "write-stop-random-extra-us", 100 },
    };

    for (size_t i = 0; i < ARRAY_SIZE(sd_timings); i++) {
        /* qdev_new already applied -global: preserve explicit overrides. */
        if (!qdev_find_global_prop(OBJECT(sd), sd_timings[i].name)) {
            if (!strcmp(sd_timings[i].name, "write-stop-decrement-us")) {
                qdev_prop_set_int32(sd, sd_timings[i].name, sd_timings[i].us);
            } else {
                qdev_prop_set_uint32(sd, sd_timings[i].name, sd_timings[i].us);
            }
        }
    }
    qdev_prop_set_uint8(sd, "cs", 1);       /* the SSI bus wants distinct CS indexes */
    ssi_realize_and_unref(sd, bus, &error_fatal);
    qdev_connect_gpio_out_named(gpio, ESP32S3_GPIO_OUT, 12, qdev_get_gpio_in_named(sd, SSI_GPIO_CS, 0));
    DriveInfo *dinfo = drive_get(IF_SD, 0, 0);
    DeviceState *card = qdev_new(TYPE_SD_CARD_SPI);
    qdev_prop_set_drive_err(card, "drive", dinfo ? blk_by_legacy_dinfo(dinfo) : NULL, &error_fatal);
    qdev_realize_and_unref(card, qdev_get_child_bus(sd, "sd-bus"), &error_fatal);

    DeviceState *keys = qdev_new("x3-keys");
    object_property_add_child(qdev_get_machine(), "x3-keys", OBJECT(keys));
    qdev_connect_gpio_out_named(keys, "adc", 0, qdev_get_gpio_in_named(ms->saradc, "adc1", 1));
    qdev_connect_gpio_out_named(keys, "adc", 1, qdev_get_gpio_in_named(ms->saradc, "adc1", 2));
    qdev_connect_gpio_out_named(keys, "power", 0, qdev_get_gpio_in_named(gpio, ESP32S3_GPIO_IN, 3));
    qdev_connect_gpio_out_named(keys, "power", 1, qdev_get_gpio_in_named(rtc, ESP32C3_RTC_PAD, 3));
    /* Power is held after a cold power-on, or after a wake by the Power pad. */
    for (int i = 0; i < 2; i++) {
        qdev_connect_gpio_out_named(rtc, ESP32C3_RTC_BOOT, i, qdev_get_gpio_in_named(keys, "boot", i));
    }
    sysbus_realize_and_unref(SYS_BUS_DEVICE(keys), &error_fatal);

    if (x4) {
        /* GPIO20 senses USB power; the emulated board starts on battery. */
        qemu_set_irq(qdev_get_gpio_in_named(gpio, ESP32S3_GPIO_IN, 20), 0);
        return;
    }
    I2CBus *i2c = I2C_BUS(qdev_get_child_bus(ms->i2c0, "i2c"));
    x3_add_i2c(i2c, "bq27220", 0x55);
    x3_add_i2c(i2c, "ds3231", 0x68);
    x3_add_i2c(i2c, "qmi8658", 0x6b);
}


/* Initialize machine type */
static void esp32c3_machine_class_init(ObjectClass *oc, const void *data)
{
    MachineClass *mc = MACHINE_CLASS(oc);
    mc->desc = "Espressif ESP32-C3 machine";
    mc->default_cpu_type = TYPE_ESP_RISCV_CPU;
    mc->init = esp32c3_machine_init;
    mc->max_cpus = 1;
    mc->default_cpus = 1;
    // 0x4f600
    mc->default_ram_size = 400 * 1024;
}

static void x3_machine_class_init(ObjectClass *oc, const void *data)
{
    MachineClass *mc = MACHINE_CLASS(oc);
    mc->desc = "Xteink X3 (ESP32-C3, UC8279d e-paper)";
}

static const TypeInfo x3_info = {
    .name = TYPE_X3_MACHINE,
    .parent = TYPE_ESP32C3_MACHINE,
    .class_init = x3_machine_class_init,
};

/* The C3 X4 shares SPI, SD and keys with X3, but uses an SSD1677 panel. */
static void x4_machine_class_init(ObjectClass *oc, const void *data)
{
    MachineClass *mc = MACHINE_CLASS(oc);
    mc->desc = "Xteink X4 (ESP32-C3, SSD1677 e-paper)";
}

static const TypeInfo x4_info = {
    .name = TYPE_X4_MACHINE,
    .parent = TYPE_X3_MACHINE,
    .class_init = x4_machine_class_init,
};

/* Create a new type of machine ("child class") */
static const TypeInfo esp32c3_info = {
    .name = TYPE_ESP32C3_MACHINE,
    /* Specify the parent class, i.e. the class we derivate from */
    .parent = TYPE_MACHINE,
    .interfaces = riscv32_machine_interfaces,
    /* Real size in bytes of our machine instance */
    .instance_size = sizeof(Esp32C3MachineState),
    /* Override the init function to one we defined above */
    .class_init = esp32c3_machine_class_init,
};

static void esp32c3_machine_type_init(void)
{
    type_register_static(&esp32c3_info);
    type_register_static(&x3_info);
    type_register_static(&x4_info);
}

type_init(esp32c3_machine_type_init);
