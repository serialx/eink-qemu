/* Readiness from the firmware log: CrossPoint's boot milestones (completed
   panel refreshes, then its memory report), a reboot, a card it could not
   mount, and the stock X4 Pro firmware's wait for a Power press. Both front
   ends feed it the console lines; tests/test_readiness.mjs feeds it too. */

// Panel refresh completions: UC8179/UC8279 boards log their DRF command,
// the X4 (SSD1677) a plain refresh.
const refreshDone = /Wait complete:\s+(?:(?:8179|8279|8279x4|X3)_DRF|refresh)\b/;

export class Readiness {
  /** Seconds from boot to the Home screen, once it has been drawn. */
  bootSeconds: number | null = null;
  private milestones = 0;
  private lastDrawAt = 0;
  private startedAt = 0;
  private sdFailed = false;
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

  /** The device boots afresh: Start, Reset, a changed card. */
  restart() {
    this.milestones = 0;
    this.lastDrawAt = 0;
    this.bootSeconds = null;
    this.sdFailed = false;
    this.startedAt = this.now();
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
      if (refreshDone.test(line)) {
        this.lastDrawAt = this.now();
        if (++this.milestones === 3) {
          this.bootSeconds = (this.lastDrawAt - this.startedAt) / 1000;
          if (!this.sdFailed) this.status(`${prefix()} · Home drawn in ${this.bootSeconds.toFixed(2)} s · finishing startup…`);
        }
      }
      if (line.includes('[MEM]')) {
        if (this.sdFailed) {
          this.status(`${this.device()} · The firmware could not mount the SD card · check it in the SD card panel`);
        } else if (this.bootSeconds !== null) {
          this.status(`${prefix()} · Ready · Home drawn in ${this.bootSeconds.toFixed(2)} s`);
        } else if (this.milestones && this.now() - this.lastDrawAt > 3000) {
          // Firmware that resumes a book or an app draws fewer full refreshes
          // than a boot to Home; once the screen is quiet and it reports in, it runs.
          this.status(`${prefix()} · Ready`);
        }
      }
    }
  }
}
