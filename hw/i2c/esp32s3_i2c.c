/*
 * ESP32-S3 I2C master controller (I2C0/I2C1)
 *
 * Models the registers the IDF i2c_master driver touches: CTR (TRANS_START,
 * FSM_RST, CONF_UPGATE), SR (BUS_BUSY, FIFO counts), FIFO_CONF resets, the
 * DATA FIFO port, INT_RAW/CLR/ENA/STATUS and the eight COMD registers.
 * Timing, filter and clock registers are stored and read back but have no
 * effect. A command list runs to completion inside the TRANS_START write and
 * raises END_DETECT, TRANS_COMPLETE or NACK at once; there is no bus timing,
 * no timeout, no arbitration and no slave mode.
 *
 * This program is free software; you can redistribute it and/or modify
 * it under the terms of the GNU General Public License version 2 or
 * (at your option) any later version.
 */

#include "qemu/osdep.h"
#include "qemu/log.h"
#include "qemu/module.h"
#include "qemu/fifo8.h"
#include "hw/core/sysbus.h"
#include "hw/core/irq.h"
#include "hw/i2c/i2c.h"
#include "hw/core/registerfields.h"

#define TYPE_ESP32S3_I2C "esp32s3.i2c"
OBJECT_DECLARE_SIMPLE_TYPE(ESP32S3I2CState, ESP32S3_I2C)

#define ESP32S3_I2C_IO_SIZE     0x1000
#define ESP32S3_I2C_NREGS       (0x100 / 4)
#define ESP32S3_I2C_FIFO_LEN    32
#define ESP32S3_I2C_CMD_COUNT   8

REG32(CTR, 0x04)
    FIELD(CTR, TRANS_START, 5, 1)
    FIELD(CTR, FSM_RST, 10, 1)
    FIELD(CTR, CONF_UPGATE, 11, 1)
REG32(SR, 0x08)
    FIELD(SR, RESP_REC, 0, 1)
    FIELD(SR, BUS_BUSY, 4, 1)
    FIELD(SR, RXFIFO_CNT, 8, 6)
    FIELD(SR, TXFIFO_CNT, 18, 6)
REG32(FIFO_CONF, 0x18)
    FIELD(FIFO_CONF, RX_FIFO_RST, 12, 1)
    FIELD(FIFO_CONF, TX_FIFO_RST, 13, 1)
REG32(DATA, 0x1c)
REG32(INT_RAW, 0x20)
    FIELD(INT, END_DETECT, 3, 1)
    FIELD(INT, TRANS_COMPLETE, 7, 1)
    FIELD(INT, NACK, 10, 1)
REG32(INT_CLR, 0x24)
REG32(INT_ENA, 0x28)
REG32(INT_STATUS, 0x2c)
REG32(COMD0, 0x58)
    FIELD(COMD, BYTE_NUM, 0, 8)
    FIELD(COMD, ACK_CHECK_EN, 8, 1)
    FIELD(COMD, ACK_EXP, 9, 1)
    FIELD(COMD, ACK_VALUE, 10, 1)
    FIELD(COMD, OPCODE, 11, 3)
    FIELD(COMD, DONE, 31, 1)
REG32(SCL_SP_CONF, 0x80)
    FIELD(SCL_SP_CONF, SCL_RST_SLV_EN, 0, 1)
REG32(DATE, 0xf8)

/* S3 opcodes differ from the ESP32 ones */
enum {
    OP_WRITE = 1,
    OP_STOP = 2,
    OP_READ = 3,
    OP_END = 4,
    OP_RSTART = 6,
};

struct ESP32S3I2CState {
    SysBusDevice parent_obj;

    MemoryRegion iomem;
    qemu_irq irq;
    I2CBus *bus;
    Fifo8 tx_fifo;
    Fifo8 rx_fifo;
    bool bus_held;      /* START sent, no STOP yet */
    bool addr_next;     /* next WRITE byte is an address byte */
    bool resp_rec;      /* response received from the slave */
    uint8_t addr;       /* 7-bit address of the current transfer */
    uint32_t regs[ESP32S3_I2C_NREGS];
};

static void esp32s3_i2c_update_irq(ESP32S3I2CState *s)
{
    qemu_set_irq(s->irq, !!(s->regs[R_INT_RAW] & s->regs[R_INT_ENA]));
}

static void esp32s3_i2c_release(ESP32S3I2CState *s)
{
    if (s->bus_held) {
        i2c_end_transfer(s->bus);
        s->bus_held = false;
    }
}

