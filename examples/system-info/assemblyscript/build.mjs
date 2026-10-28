import { mkdir, readFile, writeFile } from "node:fs/promises";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";

// Use the current Node executable directly. No shell or lifecycle hook is needed.
const root = fileURLToPath(new URL(".", import.meta.url));
const compiler = fileURLToPath(
  import.meta.resolve("assemblyscript/bin/asc.js"),
);
const result = spawnSync(
  process.execPath,
  [
    compiler,
    "assembly/index.ts",
    "--config",
    "asconfig.json",
    "--target",
    "release",
  ],
  {
    cwd: root,
    stdio: "inherit",
    shell: false,
    windowsHide: true,
    // json-as 1.6.1's SWAR writer corrupts some trailing surrogate pairs.
    // Its supported scalar mode preserves Unicode in portable config. These
    // small teaching reports do not need a throughput-oriented JSON writer.
    env: { ...process.env, JSON_MODE: "NAIVE" },
  },
);
if (result.error) throw result.error;
if (result.status !== 0) process.exit(result.status ?? 1);

// A build creates a disabled teaching bundle. Never overwrite a user's state.
await mkdir(new URL("build/", import.meta.url), { recursive: true });
const state = await readFile(
  new URL("system-info.state.json", import.meta.url),
);
try {
  await writeFile(
    new URL("build/system-info.state.json", import.meta.url),
    state,
    { flag: "wx" },
  );
} catch (error) {
  if (error.code !== "EEXIST") throw error;
}
