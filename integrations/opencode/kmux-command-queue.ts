/** Ordered execution for short-lived kmux integration commands. */

export type KmuxCommandResult =
  | { ok: true; exitCode: 0 }
  | { ok: false; exitCode: number }
  | { ok: false; error: unknown };

export type KmuxCommandRunner = (command: ReadonlyArray<string>) => Promise<number>;

/**
 * Run one kmux command at a time in insertion order.
 *
 * Failures are returned as values so one failed child cannot poison the queue
 * or prevent a later lifecycle transition from being reported.
 */
export class KmuxCommandQueue {
  private tail: Promise<void> = Promise.resolve();

  constructor(private readonly runner: KmuxCommandRunner) {}

  enqueue(command: ReadonlyArray<string>): Promise<KmuxCommandResult> {
    const input = [...command];
    const result = this.tail.then(async () => {
      try {
        const exitCode = await this.runner(input);
        return exitCode === 0
          ? ({ ok: true, exitCode } as const)
          : ({ ok: false, exitCode } as const);
      } catch (error) {
        return { ok: false, error } as const;
      }
    });
    this.tail = result.then(() => undefined);
    return result;
  }

  drain(timeoutMs?: number): Promise<boolean> {
    if (timeoutMs === undefined) return this.tail.then(() => true);
    return new Promise((resolve) => {
      const timer = setTimeout(() => resolve(false), Math.max(0, timeoutMs));
      void this.tail.then(() => {
        clearTimeout(timer);
        resolve(true);
      });
    });
  }
}
