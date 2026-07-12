import { describe, expect, test } from "bun:test";

import { waitForKmuxChild } from "./kmux-child-process";

function deferred<T>() {
  let resolve: (value: T) => void = () => {};
  const promise = new Promise<T>((done) => {
    resolve = done;
  });
  return { promise, resolve };
}

describe("waitForKmuxChild", () => {
  test("does not settle after timeout until the killed child exits", async () => {
    const childExited = deferred<number>();
    const killCalled = deferred<void>();
    let settled = false;
    const waiting = waitForKmuxChild(
      {
        exited: childExited.promise,
        kill() {
          killCalled.resolve();
        },
      },
      0,
      1000,
    );
    void waiting.then(
      () => {
        settled = true;
      },
      () => {
        settled = true;
      },
    );

    await killCalled.promise;
    await Promise.resolve();
    expect(settled).toBe(false);

    childExited.resolve(143);
    expect(waiting).rejects.toThrow("kmux command timed out");
  });
});
