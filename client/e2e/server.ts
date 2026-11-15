import { spawn, type ChildProcess } from "node:child_process";
import { createServer } from "node:net";

/** How a test's server authenticates. */
export type Auth =
  | { kind: "none" }
  | { kind: "basic"; user: string; password: string }
  | { kind: "token"; token: string }
  | { kind: "basic+token"; user: string; password: string; token: string };

export interface RunningServer {
  base: string;
  stop: () => Promise<void>;
}

/** A free TCP port on loopback. */
async function freePort(): Promise<number> {
  return new Promise((resolve, reject) => {
    const srv = createServer();
    srv.unref();
    srv.on("error", reject);
    srv.listen(0, "127.0.0.1", () => {
      const address = srv.address();
      if (address && typeof address === "object") {
        const { port } = address;
        srv.close(() => resolve(port));
      } else {
        srv.close(() => reject(new Error("no port")));
      }
    });
  });
}

function authArgs(auth: Auth): string[] {
  switch (auth.kind) {
    case "none":
      return [];
    case "basic":
      return ["--auth-user", auth.user, "--auth-password", auth.password];
    case "token":
      return ["--auth-token", auth.token];
    case "basic+token":
      return [
        "--auth-user",
        auth.user,
        "--auth-password",
        auth.password,
        "--auth-token",
        auth.token,
      ];
  }
}

/**
 * Starts the grafeo-server binary on a free loopback port (in memory unless
 * `extraArgs` say otherwise) and waits until `/health` answers.
 */
export async function startServer(auth: Auth, extraArgs: string[] = []): Promise<RunningServer> {
  const bin = process.env.GRAFEO_SERVER_BIN;
  if (!bin) {
    throw new Error("set GRAFEO_SERVER_BIN to a grafeo-server binary built with the Studio");
  }
  const port = await freePort();
  const child: ChildProcess = spawn(bin, ["--host", "127.0.0.1", "--port", String(port), ...authArgs(auth), ...extraArgs], {
    stdio: ["ignore", "ignore", "pipe"],
  });
  let stderr = "";
  child.stderr?.on("data", (chunk: Buffer) => {
    stderr += chunk.toString();
  });
  const base = `http://127.0.0.1:${port}`;
  const deadline = Date.now() + 15_000;
  for (;;) {
    if (child.exitCode !== null) {
      throw new Error(`grafeo-server exited with ${child.exitCode}: ${stderr}`);
    }
    try {
      const res = await fetch(`${base}/health`);
      if (res.ok) break;
    } catch {
      // not listening yet
    }
    if (Date.now() > deadline) {
      child.kill();
      throw new Error(`grafeo-server did not answer /health: ${stderr}`);
    }
    await new Promise((r) => setTimeout(r, 100));
  }
  return {
    base,
    stop: () =>
      new Promise<void>((resolve) => {
        if (child.exitCode !== null) return resolve();
        child.once("exit", () => resolve());
        child.kill();
      }),
  };
}