/* Address phase: returns true if the slave ACKed */
static bool esp32s3_i2c_address(ESP32S3I2CState *s, uint8_t byte)
{
    uint8_t addr = byte >> 1;

    /* A repeated start to the same slave keeps it selected; QEMU only rescans
     * the bus when no transfer is in progress. */
    if (s->bus_held && addr != s->addr) {
        esp32s3_i2c_release(s);
    }
    s->addr = addr;
    s->bus_held = i2c_start_transfer(s->bus, addr, byte & 1) == 0;
    return s->bus_held;
}

static void esp32s3_i2c_run(ESP32S3I2CState *s)
{
    for (int i = 0; i < ESP32S3_I2C_CMD_COUNT; i++) {
        uint32_t *cmd = &s->regs[R_COMD0 + i];
        unsigned n = FIELD_EX32(*cmd, COMD, BYTE_NUM);
        bool ack_check = FIELD_EX32(*cmd, COMD, ACK_CHECK_EN);
        bool ack_exp = FIELD_EX32(*cmd, COMD, ACK_EXP);
        bool nacked = false;

        *cmd = FIELD_DP32(*cmd, COMD, DONE, 1);
        switch (FIELD_EX32(*cmd, COMD, OPCODE)) {
        case OP_RSTART:
            s->addr_next = true;
            break;
        case OP_WRITE:
            for (unsigned k = 0; k < n && !nacked; k++) {
                bool ack;
                uint8_t byte;

                if (fifo8_is_empty(&s->tx_fifo)) {
                    qemu_log_mask(LOG_GUEST_ERROR, "esp32s3.i2c: TX FIFO underflow\n");
                    break;
                }
                byte = fifo8_pop(&s->tx_fifo);
                if (s->addr_next) {
                    s->addr_next = false;
                    ack = esp32s3_i2c_address(s, byte);
                } else {
                    ack = s->bus_held && i2c_send(s->bus, byte) == 0;
                }
                s->resp_rec = ack;
                /* ACK_EXP is the expected SDA level: 0 = ACK, 1 = NACK */
                nacked = ack_check && !ack != ack_exp;
            }
            break;
        case OP_READ:
            for (unsigned k = 0; k < n; k++) {
                uint8_t byte = s->bus_held ? i2c_recv(s->bus) : 0xff;

                if (fifo8_is_full(&s->rx_fifo)) {
                    qemu_log_mask(LOG_GUEST_ERROR, "esp32s3.i2c: RX FIFO overflow\n");
                } else {
                    fifo8_push(&s->rx_fifo, byte);
                }
            }
            if (s->bus_held && FIELD_EX32(*cmd, COMD, ACK_VALUE)) {
                i2c_nack(s->bus);
            }
            break;
        case OP_STOP:
            esp32s3_i2c_release(s);
            s->regs[R_INT_RAW] |= R_INT_TRANS_COMPLETE_MASK;
            return;
        case OP_END:
            s->regs[R_INT_RAW] |= R_INT_END_DETECT_MASK;
            return;
        default:
            qemu_log_mask(LOG_GUEST_ERROR, "esp32s3.i2c: bad opcode in COMD%d: 0x%08x\n",
                          i, *cmd);
            return;
        }
        if (nacked) {
            /* ACK mismatch generates an automatic STOP (S3 TRM 27.4.9).
             * STOP detection also raises TRANS_COMPLETE (27.6), allowing
             * the driver to finish receive bookkeeping before the next job. */
            esp32s3_i2c_release(s);
            s->regs[R_INT_RAW] |= R_INT_NACK_MASK | R_INT_TRANS_COMPLETE_MASK;
            return;
        }
    }
}

static uint64_t esp32s3_i2c_read(void *opaque, hwaddr addr, unsigned int size)
{
    ESP32S3I2CState *s = ESP32S3_I2C(opaque);

    switch (addr) {
    case A_SR: {
        uint32_t sr = 0;
        sr = FIELD_DP32(sr, SR, RESP_REC, s->resp_rec);
        sr = FIELD_DP32(sr, SR, BUS_BUSY, s->bus_held);
        sr = FIELD_DP32(sr, SR, RXFIFO_CNT, fifo8_num_used(&s->rx_fifo));
        sr = FIELD_DP32(sr, SR, TXFIFO_CNT, fifo8_num_used(&s->tx_fifo));
        return sr;
    }
    case A_DATA:
        if (fifo8_is_empty(&s->rx_fifo)) {
            qemu_log_mask(LOG_GUEST_ERROR, "esp32s3.i2c: RX FIFO read while empty\n");
            return 0;
        }
        return fifo8_pop(&s->rx_fifo);
    case A_INT_STATUS:
        return s->regs[R_INT_RAW] & s->regs[R_INT_ENA];
    case A_INT_CLR:
        return 0;
    default:
        return addr < ESP32S3_I2C_NREGS * 4 ? s->regs[addr / 4] : 0;
    }
}

