/** Lifecycle handling for one short-lived kmux child process. */

const SIGTERM = 15;
const SIGKILL = 9;

/**
 * Minimal process handle required by the child lifecycle policy.
 *
 * Values returned by `Bun.spawn` satisfy this shape structurally. Defining the
 * capability here avoids depending on Bun's full subprocess type and lets tests
 * provide a small fake.
 */
export type KmuxChildProcess = {
  exited: Promise<number>;
  kill(signal?: number): void;
};

/**
 * Bound one child command without allowing its caller to outlive the process.
 *
 * After `gracefulTimeoutMs`, SIGTERM requests a graceful shutdown. If the
 * process remains alive for `killTimeoutMs`, SIGKILL forces it to exit. The
 * promise settles only after `exited` confirms the old child is gone.
 */
export function waitForKmuxChild(
  child: KmuxChildProcess,
  gracefulTimeoutMs: number,
  killTimeoutMs = 250,
): Promise<number> {
  const normalizedGracefulTimeoutMs = Math.max(0, gracefulTimeoutMs);
  const normalizedKillTimeoutMs = Math.max(0, killTimeoutMs);

  return new Promise((resolve, reject) => {
    let timedOut = false;
    let timeoutError: unknown = new Error("kmux command timed out");
    let forceKillTimer: ReturnType<typeof setTimeout> | undefined;
    const gracefulShutdownTimer = setTimeout(() => {
      timedOut = true;
      try {
        child.kill(SIGTERM);
      } catch (error) {
        timeoutError = error;
      }
      forceKillTimer = setTimeout(() => {
        try {
          child.kill(SIGKILL);
        } catch (error) {
          timeoutError = error;
        }
      }, normalizedKillTimeoutMs);
    }, normalizedGracefulTimeoutMs);

    void child.exited.then(
      (exitCode) => {
        clearTimeout(gracefulShutdownTimer);
        if (forceKillTimer !== undefined) {
          clearTimeout(forceKillTimer);
        }
        if (timedOut) {
          reject(timeoutError);
        } else {
          resolve(exitCode);
        }
      },
      (error) => {
        clearTimeout(gracefulShutdownTimer);
        if (forceKillTimer !== undefined) {
          clearTimeout(forceKillTimer);
        }
        reject(timedOut ? timeoutError : error);
      },
    );
  });
}
