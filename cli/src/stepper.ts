/**
 * Multi-step progress tracker for CLI commands.
 * Shows current step, elapsed time, and clear phase boundaries.
 *
 *   ● Step 1/8 · Checking Azure credentials...        [3s]
 *   ✓ Step 1/8 · Azure credentials verified            [3s]
 *   ● Step 2/8 · Creating resource group...            [5s]
 */

import chalk from "chalk";
import ora, { type Ora } from "ora";

const BLUE = chalk.hex("#0078D4");

export interface StepperOptions {
  totalSteps: number;
  /** Show elapsed time per step (default: true) */
  showElapsed?: boolean;
}

export class Stepper {
  private total: number;
  private current = 0;
  private stepStart = 0;
  private overallStart: number;
  private spinner: Ora;
  private showElapsed: boolean;
  private warnings: string[] = [];

  constructor(opts: StepperOptions) {
    this.total = opts.totalSteps;
    this.showElapsed = opts.showElapsed ?? true;
    this.overallStart = Date.now();
    this.spinner = ora({ color: "cyan", prefixText: "" });
  }

  /** Format elapsed time as human-readable string */
  private elapsed(): string {
    if (!this.showElapsed) return "";
    const ms = Date.now() - this.stepStart;
    if (ms < 1000) return chalk.dim(` [<1s]`);
    const secs = Math.floor(ms / 1000);
    if (secs < 60) return chalk.dim(` [${secs}s]`);
    const mins = Math.floor(secs / 60);
    return chalk.dim(` [${mins}m ${secs % 60}s]`);
  }

  private prefix(): string {
    return chalk.dim(`  ${this.current}/${this.total}`);
  }

  /** Start a new step — automatically completes the previous one */
  step(text: string): void {
    if (this.current > 0) {
      // Complete previous step silently (caller should use .done() for explicit completion)
      this.spinner.stop();
    }
    this.current++;
    this.stepStart = Date.now();
    this.spinner.prefixText = this.prefix();
    this.spinner.start(text);
  }

  /** Update the current step's text (for sub-phases) */
  update(text: string): void {
    this.spinner.text = text;
  }

  /** Mark current step as successful */
  done(text?: string): void {
    if (text) this.spinner.text = text;
    this.spinner.prefixText = this.prefix();
    this.spinner.succeed(this.spinner.text + this.elapsed());
  }

  /** Mark current step as warning (non-fatal skip) */
  warn(text: string): void {
    this.spinner.prefixText = this.prefix();
    this.spinner.warn(text + this.elapsed());
    this.warnings.push(text);
  }

  /** Mark current step as failed */
  fail(text: string): void {
    this.spinner.prefixText = this.prefix();
    this.spinner.fail(text + this.elapsed());
  }

  /** Skip a step (increment counter without running) */
  skip(text: string): void {
    this.current++;
    this.stepStart = Date.now();
    console.log(`${this.prefix()} ${chalk.dim("○")} ${chalk.dim(text)} ${chalk.dim("[skipped]")}`);
  }

  /** Print an indented detail line within the current step (sub-item status) */
  detail(icon: "ok" | "skip" | "new" | "info", text: string): void {
    this.spinner.stop();
    const icons = {
      ok:   chalk.green("✓"),
      skip: chalk.dim("○"),
      new:  chalk.cyan("✦"),
      info: chalk.dim("·"),
    };
    console.log(`           ${icons[icon]} ${text}`);
    this.spinner.start(this.spinner.text);
  }

  /** Print overall summary */
  summary(): void {
    const totalMs = Date.now() - this.overallStart;
    const secs = Math.floor(totalMs / 1000);
    const mins = Math.floor(secs / 60);
    const timeStr = mins > 0 ? `${mins}m ${secs % 60}s` : `${secs}s`;
    console.log(chalk.dim(`\n  Completed ${this.current} steps in ${timeStr}`));
    if (this.warnings.length > 0) {
      console.log(chalk.yellow(`  ${this.warnings.length} warning(s) — review above`));
    }
  }

  /** Get the total elapsed time in seconds */
  get totalElapsed(): number {
    return Math.floor((Date.now() - this.overallStart) / 1000);
  }

  /** Stop the spinner (for cleanup on error) */
  stop(): void {
    this.spinner.stop();
  }
}

/**
 * Print a boxed banner at the start of a command.
 */
export function banner(title: string, subtitle?: string): void {
  const line = "─".repeat(48);
  console.log(BLUE(`\n  ╭${line}╮`));
  console.log(BLUE(`  │`) + chalk.bold(`  ${title.padEnd(46)}`) + BLUE(`│`));
  if (subtitle) {
    console.log(BLUE(`  │`) + chalk.dim(`  ${subtitle.padEnd(46)}`) + BLUE(`│`));
  }
  console.log(BLUE(`  ╰${line}╯\n`));
}

/**
 * Print a section header.
 */
export function section(title: string): void {
  console.log(BLUE(`\n  ── ${title} ${"─".repeat(Math.max(0, 43 - title.length))}`));
}

/**
 * Print a key-value pair in the summary.
 */
export function kvLine(key: string, value: string, icon?: string): void {
  const k = key.padEnd(12);
  const prefix = icon ? `  ${icon} ` : "  ";
  console.log(`${prefix}${k} ${chalk.bold(value)}`);
}

/**
 * Print a security check line.
 */
export function checkLine(ok: boolean, text: string): void {
  const icon = ok ? chalk.green("✓") : chalk.yellow("○");
  console.log(`  ${icon} ${text}`);
}
