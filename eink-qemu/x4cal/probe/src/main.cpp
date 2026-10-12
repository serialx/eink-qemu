// SPDX-License-Identifier: GPL-2.0-or-later
// Identical diagnostic for hardware and QEMU. No automatic benchmark on boot.
#include <Arduino.h>
#include <SPI.h>
#include <esp_heap_caps.h>
#include <esp_sleep.h>
#include <esp_timer.h>
#include <esp_rtc_time.h>
#include <esp_vfs_fat.h>
#include <driver/sdmmc_host.h>
#include <driver/gpio.h>
#include <sdmmc_cmd.h>
#include <ff.h>
#include <cstring>

#define NOINLINE __attribute__((noinline, noclone, noipa))
constexpr unsigned kRepeats = 5, kWords = 8192, kLoops = 500000;
alignas(16) uint32_t memory[kWords];
uint32_t *workMemory = memory;
unsigned workWords = kWords;
volatile uint32_t sink, factor = 1664525;
volatile float fFactor = 0.99999988f, fDivisor = 1.00000012f;
volatile double dFactor = 0.999999999;
RTC_DATA_ATTR uint32_t deepMarker;
RTC_NOINIT_ATTR uint32_t lightRecord;
RTC_NOINIT_ATTR int64_t lastLightUs;
RTC_NOINIT_ATTR esp_err_t lastLightError;
RTC_NOINIT_ATTR uint32_t deepRecord, deepWake;
RTC_NOINIT_ATTR int64_t deepStart, deepElapsed;

