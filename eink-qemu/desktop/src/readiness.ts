/* Readiness from the firmware log: CrossPoint's boot milestones (completed
   panel refreshes, then its memory report), a reboot, a card it could not
   mount, and the stock X4 Pro firmware's wait for a Power press. Firmware
   that never prints CrossPoint's memory report settles as Running a while
   after its Home screen. Both front ends feed it the console lines;
   tests/test_readiness.mjs feeds it too. */

// Panel refresh completions: UC8179/UC8279 boards log their DRF command,
// the X4 (SSD1677) a plain refresh.
const refreshDone = /Wait complete:\s+(?:(?:8179|8279|8279x4|X3)_DRF|refresh)\b/;
// A panel wait and how long it took, in guest time. A real refresh takes
// seconds at most; a wait this long ran out because the panel never answered,
// as when firmware for one board drives another's display.
const panelWait = /Wait complete:\s+\S+\s+\((\d+) ms\)/;
const WAIT_RAN_OUT_MS = 10000;
/** Host time from the Home screen to Running, for firmware without [MEM]. */
const SETTLE_MS = 5000;

type Later = (run: () => void, ms: number) => () => void;
const timeout: Later = (run, ms) => { const timer = setTimeout(run, ms); return () => clearTimeout(timer); };

export class Readiness {
  /** Seconds from boot to the Home screen, once it has been drawn. */
  bootSeconds: number | null = null;
  private milestones = 0;
  private lastDrawAt = 0;
  private startedAt = 0;
  private sdFailed = false;
  private waitRanOut = false;
  private ready = false;
  private cancelSettle: (() => void) | null = null;
  private readonly status: (text: string) => void;
  private readonly device: () => string;
  private readonly timing: () => string;
  private readonly now: () => number;
  private readonly later: Later;

  /** `device` names the board ("X3"), `timing` the mode ("Accurate timing"). */
  constructor(status: (text: string) => void, device: () => string, timing: () => string,
    now: () => number = () => performance.now(), later: Later = timeout) {
    this.status = status;
    this.device = device;
    this.timing = timing;
    this.now = now;
    this.later = later;
  }

  /** The device boots afresh: Start, Reset, a changed card. */
  restart() {
    this.stop();
    this.milestones = 0;
    this.lastDrawAt = 0;
    this.bootSeconds = null;
    this.sdFailed = false;
    this.waitRanOut = false;
    this.ready = false;
    this.startedAt = this.now();
  }

  /** The run ended: nothing more is said about it. */
  stop() {
    this.cancelSettle?.();
    this.cancelSettle = null;
  }

  feed(lines: string[]) {
    const prefix = () => `${this.device()} · ${this.timing()}`;
    for (const line of lines) {
      if (line.startsWith('ESP-ROM:') && this.milestones) {
        // The guest rebooted (a crash, a firmware reset): count again.
        this.restart();
        this.status(`${prefix()} · Restarting…`);
      }
      // CrossPoint reports a card it cannot mount; its Home is then an error screen.
      if (/SD card initialization failed/.test(line)) this.sdFailed = true;
      // Stock X4 Pro firmware powers itself off unless Power is held at boot.
      if (/reject physical boot before init|POWERON without key/.test(line) && this.bootSeconds === null) {
        this.status(`${this.device()} is off · hold Power (P) for a moment to turn it on`);
      }
      if (/main_task: Returned from app_main\(\)/.test(line) && this.bootSeconds === null) {
        this.status(`${prefix()} · Firmware running`);
      }
      const wait = panelWait.exec(line);
      if (wait && Number(wait[1]) >= WAIT_RAN_OUT_MS) {
        // Not a refresh: nothing was drawn.
        if (!this.waitRanOut) {
          this.waitRanOut = true;
          this.stop();
          this.status(`${this.device()} · The display did not answer the firmware (a ${Math.round(Number(wait[1]) / 1000)} s wait ran out) · is this firmware for this device?`);
        }
        continue;
      }
      if (refreshDone.test(line)) {
        this.lastDrawAt = this.now();
        if (++this.milestones === 3) {
          this.bootSeconds = (this.lastDrawAt - this.startedAt) / 1000;
          if (!this.sdFailed && !this.waitRanOut) {
            this.status(`${prefix()} · Home drawn in ${this.bootSeconds.toFixed(2)} s · finishing startup…`);
            this.cancelSettle = this.later(() => this.settle(), SETTLE_MS);
          }
        }
      }
      if (line.includes('[MEM]')) {
        if (this.sdFailed) {
          this.stop();
          this.status(`${this.device()} · The firmware could not mount the SD card · check it in the SD card panel`);
        } else if (this.bootSeconds !== null) {
          this.stop();
          this.ready = true;
          this.status(`${prefix()} · Ready · Home drawn in ${this.bootSeconds.toFixed(2)} s`);
        } else if (this.milestones && this.now() - this.lastDrawAt > 3000) {
          // Firmware that resumes a book or an app draws fewer full refreshes
          // than a boot to Home; once the screen is quiet and it reports in, it runs.
          this.ready = true;
          this.status(`${prefix()} · Ready`);
        }
      }
    }
  }

  /** Home was drawn a while ago and no memory report came: the firmware is
      not CrossPoint, or not one that reports. It runs. */
  private settle() {
    this.cancelSettle = null;
    if (this.ready || this.sdFailed || this.waitRanOut || this.bootSeconds === null) return;
    this.status(`${this.device()} · ${this.timing()} · Running · Home drawn in ${this.bootSeconds.toFixed(2)} s`);
  }
}
