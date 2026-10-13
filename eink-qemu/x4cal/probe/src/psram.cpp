// SPDX-License-Identifier: GPL-2.0-or-later
#include <Arduino.h>
#include <esp_heap_caps.h>
#include <esp_timer.h>
#include <esp32s3/rom/cache.h>
#include <soc/extmem_reg.h>

// Fixed instruction streams; IRAM prevents instruction-fetch bus contention.
// Volatile data accesses also make stores and checksums observable.
static __attribute__((noinline, noclone, noipa)) uint32_t IRAM_ATTR kernel(
        volatile uint32_t *p, unsigned mask, unsigned stride, unsigned op,
        unsigned count) {
    uint32_t x = 0, sum = 0, rng = 1;
    for (unsigned i = 0; i < count; i++) {
        if (op == 3) {
            rng = rng * 1664525 + 1013904223;
            x = (rng >> 8) & mask;
        } else if (op == 4) {
            x = p[x] & mask;
        } else {
            x = (x + stride) & mask;
        }
        if (op == 1) p[x] = i;
        else if (op == 2) p[x] += i;
        else sum += p[x];
    }
    return sum ^ x;
}

void psramProbe() {
    constexpr unsigned count = 65536, maximum = 1024 * 1024;
    auto *p = static_cast<uint32_t *>(heap_caps_aligned_alloc(
        64, maximum, MALLOC_CAP_SPIRAM | MALLOC_CAP_8BIT));
    if (!p) { Serial.println("[CAL] ERROR psram allocation"); return; }
    Serial.printf("[CAL] CACHE ctrl=%08lx line=%lu sdk_size=%u sdk_line=%u psram_mhz=%u data=%08lx\n",
        (unsigned long)REG_READ(EXTMEM_DCACHE_CTRL_REG),
        (unsigned long)Cache_Get_DCache_Line_Size(), CONFIG_ESP32S3_DATA_CACHE_SIZE,
        CONFIG_ESP32S3_DATA_CACHE_LINE_SIZE, CONFIG_SPIRAM_SPEED, (unsigned long)p);
    for (unsigned mhz : {240u, 80u}) {
        setCpuFrequencyMhz(mhz);
        for (unsigned bytes : {4096u, 16384u, 32768u, 65536u, 131072u, 524288u, maximum}) {
            for (unsigned op : {0u, 1u, 2u, 3u, 4u}) {
                for (unsigned stride : {1u, 8u, 16u, 1024u}) {
                    if (op >= 3 && stride != 1) continue;
                    for (unsigned rep = 0; rep < 3; rep++) {
                        // LCG permutation has full period for power-of-two sizes.
                        for (unsigned i = 0; i < bytes / 4; i++)
                            p[i] = (i * 1664525u + 1013904223u) & (bytes / 4 - 1);
                        Cache_WriteBack_Addr((uint32_t)p, bytes);
                        Cache_Invalidate_Addr((uint32_t)p, bytes);
                        for (unsigned warm = 0; warm < 2; warm++) {
                            int64_t start = esp_timer_get_time();
                            uint32_t sum = kernel(p, bytes / 4 - 1, stride, op, count);
                            int64_t us = esp_timer_get_time() - start;
                            Serial.printf("[CAL] PSRAM mhz=%u bytes=%u op=%u stride=%u rep=%u warm=%u count=%u us=%lld sum=%08lx\n",
                                mhz, bytes, op, stride, rep, warm, count, us, (unsigned long)sum);
                            Serial.flush();
                        }
                    }
                }
            }
        }
    }
    setCpuFrequencyMhz(240);
    heap_caps_free(p);
    Serial.println("[CAL] DONE psram");
}

template<unsigned Op>
static __attribute__((noinline, noclone, noipa)) uint32_t IRAM_ATTR detailKernel(
        volatile uint32_t *p, unsigned mask, unsigned offset, unsigned stride) {
    uint32_t x = offset, sum = 0;
    for (unsigned i = 0; i < 65536; i++) {
        if constexpr (Op == 1) p[x] = i;
        else if constexpr (Op == 2) p[x] += i;
        else sum += p[x];
        x = (x + stride) & mask;
    }
    return sum;
}

void psramDetail() {
    auto *p = static_cast<uint32_t *>(heap_caps_aligned_alloc(
        64, 524288, MALLOC_CAP_SPIRAM | MALLOC_CAP_8BIT));
    if (!p) { Serial.println("[CAL] ERROR psram allocation"); return; }
    for (unsigned mhz : {240u, 80u}) {
        setCpuFrequencyMhz(mhz);
        for (unsigned bytes : {4096u, 524288u})
        for (unsigned stride : {1u, 8u})
        for (unsigned op : {0u, 1u, 2u})
        for (unsigned offset : {0u, 1u, 3u, 7u})
        for (unsigned rep = 0; rep < 3; rep++) {
            for (unsigned i = 0; i < bytes / 4; i++) p[i] = i;
            Cache_WriteBack_Addr((uint32_t)p, bytes);
            Cache_Invalidate_Addr((uint32_t)p, bytes);
            auto fn = op == 0 ? detailKernel<0> : op == 1 ? detailKernel<1> : detailKernel<2>;
            fn(p, bytes / 4 - 1, offset, stride); // warm the small working set
            int64_t start = esp_timer_get_time();
            uint32_t sum = fn(p, bytes / 4 - 1, offset, stride);
            int64_t us = esp_timer_get_time() - start;
            // Full contents checked outside the timed interval, for stores too.
            uint32_t hash = 0;
            for (unsigned i = 0; i < bytes / 4; i++) hash = (hash * 33) ^ p[i];
            Serial.printf("[CAL] DETAIL mhz=%u bytes=%u stride=%u op=%u offset=%u rep=%u us=%lld sum=%08lx hash=%08lx\n",
                mhz, bytes, stride, op, offset, rep, us, (unsigned long)sum, (unsigned long)hash);
            Serial.flush();
        }
    }
    setCpuFrequencyMhz(240);
    heap_caps_free(p);
    Serial.println("[CAL] DONE detail");
}
