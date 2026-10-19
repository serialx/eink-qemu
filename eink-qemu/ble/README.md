# Bluetooth link layer: what the controller expects (work in progress)

CrossPoint's page turner (C3 builds) runs NimBLE on Espressif's closed
`libbtdm_app_flash.a`, which drives a RivieraWaves BLE link-layer core. The
S3's `misc.esp32s3.ble` block, mapped on the C3 at 0x60031000, lets it come up;
a fake remote needs the hardware side of the exchange memory modeled. These
notes are what the traces below established, on CrossPoint `develop` b64d797c
(2026-10-19), built locally so the ELF names every controller function.

## Tracing

`contrib/plugins/iotrace.c` (built as `build/contrib/plugins/libiotrace.so`)
logs every guest access to chosen physical ranges with the PC of the
instruction:

```sh
eink-qemu/cli run .pio/build/default --machine x3 \
  --qemu-arg=-plugin --qemu-arg=build/contrib/plugins/libiotrace.so,range=0x60031000-0x60032000,range=0x600c2000-0x600c2200,out=io.txt \
  ready ... press:confirm wait:8
python3 eink-qemu/ble/emwins.py io.txt 0x400     # exchange-memory windows -> emwins.json, range args
# run again with those range args added to the plugin options, then:
nm -n firmware.elf | grep ' [TtWw] ' > fw-syms.txt
python3 eink-qemu/ble/symtrace.py fw-syms.txt io.txt runs      # accesses in order, per function
python3 eink-qemu/ble/symtrace.py fw-syms.txt io.txt summary   # registers per function
```

The plugin takes `limit=N` to stop after N lines (the file then ends with a
`#` line, which the scripts skip). `emwins.py` pads each range
(here by 0x400 bytes) because the heap addresses drift a little between runs;
its `emwins.json` is only a record. `symtrace.py` labels register offsets
(`ble+`), baseband (`bb+`, 0x60011000), interrupt matrix (`intm+`) and exchange
memory (`em+`): it replays the mapping-table writes of the trace it reads, so a
DRAM block counts as a window only while its table entry maps it, with the
extent the table gave it at the time. While the controller is still filling
the table, a window's extent reaches to the next entry written so far, so heap
writes to blocks not yet mapped can show as the tail of an earlier window;
accesses after `r_lld_core_init` are attributed as the hardware would. Both
scripts share `emtab.py`.

## Exchange memory

The controller never touches the 0x3fc00000 window itself. Its exchange
memory (EM) is heap DRAM; the hardware reaches it through a mapping table:

- `0x60031204 + 4*i`: bits [31:18] = EM base offset >> 2, bits [17:0] = DRAM
  address >> 2, DRAM = 0x3fc00000 + field*4; a zero DRAM field maps nothing.
  The firmware writes i = 0..39, the last being 0xcc000000 (EM top, unmapped)
  as the end of the list; the enable registers (`0x600312c4` for i < 32,
  `0x600312c8` for 32..47) span 48 entries, so the scripts watch that many.
- Page table in ROM, `em_base_reg_lut` at 0x3ff1f518: 51 entries of 1 KiB EM
  pages, each `{u8 window, u8 pad, u16 base}`; 39 windows cover 51 KiB.
  Window sizes are 1 or 2 KiB; the DRAM block is only as large as needed.
- `r_emi_get_mem_addr_by_offset(off)` = DRAM(window of page off>>10) + off -
  base. The model must translate EM offsets the same way when it walks
  structures.

Layout seen in the scan trace (EM offsets):

| EM | What | Written by |
|---|---|---|
| 0x0000 | ? (356 B) | |
| 0x0400 | control structure (CS) of activity 0 (scan) | `r_lld_scan_start` |
| 0x0c00 | CS of other activities? | |
| 0x1000 | RX descriptor list, stride 0x14: +0 next (0x1014, 0x1028, ...), +2 status 0x8000, +0x12 data pointer (0x7805, 0x7c05, ...) | `r_lld_core_init` |
| 0x1400 | exchange table (ET): entry 0 +0 = 0, +2 = 0xc03, +4 = 0; +0xa written by `r_lld_ext_scan_dynamic_pti_process` | `r_lld_scan_start` |
| 0x7400 | 1 KiB | |
| 0x7800 + n*0x400, n = 0..8 | RX buffers (292 B each) | |
| 0x9800 | 1 KiB; +0x388/+0x38b HCI command/event exchange | NimBLE transport |

