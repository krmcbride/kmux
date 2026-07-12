/** Lifecycle handling for one short-lived kmux child process. */

export type KmuxChildProcess = {
  exited: Promise<number>;
  kill(signal?: number): void;
};

/**
 * Bound one child command without allowing its caller to outlive the process.
 *
 * A timeout signals termination, then escalates to an unignorable signal, but
 * the promise settles only after `exited` confirms the old child is gone.
 */
export function waitForKmuxChild(
  child: KmuxChildProcess,
  timeoutMs: number,
  killGraceMs = 250,
): Promise<number> {
  return new Promise((resolve, reject) => {
    let timedOut = false;
    let timeoutError: unknown = new Error("kmux command timed out");
    let forceTimer: ReturnType<typeof setTimeout> | undefined;
    const timer = setTimeout(
      () => {
        timedOut = true;
        try {
          child.kill();
        } catch (error) {
          timeoutError = error;
        }
        forceTimer = setTimeout(
          () => {
            try {
              child.kill(9);
            } catch (error) {
              timeoutError = error;
            }
          },
          Math.max(0, killGraceMs),
        );
      },
      Math.max(0, timeoutMs),
    );

    void child.exited.then(
      (exitCode) => {
        clearTimeout(timer);
        if (forceTimer !== undefined) clearTimeout(forceTimer);
        if (timedOut) reject(timeoutError);
        else resolve(exitCode);
      },
      (error) => {
        clearTimeout(timer);
        if (forceTimer !== undefined) clearTimeout(forceTimer);
        reject(timedOut ? timeoutError : error);
      },
    );
  });
}
