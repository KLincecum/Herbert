// Exercise the shipped local-only plugin through real CLI and MCP downloads.
// A loopback server supplies admission only and rejects every collection route.
import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { once } from 'node:events';
import { copyFile, mkdtemp, mkdir, readFile, writeFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, resolve, dirname, basename } from 'node:path';
import { randomUUID, createHash } from 'node:crypto';
import { Client, successful } from '../../scripts/test-client.mjs';
const [executable, module, evidence] = process.argv.slice(2);
assert.ok(executable && module && evidence);
const root = await mkdtemp(join(tmpdir(), 'herbert-local-example-'));
const calls = [];
const checks = [];
const clients = [];
const server = createServer(async (request, response) => {
  for await (const chunk of request) { /* Drain the admission request. */ }
  const method = new URL(request.url, 'http://127.0.0.1').pathname.split('/').at(-1);
  calls.push(method);
  let value;
  if (method === 'GetDatabaseVersion') value = { returnObj: 'fixture-5.2.100.0' };
  else if (method === 'GetUserFile' || method === 'GetByID') value = { returnObj: { UserFile: [{ UserID: 'manager', SecurityMgr: true, UserDisabled: false, LockedOut: false }] } };
  else { response.writeHead(500); response.end('Unexpected collection route'); return; }
  response.writeHead(200, { 'content-type': 'application/json' });
  response.end(JSON.stringify(value));
});
server.listen(0, '127.0.0.1');
await once(server, 'listening');
try {
  for (const mode of ['cli', 'mcp']) {
    const plugins = join(root, mode, 'plugins');
    await mkdir(plugins, { recursive: true });
    await copyFile(module, join(plugins, 'example.wasm'));
    const client = new Client(executable, mode, join(root, mode, 'state'), plugins, join(root, mode, 'coordination'));
    clients.push(client);
    await client.initialize();
    const cap = successful(await client.call('capabilities'));
    assert.equal(cap.categories.length, 1);
    const category = cap.categories[0].id;
    successful(await client.call('plugin.configure', { category, values: { enabled: true, demoCheckbox: false, demoChoice: 'alpha', demoFilter: 'Qualification Ω 😀' }, selected: true }));
    successful(await client.call('connection.connect', { url: 'http://127.0.0.1:' + server.address().port, company: 'FIXTURE', authMode: 'basicApiKey', credentialSource: { type: 'inline', credentials: { username: 'manager', password: randomUUID(), apiKey: 'fixture-key' } } }));
    const before = calls.length;
    const start = successful(await client.call('backup.start', { requestId: randomUUID(), destination: join(root, mode, 'output'), outputMode: 'separate', categories: [category], temporaryAccess: false }));
    let journal;
    do {
      journal = successful(await client.call('run.status', { id: start.runId }));
      if (journal.executionActive !== false || !['completed','partial','failed','canceled'].includes(journal.phase)) await new Promise(r => setTimeout(r, 20));
    } while (journal.executionActive !== false || !['completed','partial','failed','canceled'].includes(journal.phase));
    assert.equal(journal.phase, 'completed');
    assert.equal(journal.artifacts.length, 1);
    const artifact = journal.artifacts[0];
    assert.equal(artifact.acquisition, 'collected');
    assert.deepEqual(artifact.diagnostics, []);
    const files = [];
    for (const file of artifact.files) {
      const bytes = await readFile(join(start.destination, file.path));
      assert.equal(bytes.length, file.bytes);
      assert.equal(createHash('sha256').update(bytes).digest('hex'), file.sha256);
      files.push(file.path);
    }
    const state = JSON.parse(await readFile(join(plugins, 'example.state.json'), 'utf8'));
    assert.equal(state.status.runCount, 1);
    assert.equal(state.status.lastReport.facts.platform.os, 'windows');
    assert.equal(state.status.lastReport.facts.pluginAbi, 1);
    assert.equal(calls.length, before, 'A system-info download must not call Epicor');
    checks.push({ mode, category, files, phase: journal.phase, runCount: state.status.runCount, collectionRequests: calls.length - before });
    successful(await client.call('connection.disconnect'));
    await client.close();
  }
  await mkdir(dirname(resolve(evidence)), { recursive: true });
  await writeFile(evidence, JSON.stringify({ passed: true, proofClass: 'Real CLI/MCP with controlled admission; no live Epicor collection', checks }, null, 2) + '\n');
  console.log(JSON.stringify({ passed: true, checks: checks.length }));
} finally {
  for (const client of clients) if (!client.child.stdin.writableEnded) await client.close();
  await new Promise(r => server.close(r));
  assert.equal(dirname(resolve(root)).toLowerCase(), resolve(tmpdir()).toLowerCase());
  assert.ok(basename(root).startsWith('herbert-local-example-'));
  await rm(root, { recursive: true });
}