NOINLINE uint32_t IRAM_ATTR iramLoop() {
    uint32_t x = 0x12345678;
    for (unsigned i = 0; i < kLoops; i++) { x ^= x << 5; x ^= x >> 7; x += i; }
    return x;
}
NOINLINE uint32_t flashLoop() {
    uint32_t x = 0x12345678;
    for (unsigned i = 0; i < kLoops; i++) { x ^= x << 5; x ^= x >> 7; x += i; }
    return x;
}
NOINLINE uint32_t multiply() {
    uint32_t x = 3, f = factor;
    for (unsigned i = 0; i < kLoops; i++) x = x * f + i;
    return x;
}
NOINLINE uint32_t divide() {
    uint32_t x = 123456789;
    for (unsigned i = 0; i < kLoops; i++) x = (x + 0x12345678) / ((i & 31) + 1);
    return x;
}
NOINLINE uint32_t fmul() {
    float x = 1.25f, f = fFactor;
    for (unsigned i = 0; i < kLoops; i++) x = x * f + 0.00001f;
    uint32_t bits; memcpy(&bits, &x, 4); return bits;
}
NOINLINE uint32_t fdiv() {
    float x = 1.25f, f = fDivisor;
    for (unsigned i = 0; i < kLoops; i++) x = x / f + 0.00001f;
    uint32_t bits; memcpy(&bits, &x, 4); return bits;
}
NOINLINE uint32_t dmul() {
    double x = 1.25, f = dFactor;
    for (unsigned i = 0; i < kLoops / 4; i++) x = x * f + 0.00001;
    uint64_t bits; memcpy(&bits, &x, 8); return bits ^ (bits >> 32);
}
NOINLINE uint32_t step(uint32_t x) { return (x * 33) ^ (x >> 5); }
NOINLINE uint32_t IRAM_ATTR iramStep(uint32_t x) { return (x * 33) ^ (x >> 5); }
NOINLINE uint32_t calls() { uint32_t x = 1; for (unsigned i=0;i<kLoops;i++) x=step(x); return x; }
NOINLINE uint32_t iramCalls() { uint32_t x = 1; for (unsigned i=0;i<kLoops;i++) x=iramStep(x); return x; }
NOINLINE uint32_t branch() {
    uint32_t x = 1, y = 0;
    for (unsigned i=0;i<kLoops;i++) {
        x = x * 1664525 + 1013904223;
        if (x & 0x10000) y += x; else y ^= x;
    }
    return y;
}
NOINLINE uint32_t loadStore() {
    volatile uint32_t *p = workMemory;
    for (unsigned pass=0;pass<16;pass++) for (unsigned i=0;i<workWords;i++) p[i] += i + pass;
    return p[workWords-1];
}
NOINLINE uint32_t loadRandom() {
    volatile uint32_t *p = workMemory;
    uint32_t x = 1, sum = 0;
    for (unsigned i=0;i<kLoops;i++) {
        x = x * 1664525 + 1013904223;
        sum += p[(x >> 8) & (workWords-1)];
    }
    return sum;
}
void *(*volatile copyBytes)(void *, const void *, size_t) = memcpy;
void *(*volatile fillBytes)(void *, int, size_t) = memset;
NOINLINE uint32_t copy1k() {
    for (unsigned i=0;i<5000;i++) copyBytes(workMemory+256, workMemory, 1024);
    return workMemory[511];
}
NOINLINE uint32_t fill1k() {
    for (unsigned i=0;i<5000;i++) fillBytes(workMemory, i & 255, 1024);
    return workMemory[255];
}
// Held-out mixed workload: glyph-like masks, offsets and pixel blending.
NOINLINE uint32_t render() {
    uint32_t sum = 0;
    for (unsigned page=0;page<64;page++) for (unsigned i=0;i<kWords;i++) {
        uint32_t glyph = (i * 2654435761u) ^ (page * 97);
        unsigned x = (i * 13 + page) & (kWords-1);
        uint32_t v = workMemory[x];
        for (unsigned bit=0;bit<8;bit++) if ((glyph >> bit) & 1) v = (v * 3 + bit * 85) / 4;
        workMemory[x] = v; sum += v;
    }
    return sum;
}
void measure(const char *name, uint32_t (*fn)(), unsigned mhz) {
    for (unsigned rep=0;rep<kRepeats;rep++) {
        for (unsigned i=0;i<workWords;i++) workMemory[i] = i * 2654435761u;
        delay(2);
        int64_t start = esp_timer_get_time();
        uint32_t value = fn();
        int64_t elapsed = esp_timer_get_time() - start;
        sink = value;
        Serial.printf("[CAL] CPU name=%s mhz=%u actual_mhz=%u rep=%u us=%lld sum=%08lx pc=%08lx data=%08lx bytes=%u\n",
                      name, mhz, getCpuFrequencyMhz(), rep, elapsed, (unsigned long)value,
                      (unsigned long)fn, (unsigned long)workMemory, workWords * 4);
        Serial.flush();
    }
}
void cpuProbe() {
    for (unsigned mhz : {240u, 160u, 80u, 40u}) {
        if (!setCpuFrequencyMhz(mhz)) { Serial.printf("[CAL] ERROR clock=%u\n", mhz); continue; }
        workMemory = memory; workWords = kWords;
        measure("iram_loop", iramLoop, mhz); measure("flash_loop", flashLoop, mhz);
        if (mhz != 240) continue;
        measure("mul", multiply, mhz); measure("div", divide, mhz);
        measure("fmul", fmul, mhz); measure("fdiv", fdiv, mhz); measure("dmul", dmul, mhz);
        measure("call", calls, mhz); measure("call_iram", iramCalls, mhz); measure("branch", branch, mhz);
        measure("sram_seq", loadStore, mhz); measure("sram_random", loadRandom, mhz);
        measure("sram_copy", copy1k, mhz); measure("sram_fill", fill1k, mhz); measure("render_sram", render, mhz);
        for (unsigned words : {kWords, 131072u}) {
            workMemory = (uint32_t *)heap_caps_malloc(words * 4, MALLOC_CAP_SPIRAM | MALLOC_CAP_8BIT);
            if (!workMemory) { Serial.println("[CAL] ERROR psram allocation"); continue; }
            workWords = words;
            measure("psram_seq", loadStore, mhz); measure("psram_random", loadRandom, mhz);
            measure("psram_copy", copy1k, mhz); measure("psram_fill", fill1k, mhz);
            if (words == kWords) measure("render_psram", render, mhz);
            heap_caps_free(workMemory);
        }
    }
    setCpuFrequencyMhz(240); workMemory = memory; workWords = kWords;
    Serial.println("[CAL] DONE cpu");
}

