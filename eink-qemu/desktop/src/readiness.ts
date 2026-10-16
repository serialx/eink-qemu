/* When a boot is finished, decided as the Rust core decides it
   (core/src/script/readiness.rs). Firmware with a profile in
   eink-qemu/profiles is followed by its log: CrossPoint's boot milestones
   (completed panel refreshes, then its memory report) and a card it could
   not mount, the stock X4 Pro firmware's wait for Power. Any other firmware
   is ready once its screen has been quiet for 5 s after an update of this
   boot, as long as the ROM is not rebooting it in a loop. ROM messages about
   an image it cannot boot are reported at once. Both front ends feed it the
   console lines and screen updates and app.ts ticks it;
   tests/test_readiness.mjs drives it with a fake clock. */
import crosspoint from '../../profiles/crosspoint.json' with { type: 'json' };
import stock from '../../profiles/xteink-stock.json' with { type: 'json' };

const refreshDone = new RegExp(crosspoint.ready.refresh_done);
const memoryReport = new RegExp(crosspoint.ready.report);
const sdFailure = new RegExp(crosspoint.ready.sd_failed);
const powerWait = new RegExp(stock.ready.power_wait);
// ESP-IDF's ROM or second-stage bootloader refusing the image.
const bootFailure = /No bootable app partitions|invalid segment length|Image hash failed|Failed to verify app image|invalid magic byte|Checksum failure|ota data partition invalid and no factory/;
/** Quiet screen (and no new ROM banner) for this long makes firmware without a profile ready. */
export const QUIET_MS = 5000;

export class Readiness {
  /** Seconds from boot to the Home screen (CrossPoint), or to the last screen
      update before the screen went quiet (other firmware), once ready. */
  bootSeconds: number | null = null;
  private milestones = 0;
  private lastDrawAt = 0;
  private startedAt = 0;
  private sdFailed = false;
  /** Seen CrossPoint's refresh lines: its rules decide, not the quiet screen. Kept across resets. */
  private crosspoint = false;
  private lastRomAt: number | null = null;
  /** The last screen update of this boot. */
  private lastScreenAt: number | null = null;
  /** After a reset: nothing counts until the new boot's ROM banner. */
  private awaitRom = false;
  private restarts = 0;
  /** This boot is held at the stock firmware's power gate: its screen is quiet because the device is off. */
  private powerWait = false;
  /** Ready, or a card it cannot mount, has been said for this boot. */
  private decided = false;
  private failed = false;
  private readonly status: (text: string) => void;
  private readonly device: () => string;
  private readonly timing: () => string;
  private readonly now: () => number;

  /** `device` names the board ("X3"), `timing` the mode ("Accurate timing"). */
  constructor(status: (text: string) => void, device: () => string, timing: () => string, now: () => number = () => performance.now()) {
    this.status = status;
    this.device = device;
    this.timing = timing;
    this.now = now;
  }

  /** The device boots afresh: Start (other firmware, perhaps), or with
      `reset` a Reset or a changed card, where the firmware is the same and
      what came before the new boot's ROM banner no longer counts. */
  restart(reset = false) {
    this.newBoot();
    this.restarts = 0;
    this.failed = false;
    this.startedAt = this.now();
    this.lastRomAt = null;
    this.awaitRom = reset;
    if (!reset) this.crosspoint = false;
  }

  /** A page that attaches to a session already running (a reloaded
      WebView): its boot is over; the next reboot counts. */
  resume() {
    this.restart();
    this.decided = true;
    this.lastRomAt = this.now();
  }

  private newBoot() {
    this.milestones = 0;
    this.lastDrawAt = 0;
    this.bootSeconds = null;
    this.sdFailed = false;
    this.lastScreenAt = null;
    this.powerWait = false;
    this.decided = false;
  }

  private prefix() { return `${this.device()} · ${this.timing()}`; }

  feed(lines: string[]) {
    for (const line of lines) {
      if (this.awaitRom) {
        if (!line.startsWith('ESP-ROM:')) continue;
        // The new boot starts here.
        this.awaitRom = false;
        this.startedAt = this.now();
      }
      if (line.startsWith('ESP-ROM:')) {
        if (this.milestones || this.lastRomAt !== null) {
          // The guest rebooted (a crash, a firmware reset, a wake from deep sleep): count again.
          this.restarts++;
          this.newBoot();
          this.startedAt = this.now();
          const loop = this.restarts > 2 ? ` (rebooted ${this.restarts} times: a boot loop?)` : '';
          if (!this.failed) this.status(`${this.prefix()} · Restarting…${loop}`);
        }
        this.lastRomAt = this.now();
      }
      const refused = bootFailure.exec(line);
      if (refused && !this.failed) {
        this.failed = true;
        this.status(`${this.device()} · The ROM cannot boot this image: ${refused[0]}`);
      }
      if (this.failed) continue;
      if (sdFailure.test(line)) this.sdFailed = true;
      // Stock X4 Pro firmware powers itself off unless Power is still held when its app starts.
      if (powerWait.test(line) && this.bootSeconds === null) {
        this.powerWait = true;
        this.status(`${this.device()} is off · hold Power (P) for about 3 s to turn it on`);
      }
      if (/main_task: Returned from app_main\(\)/.test(line) && this.bootSeconds === null && !this.decided) {
        this.status(`${this.prefix()} · Firmware running`);
      }
      if (refreshDone.test(line)) {
        this.crosspoint = true;
        this.lastDrawAt = this.now();
        if (++this.milestones === crosspoint.ready.milestones) {
          this.bootSeconds = (this.lastDrawAt - this.startedAt) / 1000;
          if (!this.sdFailed) this.status(`${this.prefix()} · Home drawn in ${this.bootSeconds.toFixed(2)} s · finishing startup…`);
        }
      }
      if (memoryReport.test(line) && !this.decided) {
        if (this.sdFailed) {
          this.decided = true;
          this.status(`${this.device()} · The firmware could not mount the SD card · check it in the SD card panel`);
        } else if (this.bootSeconds !== null) {
          this.decided = true;
          this.status(`${this.prefix()} · Ready · Home drawn in ${this.bootSeconds.toFixed(2)} s`);
        } else if (this.milestones && this.now() - this.lastDrawAt > crosspoint.ready.quiet_after_draw_s * 1000) {
          // Firmware that resumes a book or an app draws fewer full refreshes
          // than a boot to Home; once the screen is quiet and it reports in, it runs.
          this.decided = true;
          this.status(`${this.prefix()} · Ready`);
        }
      }
    }
  }

  /** The panel shows something new (the backends skip blank frames). */
  screen() {
    if (!this.awaitRom) this.lastScreenAt = this.now();
  }

  /** Called every second or so: decides a quiet screen, which no event marks. */
  tick() {
    if (this.decided || this.failed || this.crosspoint || this.awaitRom || this.powerWait || this.lastScreenAt === null) return;
    const now = this.now();
    if (now - this.lastScreenAt < QUIET_MS || (this.lastRomAt !== null && now - this.lastRomAt < QUIET_MS)) return;
    this.decided = true;
    this.bootSeconds = Math.max(0, this.lastScreenAt - this.startedAt) / 1000;
    this.status(`${this.prefix()} · Ready · screen settled ${this.bootSeconds.toFixed(2)} s after boot`);
  }
}