Scan CS fields written (values on the X3 with the default NIC MAC): +0x00 0x229
then 0x209, +0x02 0x800, +0x04 0x1000, +0x06..0x0a BD address (NIC MAC + 2),
+0x0c/0x0e access address 0x8e89bed6, +0x10/0x12 CRC init 0x555555, +0x14 0x41,
+0x16 0x8027 (channel 39), +0x18 TX power 0xb, +0x1a 0x8100, +0x1c 0x1400,
+0x20 0x100, +0x28/0x2a 0, +0x34 0x1400, +0x46 0.

## Registers (offsets from 0x60031000)

| Offset | Use seen | Function |
|---|---|---|
| 0x000 | control, 0x10060f / 0x10070f, bit 31 soft reset | `r_lld_core_init`, `r_rwble_hw_disable` |
| 0x004 | version 0x09001b00 | |
| 0x00c | interrupt enable: 0x640166, \| 0x800 while a timer target is armed | `r_rwip_timer_hus_set` |
| 0x010 | interrupt status, read by `r_rwble_isr_hack`; bits 21 and 22 handled specially | |
| 0x018 | interrupt ack (0x800, 0x800000, mask 0xffe00000 ...) | |
| 0x01c/0x020 | base time (bit 31 samples), fine time | `r_rwip_time_get` |
| 0x024 | 0x1000 | `r_lld_core_init` |
| 0x0ec/0x0f0 | timer target: half-slots 0x170d5, fine 0x270 | `r_rwip_timer_hus_set` |
| 0x0e0 | 0x190012c / 0x1be00fa | `r_lld_core_init`, `r_rf_rw_v9_le_init` |
| 0x0f8 | indirect RF table port (index/strobe sequence) | `r_cali_phase_match_p` |
| 0x120..0x17c | 0xffff02d9, 0xc0c00, 0xc00, 0xa0c60, 0xc60, 0x10; RPA renew at 0x178/0x17c | `r_lld_core_init`, `lld_rpa_renew_start` |
| 0x124 | 0x10001 at scan start | `r_lld_scan_start` |
| 0x150, 0x15c, 0x160 | 2, 3, 0x8888888f, 0 (coexistence) | `r_rwip_wlcoex_set` |
| 0x2d4/0x2d8 | 0xff; 0x80000000 then 0x8000001e | `r_lld_core_init` |
| 0x32c | bit 30 set at scan start (priority) | `r_lld_ext_scan_dynamic_pti_process` |
| 0x364 | 0x100 at scan start | `r_lld_scan_start` |
| 0x38c | 0x800 with the timer target | `r_rwip_timer_hus_set` |
| 0x530 | 0xf0 / 0xf1 | `coex_pti_v2` |
| 0x070-0x09c | RF/timing config (0x1020, 0xcc10100, 0x640064 x4, 0xd, 0x20202 ...) | `r_rf_rw_v9_le_init` |

Interrupt matrix: the RWBLE source (8, offset 0x20) is routed to CPU interrupt
11; source 16 (offset 0x40) to 10. No interrupt is ever raised by the model
yet, so the controller's own scheduler (`rwip` timer at 0x0ec) never fires and
a scan stays in "Scanning..." until the host stops it.

## Next

1. Timer target: raise RWBLE when the sampled base time passes 0x0ec/0x0f0
   with the matching status bit (find it in `r_rwble_isr_hack`'s dispatch and
   `r_rwip_isr`), honouring 0x00c and 0x018.
2. Exchange table: when the timer or ET start time is reached, run the
   activity in the ET entry's CS: for a scan, fill the next RX descriptor at
   EM 0x1000 with an ADV_IND from a fake HOGP remote (buffer from +0x12),
   set its status words (layout from `r_lld_scan_process_pkt_rx_legacy_adv`),
   mark the ET entry done (`r_lld_scan_frm_eof_isr`, `r_sch_prog_end_isr_handler`)
   and raise the RX and end-of-event bits.
3. Connection: CONNECT_IND from the TX descriptor, then connection events with
   LL/L2CAP/ATT exchanged with a small GATT server (HID service 0x1812).