constexpr char kTestFile[] = "0:/EQCAL013.BIN";
constexpr unsigned kFileSectors = 8192;
uint32_t pattern(unsigned word) { return word * 2654435761u ^ 0x82796813; }
void sdProbe() {
    setCpuFrequencyMhz(240);
    pinMode(5, OUTPUT); digitalWrite(5, HIGH); delay(80); digitalWrite(5, LOW); delay(120);
    sdmmc_host_t host = SDMMC_HOST_DEFAULT(); host.max_freq_khz = SDMMC_FREQ_HIGHSPEED;
    sdmmc_slot_config_t slot = SDMMC_SLOT_CONFIG_DEFAULT();
    slot.width = 1; slot.clk = GPIO_NUM_41; slot.cmd = GPIO_NUM_42; slot.d0 = GPIO_NUM_40;
    slot.flags |= SDMMC_SLOT_FLAG_INTERNAL_PULLUP;
    esp_vfs_fat_sdmmc_mount_config_t cfg = {};
    cfg.format_if_mount_failed = false; cfg.max_files = 2; cfg.allocation_unit_size = 0;
    sdmmc_card_t *card = nullptr;
    esp_err_t error = esp_vfs_fat_sdmmc_mount("/sdcard", &host, &slot, &cfg, &card);
    if (error != ESP_OK) { Serial.printf("[CAL] ERROR mount=%x\n", error); return; }
    Serial.printf("[CAL] CARD name=%s mid=%u oid=%u serial=%lu sectors=%lu size=%u max_khz=%lu real_khz=%u width=%u\n",
                  card->cid.name, card->cid.mfg_id, card->cid.oem_id, (unsigned long)card->cid.serial,
                  (unsigned long)card->csd.capacity, card->csd.sector_size,
                  (unsigned long)card->max_freq_khz, card->real_freq_khz, card->log_bus_width);
    FIL file = {};
    FRESULT fr = f_open(&file, kTestFile, FA_WRITE | FA_CREATE_NEW);
    if (fr != FR_OK) {
        Serial.printf("[CAL] ERROR create=%u existing_file_is_never_overwritten\n", fr);
        esp_vfs_fat_sdcard_unmount("/sdcard", card); return;
    }
    fr = f_expand(&file, kFileSectors * 512, 1);
    uint32_t first = file.obj.fs->database + (file.obj.sclust - 2) * file.obj.fs->csize;
    unsigned cluster = file.obj.fs->csize;
    FRESULT sync = f_sync(&file), close = f_close(&file);
    auto *buf = (uint32_t *)heap_caps_malloc(32 * 512, MALLOC_CAP_DMA | MALLOC_CAP_8BIT);
    unsigned errors = 0, ops = 0;
    if (fr != FR_OK || sync != FR_OK || close != FR_OK || !buf || first + kFileSectors > card->csd.capacity) {
        Serial.printf("[CAL] ERROR extent=%u sync=%u close=%u buffer=%u\n",fr,sync,close,buf!=nullptr);
        errors++;
    } else {
        Serial.printf("[CAL] EXTENT first=%lu sectors=%u cluster_sectors=%u\n",(unsigned long)first,kFileSectors,cluster);
        // f_expand allocated one contiguous file. All raw writes stay inside it.
        for (unsigned i=0;i<32*128;i++) buf[i] = pattern(i);
        for (unsigned off=0;off<kFileSectors;off+=32) {
            if (sdmmc_write_sectors(card,buf,first+off,32)!=ESP_OK) { errors++; break; }
        }
        for (unsigned khz : {20000u, 40000u, 10000u}) {
            if (errors || khz > (unsigned)card->max_freq_khz) continue;
            error = sdmmc_host_set_card_clk(host.slot, khz);
            int real = 0; sdmmc_host_get_real_freq(host.slot, &real);
            if (error != ESP_OK) { errors++; break; }
            for (char op : {'r','w'}) for (const char *pat : {"rep","seq","rnd"})
                for (unsigned n : {1u,2u,4u,8u,32u}) for (unsigned rep=0;rep<24;rep++) {
                    unsigned off = !strcmp(pat,"rep") ? 0 : !strcmp(pat,"seq") ? rep*32 : ((rep*337u+53u)%256)*32;
                    for (unsigned i=0;i<n*128;i++) buf[i]=pattern(i);
                    int64_t start=esp_timer_get_time();
                    error = op=='r' ? sdmmc_read_sectors(card,buf,first+off,n) : sdmmc_write_sectors(card,buf,first+off,n);
                    int64_t elapsed=esp_timer_get_time()-start;
                    if (op=='w' && error==ESP_OK) error=sdmmc_read_sectors(card,buf,first+off,n);
                    uint32_t sum=0; unsigned mismatch=0;
                    for (unsigned i=0;i<n*128;i++) { sum+=buf[i]; mismatch += buf[i]!=pattern(i); }
                    errors += error!=ESP_OK || mismatch!=0; ops++;
                    Serial.printf("[CAL] SD op=%c pat=%s n=%u rep=%u khz=%d sector=%lu us=%lld sum=%08lx err=%x mismatch=%u\n",
                                  op,pat,n,rep,real,(unsigned long)(first+off),elapsed,(unsigned long)sum,error,mismatch);
                    Serial.flush();
                }
        }
    }
    heap_caps_free(buf);
    fr=f_unlink(kTestFile); if (fr!=FR_OK) errors++;
    esp_vfs_fat_sdcard_unmount("/sdcard",card);
    Serial.printf("[CAL] DONE sd ops=%u errors=%u cleanup=%u\n",ops,errors,fr);
}