static void esp32s3_i2c_write(void *opaque, hwaddr addr, uint64_t value, unsigned int size)
{
    ESP32S3I2CState *s = ESP32S3_I2C(opaque);

    switch (addr) {
    case A_CTR:
        if (FIELD_EX32(value, CTR, FSM_RST)) {
            esp32s3_i2c_release(s);
            s->addr_next = false;
            s->resp_rec = false;
        }
        s->regs[R_CTR] = value & ~(R_CTR_TRANS_START_MASK | R_CTR_FSM_RST_MASK |
                                   R_CTR_CONF_UPGATE_MASK);
        if (FIELD_EX32(value, CTR, TRANS_START)) {
            esp32s3_i2c_run(s);
            esp32s3_i2c_update_irq(s);
        }
        break;
    case A_FIFO_CONF:
        if (FIELD_EX32(value, FIFO_CONF, RX_FIFO_RST)) {
            fifo8_reset(&s->rx_fifo);
        }
        if (FIELD_EX32(value, FIFO_CONF, TX_FIFO_RST)) {
            fifo8_reset(&s->tx_fifo);
        }
        s->regs[R_FIFO_CONF] = value;
        break;
    case A_DATA:
        if (fifo8_is_full(&s->tx_fifo)) {
            qemu_log_mask(LOG_GUEST_ERROR, "esp32s3.i2c: TX FIFO overflow\n");
        } else {
            fifo8_push(&s->tx_fifo, value & 0xff);
        }
        break;
    case A_INT_CLR:
        s->regs[R_INT_RAW] &= ~value;
        esp32s3_i2c_update_irq(s);
        break;
    case A_INT_ENA:
        s->regs[R_INT_ENA] = value;
        esp32s3_i2c_update_irq(s);
        break;
    case A_SR:
    case A_INT_RAW:
    case A_INT_STATUS:
        break;
    case A_SCL_SP_CONF:
        /* The 9-clock bus clear finishes at once */
        s->regs[R_SCL_SP_CONF] = value & ~R_SCL_SP_CONF_SCL_RST_SLV_EN_MASK;
        break;
    default:
        if (addr < ESP32S3_I2C_NREGS * 4) {
            s->regs[addr / 4] = value;
        }
        break;
    }
}

static const MemoryRegionOps esp32s3_i2c_ops = {
    .read = esp32s3_i2c_read,
    .write = esp32s3_i2c_write,
    .endianness = DEVICE_LITTLE_ENDIAN,
    .valid.min_access_size = 4,
    .valid.max_access_size = 4,
};

static void esp32s3_i2c_reset_hold(Object *obj, ResetType type)
{
    ESP32S3I2CState *s = ESP32S3_I2C(obj);

    esp32s3_i2c_release(s);
    s->addr_next = false;
    s->resp_rec = false;
    fifo8_reset(&s->tx_fifo);
    fifo8_reset(&s->rx_fifo);
    memset(s->regs, 0, sizeof(s->regs));
    s->regs[R_DATE] = 0x20070201;
    esp32s3_i2c_update_irq(s);
}

static void esp32s3_i2c_init(Object *obj)
{
    ESP32S3I2CState *s = ESP32S3_I2C(obj);
    SysBusDevice *sbd = SYS_BUS_DEVICE(obj);

    memory_region_init_io(&s->iomem, obj, &esp32s3_i2c_ops, s, TYPE_ESP32S3_I2C,
                          ESP32S3_I2C_IO_SIZE);
    sysbus_init_mmio(sbd, &s->iomem);
    sysbus_init_irq(sbd, &s->irq);
    s->bus = i2c_init_bus(DEVICE(s), "i2c");
    fifo8_create(&s->tx_fifo, ESP32S3_I2C_FIFO_LEN);
    fifo8_create(&s->rx_fifo, ESP32S3_I2C_FIFO_LEN);
}

static void esp32s3_i2c_finalize(Object *obj)
{
    ESP32S3I2CState *s = ESP32S3_I2C(obj);

    fifo8_destroy(&s->tx_fifo);
    fifo8_destroy(&s->rx_fifo);
}

static void esp32s3_i2c_class_init(ObjectClass *klass, const void *data)
{
    ResettableClass *rc = RESETTABLE_CLASS(klass);

    rc->phases.hold = esp32s3_i2c_reset_hold;
}

static const TypeInfo esp32s3_i2c_info = {
    .name = TYPE_ESP32S3_I2C,
    .parent = TYPE_SYS_BUS_DEVICE,
    .instance_size = sizeof(ESP32S3I2CState),
    .instance_init = esp32s3_i2c_init,
    .instance_finalize = esp32s3_i2c_finalize,
    .class_init = esp32s3_i2c_class_init,
};

static void esp32s3_i2c_register_types(void)
{
    type_register_static(&esp32s3_i2c_info);
}

type_init(esp32s3_i2c_register_types)
