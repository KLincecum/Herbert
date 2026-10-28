// Drive the actual local frontends in qualification scripts. This helper has no
// credentials or native connection and does not change product operations.
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { createInterface } from "node:readline";

export class Client {
  constructor(executable, mode, state, plugins, coordination) {
    this.mode = mode;
    this.sequence = 0;
    this.stderr = "";
    this.child = spawn(
      executable,
      [mode, ...(state ? ["--state-dir", state] : []), "--plugin-dir", plugins,
        ...(coordination ? ["--coordination-dir", coordination] : [])],
      { shell: false, windowsHide: true, stdio: ["pipe", "pipe", "pipe"] },
    );
    this.closed = new Promise((resolve, reject) => {
      this.child.once("error", reject);
      this.child.once("close", (code) => resolve(code));
    });
    this.lines = createInterface({ input: this.child.stdout })[
      Symbol.asyncIterator
    ]();
    this.child.stderr.on("data", (data) => {
      this.stderr += data.toString("utf8");
    });
  }

  async request(input) {
    this.child.stdin.write(JSON.stringify(input) + "\n");
    const line = await this.lines.next();
    if (line.done)
      throw new Error(`Herbert closed before replying: ${this.stderr}`);
    return JSON.parse(line.value);
  }

  async initialize() {
    if (this.mode !== "mcp") return;
    const reply = await this.request({
      jsonrpc: "2.0",
      id: ++this.sequence,
      method: "initialize",
      params: {
        protocolVersion: "2025-11-25",
        capabilities: {},
        clientInfo: { name: "Herbert local qualification", version: "1" },
      },
    });
    assert.equal(reply.error, undefined);
    this.child.stdin.write(
      JSON.stringify({ jsonrpc: "2.0", method: "notifications/initialized" }) +
        "\n",
    );
  }

  async call(method, args = {}) {
    if (this.mode === "cli") {
      return this.request({ schemaVersion: 1, method, arguments: args });
    }
    const reply = await this.request({
      jsonrpc: "2.0",
      id: ++this.sequence,
      method: "tools/call",
      params: { name: method.replaceAll(".", "_"), arguments: args },
    });
    assert.equal(reply.error, undefined);
    return reply.result.structuredContent;
  }

  async close() {
    this.child.stdin.end();
    assert.equal(await this.closed, 0, this.stderr);
  }
}

export function successful(reply) {
  assert.equal(reply.ok, true, JSON.stringify(reply.error));
  return reply.result;
}