SPIClass panelSPI(FSPI);
void cmd(uint8_t c) { digitalWrite(18,LOW); digitalWrite(13,LOW); panelSPI.transfer(c); digitalWrite(13,HIGH); }
void data(const uint8_t *p, unsigned n) {
    digitalWrite(18,HIGH); digitalWrite(13,LOW); panelSPI.writeBytes(p,n); digitalWrite(13,HIGH);
}
void reg(uint8_t c, std::initializer_list<uint8_t> bytes) { cmd(c); data(bytes.begin(),bytes.size()); }
bool busy(const char *name, unsigned rep, unsigned frames=0, unsigned pll=14) {
    int64_t start=esp_timer_get_time();
    while (digitalRead(6) && esp_timer_get_time()-start<5000) {}
    int64_t edge=esp_timer_get_time(); bool asserted=!digitalRead(6);
    while (!digitalRead(6) && esp_timer_get_time()-start<5000000) delayMicroseconds(10);
    int64_t end=esp_timer_get_time();
    Serial.printf("[CAL] PANEL name=%s rep=%u frames=%u pll=%u us=%lld edge_us=%lld asserted=%u timeout=%u\n",
                  name,rep,frames,pll,end-start,edge-start,asserted,!digitalRead(6));
    Serial.flush(); return asserted && digitalRead(6);
}
void panelInit() {
    for (int pin : {13,18,14}) pinMode(pin,OUTPUT);
    pinMode(6,INPUT); digitalWrite(13,HIGH);
    digitalWrite(14,LOW); delay(10); digitalWrite(14,HIGH); delay(20);
    panelSPI.begin(12,-1,11,13); panelSPI.beginTransaction(SPISettings(10000000,MSBFIRST,SPI_MODE0));
    reg(0x00,{0x37,0x4d}); reg(0x61,{3,32,2,88}); reg(0x65,{0,0,0,0});
    reg(0x03,{0x20}); reg(0x30,{0x0e}); reg(0xe1,{2});
}
void plane(uint8_t command, uint8_t value, unsigned rep) {
    uint8_t row[100]; memset(row,value,sizeof(row));
    int64_t start=esp_timer_get_time(); cmd(command);
    for(unsigned y=0;y<600;y++) data(row,sizeof(row));
    Serial.printf("[CAL] PLANE cmd=%u rep=%u bytes=60000 us=%lld\n",command,rep,esp_timer_get_time()-start);
}
void panelProbe() {
    panelInit();
    for(unsigned rep=0;rep<3;rep++) {
        cmd(0x04); busy("pon",rep); reg(0x30,{0x0e});
        cmd(0x92); plane(0x10,0xff,rep); plane(0x13,0xff,rep);
        reg(0x50,{0x97}); reg(0xe0,{2}); reg(0xe5,{0x1e}); reg(0x00,{0x17,0x4d}); cmd(0x12); busy("full",rep);
        cmd(0x91); reg(0x90,{0,0,3,31,0,120,2,87,1});
        reg(0x50,{0xd7}); reg(0xe5,{0x5a}); cmd(0x12); busy("fast",rep); cmd(0x92);
        for (uint8_t pll : {uint8_t(14),uint8_t(13),uint8_t(15)}) {
            reg(0x30,{pll}); reg(0x50,{0x97}); reg(0x00,{0x37,0x4d});
            // All rows use neutral rails: vary duration without adding DC drive.
            for (unsigned frames : {1u,4u,7u,16u,25u,47u,63u}) {
                uint8_t lut[49]={1,uint8_t(frames),0,0,0,1,0};
                for(unsigned row=0;row<5;row++) {cmd(0x20+row);data(lut,sizeof(lut));}
                cmd(0x12); busy("neutral",rep,frames,pll);
            }
        }
        cmd(0x02); busy("pof",rep);
    }
    panelSPI.endTransaction(); panelSPI.end();
    Serial.println("[CAL] DONE panel");
}
void matrixProbe() {
    // Neutral rails only. Isolate the controller's repeat/phase overhead.
    static const struct { const char *name; uint8_t lut[49]; } cases[] = {
        {"zero", {1,0,0,0,0,0,0}},
        {"p1", {1,7,0,0,0,1,0}}, {"p2", {1,0,7,0,0,1,0}},
        {"p3", {1,0,0,7,0,0,1}}, {"p4", {1,0,0,0,7,0,1}},
        {"repeat0", {1,7,0,0,0,0,0}}, {"repeat2", {1,7,0,0,0,2,0}},
        {"group2", {2,7,0,0,0,1,0}},
        {"gray", {1,2,3,1,1,1,1,0,0,0,0,0,1,1}},
        {"settle", {1,6,1,6,6,1,1,1,2,4,0,0,1,1}},
        {"split7", {1,1,2,2,2,1,1}},
        {"vcom25", {1,7,0,0,0,1,0}},
        {"ww25", {1,7,0,0,0,1,0}},
        {"bb25", {1,7,0,0,0,1,0}},
    };
    panelInit();
    for(unsigned rep=0;rep<3;rep++) {
        reg(0x30,{14}); cmd(0x04); busy("pon",rep);
        reg(0x00,{0x37,0x4d}); reg(0x50,{0x97}); reg(0xe0,{2});
        cmd(0x92); plane(0x10,0xff,rep); plane(0x13,0xff,rep);
        for(auto &c : cases) {
            for(unsigned row=0;row<5;row++) {
                uint8_t lut[49]; memcpy(lut,c.lut,sizeof(lut));
                if ((!strcmp(c.name,"vcom25") && row==0) ||
                    (!strcmp(c.name,"ww25") && row==1) ||
                    (!strcmp(c.name,"bb25") && row==4)) lut[1]=25;
                cmd(0x20+row);data(lut,sizeof(lut));
            }
            cmd(0x12); busy(c.name,rep);
        }
        for(uint8_t pll : {uint8_t(13),uint8_t(14),uint8_t(15)}) {
            reg(0x30,{pll}); cmd(0x02); busy("pof_pll",rep,0,pll);
            cmd(0x04); busy("pon_pll",rep,0,pll);
        }
        reg(0x30,{14}); cmd(0x02); busy("pof",rep);
    }
    panelSPI.endTransaction();panelSPI.end();Serial.println("[CAL] DONE matrix");
}
void vendorProbe() {
    for(unsigned rep=0;rep<3;rep++) {
        panelInit();cmd(0x04);busy("vendor_pon",rep);
        plane(0x10,0xff,rep);plane(0x13,0xff,rep);
        auto gray = [&](const char *name, bool rails) {
            reg(0x00,{0x37,0x4d});reg(0x50,{0x97});
            for(unsigned row=0;row<5;row++) {
                uint8_t lut[49]={1,2,3,1,1,1,1,0,0,0,0,0,1,1};
                if(rails) {
                    if(row==1)lut[3]=0x41;
                    if(row==2 || row==3)lut[2]=0x83;
                    if(row==4)lut[3]=0x81;
                }
                cmd(0x20+row);data(lut,sizeof(lut));
            }
            cmd(0x12);busy(name,rep,7);
        };
        auto longer = [&](const char *name) {
            for(unsigned frames : {25u,47u}) {
                reg(0x00,{0x37,0x4d});reg(0x50,{0x97});
                uint8_t lut[49]={1,uint8_t(frames),0,0,0,1,0};
                for(unsigned row=0;row<5;row++) {cmd(0x20+row);data(lut,sizeof(lut));}
                cmd(0x12);busy(name,rep,frames);
            }
        };
        gray("before_otp",false);
        reg(0xe0,{2});reg(0xe5,{30});reg(0x00,{0x17,0x4d});cmd(0x12);busy("vendor_full",rep);
        gray("after_full",false);
        longer("long_after_full");
        reg(0x30,{14});gray("pll_after_full",false);
        cmd(0x91);reg(0x90,{0,0,3,31,0,120,2,87,1});
        reg(0x50,{0xd7});reg(0xe5,{90});reg(0x00,{0x17,0x4d});cmd(0x12);busy("vendor_fast",rep);
        gray("partial_after_fast",false);cmd(0x92);
        cmd(0x91);longer("long_partial_after_fast");cmd(0x92);
        gray("after_fast",false);
        longer("long_after_fast");
        reg(0x30,{14});gray("pll_after_fast",false);
        gray("vendor_gray",true);
        cmd(0x02);busy("vendor_pof",rep);
        panelSPI.endTransaction();panelSPI.end();
    }
    Serial.println("[CAL] DONE vendor");
}
void spiProbe() {
    uint8_t buf[1024];memset(buf,0xff,sizeof(buf));
    panelInit();panelSPI.endTransaction();
    for(unsigned hz : {2500000u,5000000u,10000000u}) {
        panelSPI.beginTransaction(SPISettings(hz,MSBFIRST,SPI_MODE0));
        for(unsigned chunk : {1u,4u,16u,32u,64u,100u,1024u}) for(unsigned rep=0;rep<3;rep++) {
            cmd(0x13);int64_t start=esp_timer_get_time();
            for(unsigned off=0;off<60000;off+=chunk) data(buf,min(chunk,60000-off));
            Serial.printf("[CAL] SPI hz=%u chunk=%u rep=%u bytes=60000 us=%lld\n",hz,chunk,rep,esp_timer_get_time()-start);
            Serial.flush();
        }
        panelSPI.endTransaction();
    }
    panelSPI.end();Serial.println("[CAL] DONE spi");
}
void info() {
    Serial.printf("[CAL] READY version=4 cpu_mhz=%u flash_hz=%lu psram=%lu wake=%u deep=%lu\n",
                  getCpuFrequencyMhz(),(unsigned long)ESP.getFlashChipSpeed(),(unsigned long)ESP.getPsramSize(),
                  esp_sleep_get_wakeup_cause(),(unsigned long)deepMarker);
    deepMarker=0;
    if(lightRecord==0x82790001 || lightRecord==0x82790002)
        Serial.printf("[CAL] SLEEP_RECORD state=%lx us=%lld reset=%u\n",(unsigned long)lightRecord,lastLightUs,esp_reset_reason());
    if(lightRecord==0x82790002) Serial.printf("[CAL] DONE light us=%lld err=%x wake=%u\n",lastLightUs,lastLightError,esp_sleep_get_wakeup_cause());
    if(deepRecord==0x82790004) Serial.printf("[CAL] DEEP_RECORD wake=%lu us=%lld\n",(unsigned long)deepWake,deepElapsed);
}
void setup() {
    if(deepRecord==0x82790003 && esp_sleep_get_wakeup_cause()!=ESP_SLEEP_WAKEUP_UNDEFINED) {
        deepWake=esp_sleep_get_wakeup_cause();deepElapsed=esp_rtc_get_time_us()-deepStart;deepRecord=0x82790004;
    }
    gpio_deep_sleep_hold_dis(); gpio_hold_dis(GPIO_NUM_1);
    pinMode(1,OUTPUT); digitalWrite(1,HIGH);
    Serial.begin(115200); unsigned start=millis(); while(!Serial && millis()-start<5000) delay(10);
    setCpuFrequencyMhz(240); info();
}
void loop() {
    static char input[32]; static unsigned used;
    while(Serial.available()) {
        char c=Serial.read();
        if(c=='\n') {
            input[used]=0; used=0;
            if(!strcmp(input,"info")) { info(); Serial.println("[CAL] DONE info"); }
            else if(!strcmp(input,"cpu")) cpuProbe();
            else if(!strcmp(input,"sd")) sdProbe();
            else if(!strcmp(input,"panel")) panelProbe();
            else if(!strcmp(input,"matrix")) matrixProbe();
            else if(!strcmp(input,"vendor")) vendorProbe();
            else if(!strcmp(input,"spi")) spiProbe();
            else if(!strcmp(input,"light")) {
                Serial.flush(); esp_sleep_enable_timer_wakeup(100000);
                lightRecord=0x82790001;lastLightUs=-1;
                int64_t start=esp_timer_get_time(); lastLightError=esp_light_sleep_start();
                lastLightUs=esp_timer_get_time()-start;
                lightRecord=0x82790002;
                Serial.printf("[CAL] DONE light us=%lld err=%x wake=%u\n",lastLightUs,lastLightError,esp_sleep_get_wakeup_cause());
            } else if(!strcmp(input,"deep")) {
                Serial.println("[CAL] ENTER deep"); Serial.flush(); deepMarker=0x827968;
                deepStart=esp_rtc_get_time_us();deepRecord=0x82790003;
                gpio_hold_en(GPIO_NUM_1); gpio_deep_sleep_hold_en();
                esp_sleep_enable_timer_wakeup(200000); esp_deep_sleep_start();
            } else Serial.println("[CAL] ERROR command");
        } else if(c!='\r' && used<sizeof(input)-1) input[used++]=c;
    }
    delay(1);
}
