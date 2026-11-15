import { spawn, type ChildProcess } from "node:child_process";
import { createInterface } from "node:readline";

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

/** The port in a JSON log line of the server saying it is ready (`"addr": "127.0.0.1:55476"`), if it is that line. */
function readyPort(line: string): number | null {
  try {
    const event = JSON.parse(line) as { fields?: { message?: string; addr?: string } };
    const fields = event.fields;
    if (!fields?.message?.startsWith("Grafeo Server ready") || !fields.addr) return null;
    const port = Number(fields.addr.slice(fields.addr.lastIndexOf(":") + 1));
    return Number.isInteger(port) && port > 0 ? port : null;
  } catch {
    return null;
  }
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
 * Starts the grafeo-server binary (in memory unless `extraArgs` say otherwise)
 * and waits until `/health` answers. The server binds port 0 and its ready
 * line names the port it got, so two workers never share a port and a test
 * never talks to another worker's server.
 */
export async function startServer(
  auth: Auth,
  extraArgs: string[] = [],
  /** The address to bind and to open the Studio on; loopback by default. */
  host = "127.0.0.1",
): Promise<RunningServer> {
  const bin = process.env.GRAFEO_SERVER_BIN;
  if (!bin) {
    throw new Error("set GRAFEO_SERVER_BIN to a grafeo-server binary built with the Studio");
  }
  const child: ChildProcess = spawn(
    bin,
    ["--host", host, "--port", "0", "--log-format", "json", ...authArgs(auth), ...extraArgs],
    { stdio: ["ignore", "pipe", "pipe"] },
  );
  let stderr = "";
  child.stderr?.on("data", (chunk: Buffer) => {
    stderr += chunk.toString();
  });
  const deadline = Date.now() + 15_000;
  const port = await new Promise<number>((resolve, reject) => {
    const lines = createInterface({ input: child.stdout! });
    const timer = setTimeout(() => {
      child.kill();
      reject(new Error(`grafeo-server did not say it was ready: ${stderr}`));
    }, 15_000);
    lines.on("line", (line) => {
      const found = readyPort(line);
      if (found !== null) {
        clearTimeout(timer);
        resolve(found);
      }
    });
    child.once("exit", (code) => {
      clearTimeout(timer);
      reject(new Error(`grafeo-server exited with ${code}: ${stderr}`));
    });
  });
  // Keep reading the log, so a full pipe never blocks the server.
  child.stdout?.resume();
  const base = `http://${host}:${port}`;
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
