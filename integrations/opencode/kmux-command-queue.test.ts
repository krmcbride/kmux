import { describe, expect, test } from "bun:test";

import { KmuxCommandQueue } from "./kmux-command-queue";

function deferred<T>() {
  let resolve: (value: T) => void = () => {};
  const promise = new Promise<T>((done) => {
    resolve = done;
  });
  return { promise, resolve };
}

describe("KmuxCommandQueue", () => {
  test("runs commands one at a time in insertion order", async () => {
    const firstStarted = deferred<void>();
    const releaseFirst = deferred<number>();
    const calls: string[] = [];
    let active = 0;
    let maximumActive = 0;
    const queue = new KmuxCommandQueue(async (command) => {
      active += 1;
      maximumActive = Math.max(maximumActive, active);
      calls.push(command[0] ?? "missing");
      if (command[0] === "working") {
        firstStarted.resolve();
        const exitCode = await releaseFirst.promise;
        active -= 1;
        return exitCode;
      }
      active -= 1;
      return 0;
    });

    const first = queue.enqueue(["working"]);
    const second = queue.enqueue(["done"]);
    await firstStarted.promise;

    expect(calls).toEqual(["working"]);
    releaseFirst.resolve(0);
    await Promise.all([first, second, queue.drain()]);

    expect(calls).toEqual(["working", "done"]);
    expect(maximumActive).toBe(1);
  });

  test("continues after rejected and non-zero commands", async () => {
    const calls: string[] = [];
    const queue = new KmuxCommandQueue(async (command) => {
      const name = command[0] ?? "missing";
      calls.push(name);
      if (name === "rejected") throw new Error("spawn failed");
      return name === "non-zero" ? 1 : 0;
    });

    const rejected = queue.enqueue(["rejected"]);
    const nonZero = queue.enqueue(["non-zero"]);
    const succeeded = queue.enqueue(["succeeded"]);

    expect(await rejected).toMatchObject({ ok: false });
    expect(await nonZero).toEqual({ ok: false, exitCode: 1 });
    expect(await succeeded).toEqual({ ok: true, exitCode: 0 });
    expect(calls).toEqual(["rejected", "non-zero", "succeeded"]);
  });

  test("supports bounded draining", async () => {
    const release = deferred<number>();
    const queue = new KmuxCommandQueue(() => release.promise);
    void queue.enqueue(["blocked"]);

    expect(await queue.drain(1)).toBe(false);
    release.resolve(0);
    expect(await queue.drain(100)).toBe(true);
  });

  test("keeps independent plugin instances isolated", async () => {
    const releaseOld = deferred<number>();
    const oldQueue = new KmuxCommandQueue(() => releaseOld.promise);
    const newCalls: string[] = [];
    const newQueue = new KmuxCommandQueue(async (command) => {
      newCalls.push(command[0] ?? "missing");
      return 0;
    });

    void oldQueue.enqueue(["old-working"]);
    expect(await newQueue.enqueue(["new-working"])).toEqual({
      ok: true,
      exitCode: 0,
    });
    expect(newCalls).toEqual(["new-working"]);

    releaseOld.resolve(0);
    await oldQueue.drain();
  });
});
