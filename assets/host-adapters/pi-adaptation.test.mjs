// Pure mocks + disposable files. Never import or execute Pi / its SDK.
import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import os from 'node:os';
import { EventEmitter } from 'node:events';
import { syncBuiltinESMExports } from 'node:module';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { createHash } from 'node:crypto';
import childProcess from 'node:child_process';
import realPiAdaptation from './pi-adaptation.js';

const marker = '[PLEAT managed adaptation:';
const id = (dir) => {
  const s = fs.lstatSync(dir, { bigint: true });
  return `${s.dev}:${s.ino}`;
};

const readerResponse = (request, text) => ({ ...request, text,
  digest: createHash('sha256').update(text).digest('hex') });

// Ordinary fixture reader only: NOT an openat implementation or a production fallback.
// FD-relative confinement is exercised by the controller's Rust/native fixtures.
function mockReadAdaptation(request) {
  const local = path.join(request.sourceRoot, 'local-data');
  assert.equal(id(request.sourceRoot), request.sourceIdentity);
  assert.equal(id(local), request.localDataIdentity);
  assert.ok(fs.lstatSync(local).isDirectory());
  const file = path.join(local, 'ADAPTATION.md');
  const before = fs.lstatSync(file);
  assert.ok(before.isFile() && before.nlink === 1 && before.size <= 65536);
  const fd = fs.openSync(file, fs.constants.O_RDONLY | fs.constants.O_NOFOLLOW | fs.constants.O_NONBLOCK);
  try {
    const bytes = Buffer.alloc(65537);
    const count = fs.readSync(fd, bytes, 0, bytes.length, 0);
    assert.ok(count <= 65536);
    return readerResponse(request, new TextDecoder('utf-8', { fatal: true }).decode(bytes.subarray(0, count)));
  } finally { fs.closeSync(fd); }
}

function piAdaptation(pi, config = {}) {
  return realPiAdaptation(pi, { readAdaptation: mockReadAdaptation, ...config });
}

function fixture(t) {
  const root = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'pleat-pi-hook-')));
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  const manifestPath = path.join(root, 'manifest.json');
  const entries = [];
  const commands = [];
  const host = { commands, tools: [{ name: 'read', sourceInfo: { source: 'builtin', path: 'builtin:read' } }] };
  const subscriptions = [];
  const emitter = new EventEmitter();
  const busErrors = [];
  // Model event-bus.js: handler starts synchronously, despite the async safe wrapper.
  const bus = {
    emit: (channel, data) => emitter.emit(channel, data),
    on(channel, handler) {
      const wrapped = async (data) => { try { await handler(data); } catch (e) { busErrors.push(e); } };
      emitter.on(channel, wrapped);
      const off = () => emitter.off(channel, wrapped);
      subscriptions.push(off);
      return off;
    },
  };
  const extensions = [];
  let sequence = 0;
  const save = (value = { schemaVersion: 1, skills: entries }) => fs.writeFileSync(manifestPath, JSON.stringify(value));
  function skill(label, nativeName = label) {
    const source = path.join(root, 'library', label);
    const installed = path.join(root, 'installed', label);
    fs.mkdirSync(path.join(source, 'local-data'), { recursive: true });
    fs.mkdirSync(path.dirname(installed), { recursive: true });
    fs.writeFileSync(path.join(source, 'SKILL.md'), `---\nname: ${nativeName}\ndescription: fixture\n---\nBODY-${label}`);
    const adap = path.join(source, 'local-data', 'ADAPTATION.md');
    fs.writeFileSync(adap, `ADAP-${label}`);
    fs.symlinkSync(source, installed, 'dir');
    const entry = { nativeName, installationSkillPath: path.join(installed, 'SKILL.md'),
      canonicalSourceRoot: source, sourceIdentity: id(source) };
    entries.push(entry);
    const command = { name: `skill:${nativeName}`, source: 'skill',
      sourceInfo: { path: entry.installationSkillPath, source: 'local', scope: 'project', origin: 'top-level' } };
    commands.push(command);
    save();
    return { entry, source, installed, adap, command };
  }
  function load(config, environment = manifestPath) {
    const handlers = new Map();
    const api = {
      events: bus,
      on(name, handler) {
        const list = handlers.get(name) ?? [];
        list.push(handler);
        handlers.set(name, list);
      },
      getCommands: () => host.commands,
      getAllTools: () => host.tools,
    };
    const old = process.env.PLEAT_PI_ADAPTATION_MANIFEST;
    if (environment === null) delete process.env.PLEAT_PI_ADAPTATION_MANIFEST;
    else process.env.PLEAT_PI_ADAPTATION_MANIFEST = environment;
    try {
      piAdaptation(api, config);
      assert.equal(process.env.PLEAT_PI_ADAPTATION_MANIFEST, environment ?? undefined);
    } finally {
      if (old === undefined) delete process.env.PLEAT_PI_ADAPTATION_MANIFEST;
      else process.env.PLEAT_PI_ADAPTATION_MANIFEST = old;
    }
    extensions.push(handlers);
    return handlers;
  }
  async function emit(type, event = {}) {
    let current = { type, ...event };
    let modified = false;
    for (const handlers of extensions) {
      for (const handler of handlers.get(type) ?? []) {
        const result = await handler(current, { cwd: root });
        if (!result) continue;
        if (type === 'input') {
          if (result.action === 'handled') return result;
          if (result.action === 'transform') {
            current = { ...current, text: result.text, images: result.images ?? current.images };
            modified = true;
          }
        } else if (type === 'tool_result') {
          current = { ...current, ...result };
          if (result.content !== undefined && result.structuredContent === undefined) delete current.structuredContent;
          modified = true;
        }
      }
    }
    return modified ? current : undefined;
  }
  const input = (text, extra = {}) => emit('input', { text, source: 'interactive', ...extra });
  const call = (s, extra = {}) => ({ toolCallId: `call-${++sequence}`, toolName: 'read',
    input: { path: s.entry.installationSkillPath }, ...extra });
  const result = (c, extra = {}) => ({ ...c, isError: false,
    content: [{ type: 'text', text: 'original skill text' }], structuredContent: 'original skill text',
    details: { kept: true }, usage: { test: 1 }, ...extra });
  async function read(s, callExtra = {}, resultExtra = {}) {
    const c = call(s, callExtra);
    await emit('tool_call', c);
    return emit('tool_result', result(c, resultExtra));
  }
  function teardownRuntime() {
    for (const off of subscriptions.splice(0)) off();
    extensions.length = 0;
  }
  save();
  return { root, manifestPath, entries, host, busErrors, skill, save, load, emit, input, call, result, read, teardownRuntime };
}

function nativeAlias(f, skill, relative = '.agents/skills/alias') {
  const installed = path.join(f.root, relative);
  fs.mkdirSync(path.dirname(installed), { recursive: true });
  fs.symlinkSync(skill.source, installed, 'dir');
  const file = path.join(installed, 'SKILL.md');
  skill.command.sourceInfo.path = file;
  return { installed, file, entry: { ...skill.entry, installationSkillPath: file } };
}

test('native same-source winner alias binds manual, automatic and canonical reads', async (t) => {
  const f = fixture(t), a = f.skill('alpha'), alias = nativeAlias(f, a);
  f.load(inline([a.entry]), null);
  const opened = traceOpens(t);
  assert.match((await f.input('/skill:alpha')).text, /ADAP-alpha/);
  assert.equal(opened.includes(alias.file), true);
  assert.equal(opened.includes(a.entry.installationSkillPath), false);
  for (const file of [alias.file, a.entry.installationSkillPath, path.join(a.source, 'SKILL.md')]) {
    await f.emit('agent_end');
    assert.match((await f.read(a, { input: { path: file } })).content[1].text, /ADAP-alpha/);
  }
});

test('native alias fallback groups actual sources before selecting across manifest orders', async (t) => {
  for (const reverse of [false, true]) {
    const f = fixture(t), a = f.skill('alpha', 'shared'), foreign = f.skill('foreign', 'shared');
    const authorizedAlias = nativeAlias(f, a, '.pi/skills/alpha');
    const winner = nativeAlias(f, a, '.agents/skills/winner');
    f.host.commands = [a.command];
    const entries = [foreign.entry, authorizedAlias.entry, a.entry];
    if (reverse) entries.reverse();
    for (const entry of entries) f.load(inline([entry]), null);
    assert.match((await f.input('/skill:shared')).text, /ADAP-alpha/);
    await f.emit('agent_end');
    assert.match((await f.read(a, { input: { path: winner.file } })).content[1].text, /ADAP-alpha/);
    assert.equal(await f.read(foreign), undefined);
    await f.emit('agent_end');
    assert.ok(await f.read(a, { input: { path: authorizedAlias.file } }));
  }
});

test('same physical source across scopes deduplicates by identity and rereads changed ADAP', async (t) => {
  const f = fixture(t), a = f.skill('alpha'), alias = nativeAlias(f, a);
  f.load(inline([a.entry]), null); f.load(inline([alias.entry]), null);
  a.command.sourceInfo.path = a.entry.installationSkillPath;
  assert.ok(await f.read(a));
  a.command.sourceInfo.path = alias.file;
  assert.equal(await f.read(a, { input: { path: alias.file } }), undefined);
  fs.writeFileSync(a.adap, 'CHANGED-SAME-SOURCE');
  assert.match((await f.read(a, { input: { path: alias.file } })).content[1].text, /CHANGED-SAME-SOURCE/);
  assert.equal(await f.read(a), undefined);
});

for (const mode of ['uninstalled', 'retargeted', 'copied', 'wrong-identity']) {
  test(`native alias cannot revive an ${mode} original authorization`, async (t) => {
    const f = fixture(t), a = f.skill('alpha'), b = f.skill('foreign'), alias = nativeAlias(f, a);
    f.host.commands = [a.command];
    if (mode === 'wrong-identity') a.entry.sourceIdentity = '0:0';
    else {
      fs.unlinkSync(a.installed);
      if (mode === 'retargeted') fs.symlinkSync(b.source, a.installed, 'dir');
      if (mode === 'copied') fs.cpSync(a.source, a.installed, { recursive: true });
    }
    f.load(inline([a.entry]), null);
    const opened = traceOpens(t);
    assert.equal(await f.input('/skill:alpha'), undefined);
    assert.equal(await f.read(a, { input: { path: alias.file } }), undefined);
    assert.equal(opened.includes(a.adap), false);
    assert.equal(opened.includes(alias.file), false);
  });
}

test('foreign same-name native alias never borrows a managed source', async (t) => {
  const f = fixture(t), a = f.skill('alpha', 'shared'), b = f.skill('foreign', 'shared');
  const alias = nativeAlias(f, b);
  f.host.commands = [b.command]; f.load(inline([a.entry]), null);
  const opened = traceOpens(t);
  assert.equal(await f.input('/skill:shared'), undefined);
  assert.equal(await f.read(b, { input: { path: alias.file } }), undefined);
  assert.equal(opened.some((p) => p.endsWith('ADAPTATION.md')), false);
});

test('an invalid exact native descriptor cannot fall back to another valid authorizer', async (t) => {
  const f = fixture(t), a = f.skill('alpha'), alias = nativeAlias(f, a);
  f.load(inline([a.entry, { ...alias.entry, sourceIdentity: '0:0' }]), null);
  assert.equal(await f.input('/skill:alpha'), undefined);
  assert.equal(await f.read(a, { input: { path: alias.file } }), undefined);
});

test('conflicting native winner descriptors cannot be bypassed by same-source alias fallback', async (t) => {
  for (const reverse of [false, true]) {
    const f = fixture(t), a = f.skill('alpha'), alias = nativeAlias(f, a);
    const entries = [a.entry, alias.entry, { ...alias.entry, sourceIdentity: '0:0' }];
    if (reverse) entries.reverse();
    for (const entry of entries) f.load(inline([entry]), null);
    assert.equal(await f.input('/skill:alpha'), undefined);
    assert.equal(await f.read(a, { input: { path: alias.file } }), undefined);
  }
});

for (const mode of ['winner-chain', 'winner-metadata', 'original-uninstalled', 'skill-bytes']) {
  test(`native alias ${mode} change between call and result invalidates the read`, async (t) => {
    const f = fixture(t), a = f.skill('alpha'), alias = nativeAlias(f, a);
    f.load(inline([a.entry]), null);
    const c = f.call(a, { input: { path: alias.file } });
    await f.emit('tool_call', c);
    if (mode === 'winner-chain') {
      fs.renameSync(alias.installed, alias.installed + '-old');
      fs.symlinkSync(a.source, alias.installed, 'dir');
    }
    if (mode === 'winner-metadata') a.command.sourceInfo.path = a.entry.installationSkillPath;
    if (mode === 'original-uninstalled') fs.unlinkSync(a.installed);
    if (mode === 'skill-bytes') fs.appendFileSync(path.join(a.source, 'SKILL.md'), '\nchanged');
    assert.equal(await f.emit('tool_result', f.result(c)), undefined);
  });
}

for (const mode of ['winner-chain', 'winner-metadata', 'original-uninstalled', 'skill-bytes']) {
  test(`native alias ${mode} change inside FD reader discards manual and automatic injection`, async (t) => {
    for (const manual of [true, false]) {
      const f = fixture(t), a = f.skill('alpha'), alias = nativeAlias(f, a);
      let reads = 0;
      f.load({ ...inline([a.entry]), readAdaptation(request) {
        reads++;
        if (mode === 'winner-chain') {
          fs.renameSync(alias.installed, alias.installed + '-old');
          fs.symlinkSync(a.source, alias.installed, 'dir');
        }
        if (mode === 'winner-metadata') a.command.sourceInfo.path = a.entry.installationSkillPath;
        if (mode === 'original-uninstalled') fs.unlinkSync(a.installed);
        if (mode === 'skill-bytes') fs.appendFileSync(path.join(a.source, 'SKILL.md'), '\nchanged');
        return readerResponse(request, 'VALID-READER-BYTES');
      } }, null);
      assert.equal(manual ? await f.input('/skill:alpha')
        : await f.read(a, { input: { path: alias.file } }), undefined);
      assert.equal(reads, 1);
    }
  });
}

test('native alias retains original storage locator after library move', async (t) => {
  const f = fixture(t), a = f.skill('alpha'), alias = nativeAlias(f, a);
  f.load(inline([a.entry]), null);
  const old = path.dirname(a.source), moved = path.join(f.root, 'moved-library');
  fs.renameSync(old, moved); fs.symlinkSync(moved, old, 'dir');
  assert.match((await f.input('/skill:alpha')).text, /ADAP-alpha/);
  await f.emit('agent_end');
  const r = await f.read(a, { input: { path: alias.file } });
  assert.ok(r.content[1].text.includes(path.join(moved, 'alpha')));
  fs.unlinkSync(a.installed);
  await f.emit('agent_end');
  assert.equal(await f.read(a, { input: { path: alias.file } }), undefined);
});

test('native metadata alias must be a checked directory chain and regular SKILL file', async (t) => {
  for (const mode of ['relative', 'uri', 'other-file', 'file-link', 'hardlink', 'cycle']) {
    const f = fixture(t), a = f.skill('alpha'), alias = nativeAlias(f, a);
    if (mode === 'relative') a.command.sourceInfo.path = path.relative(f.root, alias.file);
    if (mode === 'uri') a.command.sourceInfo.path = pathToFileURL(alias.file).href;
    if (mode === 'other-file') a.command.sourceInfo.path = path.join(alias.installed, 'OTHER.md');
    if (mode === 'file-link') {
      fs.unlinkSync(alias.installed); fs.mkdirSync(alias.installed);
      fs.symlinkSync(path.join(a.source, 'SKILL.md'), alias.file);
    }
    if (mode === 'hardlink') fs.linkSync(path.join(a.source, 'SKILL.md'), path.join(f.root, 'hardlink'));
    if (mode === 'cycle') { fs.unlinkSync(alias.installed); fs.symlinkSync(alias.installed, alias.installed); }
    f.load(inline([a.entry]), null);
    assert.equal(await f.input('/skill:alpha'), undefined);
    assert.equal(await f.read(a, { input: { path: alias.file } }), undefined);
  }
});

const openTraces = new WeakMap();
function traceOpens(t) {
  if (openTraces.has(t)) return openTraces.get(t);
  const opened = [];
  openTraces.set(t, opened);
  const original = fs.openSync;
  fs.openSync = function (file, ...args) { opened.push(String(file)); return original.call(this, file, ...args); };
  syncBuiltinESMExports();
  t.after(() => { fs.openSync = original; syncBuiltinESMExports(); });
  return opened;
}

test('a matched native read appends only its adaptation and preserves the original result', async (t) => {
  const f = fixture(t), a = f.skill('alpha'), b = f.skill('beta');
  f.load();
  const opened = traceOpens(t);
  const r = await f.read(a);
  assert.equal(r.content[0].text, 'original skill text');
  assert.match(r.content[1].text, /ADAP-alpha/);
  assert.match(r.structuredContent, /^original skill text[\s\S]*ADAP-alpha/);
  assert.deepEqual(r.details, { kept: true });
  assert.deepEqual(r.usage, { test: 1 });
  assert.equal(r.isError, false);
  assert.equal(opened.filter((p) => p === a.adap).length, 1);
  assert.equal(opened.includes(b.adap), false);
  assert.equal(await f.read(a), undefined);
  await f.emit('agent_end');
  assert.ok(await f.read(a));
});

test('contents are read again on the next actual read, including atomic file replacement', async (t) => {
  const f = fixture(t), a = f.skill('alpha'); f.load();
  assert.match((await f.read(a)).content[1].text, /ADAP-alpha/);
  fs.writeFileSync(a.adap + '.next', 'UPDATED-ADAP');
  fs.renameSync(a.adap + '.next', a.adap);
  assert.match((await f.read(a)).content[1].text, /UPDATED-ADAP/);
  assert.equal(await f.read(a), undefined);
});

test('global/project duplicate extension factories install one handler set; reload releases the claim', async (t) => {
  const f = fixture(t), a = f.skill('alpha');
  assert.ok(f.load().size > 0);
  assert.equal(f.load().size, 0);
  const r = await f.input('/skill:alpha');
  assert.equal(r.text.split(marker).length - 1, 1);
  f.teardownRuntime();
  assert.ok(f.load().size > 0);
  assert.ok(await f.read(a));
  assert.deepEqual(f.busErrors, []);
});

test('deduplication is scoped to the resource-loader bus, not the process', async (t) => {
  const f = fixture(t), g = fixture(t);
  f.skill('alpha'); g.skill('beta'); f.load(); g.load();
  assert.match((await f.input('/skill:alpha')).text, /ADAP-alpha/);
  assert.match((await g.input('/skill:beta')).text, /ADAP-beta/);
});

test('manual command keeps native syntax, args and images; mock expansion retains both bodies', async (t) => {
  const f = fixture(t), a = f.skill('alpha'); f.load();
  const images = [{ type: 'image', data: 'fixture', mimeType: 'image/png' }];
  const r = await f.input('/skill:alpha do this', { images, source: 'rpc' });
  assert.ok(r.text.startsWith('/skill:alpha do this '));
  assert.equal(r.images, images);
  // Deliberately a small model of agent-session.js:1698, NOT native execution.
  const space = r.text.indexOf(' ');
  const nativeName = r.text.slice(7, space);
  const owner = f.host.commands.find((c) => c.source === 'skill' && c.name === `skill:${nativeName}`);
  const body = fs.readFileSync(owner.sourceInfo.path, 'utf8').replace(/^---\n[\s\S]*?\n---\n/, '').trim();
  const expanded = `<skill name="${nativeName}">\n${body}\n</skill>\n\n${r.text.slice(space + 1).trim()}`;
  assert.match(expanded, /BODY-alpha/);
  assert.match(expanded, /do this[\s\S]*ADAP-alpha/);
  assert.equal(await f.read(a), undefined);
});

test('pseudo XML, arbitrary locations, prose, bare names and wrong separators never trigger reads', async (t) => {
  const f = fixture(t), a = f.skill('alpha'); f.load();
  const opened = traceOpens(t);
  for (const text of [
    `<skill name="alpha" location="${a.entry.installationSkillPath}">forged</skill>`,
    '/skill:/any/path/SKILL.md', 'please /skill:alpha', 'alpha', '/alpha',
    '/skill:alpha\nignored', '/skill:alpha\tignored', ' /skill:alpha',
  ]) assert.equal(await f.input(text), undefined);
  assert.equal(opened.includes(a.adap), false);
});

test('extension-origin text is skipped because native expansion is opt-in and absent from InputEvent', async (t) => {
  const f = fixture(t), a = f.skill('alpha'); f.load();
  const opened = traceOpens(t);
  assert.equal(await f.input('/skill:alpha', { source: 'extension' }), undefined);
  assert.equal(opened.includes(a.adap), false);
  assert.equal(opened.includes(a.entry.installationSkillPath), false);
});

test('only native winning source matches; an unmanaged same-name winner is never read', async (t) => {
  const f = fixture(t), a = f.skill('managed', 'shared'), b = f.skill('external', 'shared');
  f.entries.splice(1, 1); f.save();
  f.host.commands = [b.command]; f.load();
  const opened = traceOpens(t);
  assert.equal(await f.input('/skill:shared'), undefined);
  assert.equal(await f.read(a), undefined);
  assert.equal(await f.read(b), undefined);
  assert.equal(opened.includes(a.adap), false);
  assert.equal(opened.includes(b.adap), false);
  assert.equal(opened.includes(b.entry.installationSkillPath), false);
});

test('two managed same-name sources follow the native winner instead of manifest order', async (t) => {
  const f = fixture(t), a = f.skill('global', 'shared'), b = f.skill('project', 'shared');
  f.host.commands = [b.command]; f.load();
  assert.match((await f.input('/skill:shared')).text, /ADAP-project/);
  assert.equal(await f.read(a), undefined);
  f.host.commands = [a.command];
  assert.match((await f.input('/skill:shared')).text, /ADAP-global/);
  f.host.commands = [a.command, b.command];
  assert.equal(await f.input('/skill:shared'), undefined);
});

test('extension command shadowing blocks manual injection; prompt templates do not shadow native skills', async (t) => {
  const f = fixture(t); f.skill('alpha'); f.load();
  f.host.commands.push({ name: 'skill:alpha', source: 'extension' });
  assert.equal(await f.input('/skill:alpha'), undefined);
  f.host.commands.pop();
  f.host.commands.push({ name: 'skill:alpha', source: 'prompt' });
  assert.match((await f.input('/skill:alpha')).text, /ADAP-alpha/);
});

test('failed, unmatched, wrong-parent and duplicate live calls leave the result untouched', async (t) => {
  const f = fixture(t), a = f.skill('alpha'); f.load();
  const c = f.call(a);
  assert.equal(await f.emit('tool_result', f.result(c)), undefined);
  await f.emit('tool_call', c);
  assert.equal(await f.emit('tool_result', f.result(c, { isError: true })), undefined);
  await f.emit('tool_call', c);
  await f.emit('tool_call', c);
  assert.equal(await f.emit('tool_result', f.result(c)), undefined);
  const nested = f.call(a, { parentToolCallId: 'parent' });
  await f.emit('tool_call', nested);
  assert.equal(await f.emit('tool_result', f.result(nested, { parentToolCallId: 'other' })), undefined);
  assert.match((await f.read(a, { parentToolCallId: 'parent' })).structuredContent, /ADAP-alpha/);
});

test('other tool names, changed arguments and overridden read implementations do not inject', async (t) => {
  const f = fixture(t), a = f.skill('alpha'), b = f.skill('beta'); f.load();
  assert.equal(await f.read(a, { toolName: 'bash' }), undefined);
  assert.equal(await f.read(a, {}, { input: { path: b.entry.installationSkillPath } }), undefined);
  f.host.tools = [{ name: 'read', sourceInfo: { source: 'sdk', path: '<sdk:read>' } }];
  assert.equal(await f.read(a), undefined);
  f.host.tools = [{ name: 'read', sourceInfo: { source: 'builtin', path: 'builtin:read' } }];
  const c = f.call(a); await f.emit('tool_call', c);
  f.host.tools[0].sourceInfo = { source: 'extension', path: '/extension.js' };
  assert.equal(await f.emit('tool_result', f.result(c)), undefined);
});

test('native @, tilde, file URL, relative and authorized canonical paths resolve to the same skill', async (t) => {
  const f = fixture(t), a = f.skill('alpha'); f.load();
  assert.ok(await f.read(a, { input: { path: path.relative(f.root, a.entry.installationSkillPath) } }));
  for (const p of ['@' + a.entry.installationSkillPath, pathToFileURL(a.entry.installationSkillPath).href,
    '~/' + path.relative(os.homedir(), a.entry.installationSkillPath),
    'installed/../installed/alpha/SKILL.md', path.join(a.source, 'SKILL.md')]) {
    await f.emit('agent_end');
    assert.ok(await f.read(a, { input: { path: p } }), p);
  }
  assert.equal(await f.read(a, { input: { path: 'https://example.invalid/SKILL.md' } }), undefined);
});

test('changed skill/source/native ownership/manifest between call and result invalidates observation', async (t) => {
  for (const mode of ['skill', 'identity', 'owner', 'manifest']) {
    const f = fixture(t), a = f.skill('alpha'); f.load();
    const c = f.call(a); await f.emit('tool_call', c);
    if (mode === 'skill') fs.appendFileSync(path.join(a.source, 'SKILL.md'), '\nchanged');
    if (mode === 'identity') {
      fs.renameSync(a.source, a.source + '-old');
      fs.mkdirSync(a.source);
      fs.writeFileSync(path.join(a.source, 'SKILL.md'), 'replacement');
    }
    if (mode === 'owner') f.host.commands[0].sourceInfo.path = '/unmanaged/SKILL.md';
    if (mode === 'manifest') { f.entries.length = 0; f.save(); }
    assert.equal(await f.emit('tool_result', f.result(c)), undefined, mode);
  }
});

test('manifest field ordering is irrelevant; malformed, missing and oversized manifests fail closed', async (t) => {
  const f = fixture(t), a = f.skill('alpha'); f.load();
  const c = f.call(a); await f.emit('tool_call', c);
  f.save({ schemaVersion: 1, skills: [Object.fromEntries(Object.entries(a.entry).reverse())] });
  assert.ok(await f.emit('tool_result', f.result(c)));
  for (const value of [{ schemaVersion: 2, skills: [a.entry] },
    { schemaVersion: 1, skills: [{ ...a.entry, sourceIdentity: '0:0' }] },
    ...['', 'alpha\n', 'alpha\u0085', 'x'.repeat(1025)].map((nativeName) =>
      ({ schemaVersion: 1, skills: [{ ...a.entry, nativeName }] }))]) {
    f.save(value);
    assert.equal(await f.input('/skill:alpha'), undefined);
    assert.equal(await f.read(a), undefined);
  }
  fs.writeFileSync(f.manifestPath, '{');
  assert.equal(await f.input('/skill:alpha'), undefined);
  fs.writeFileSync(f.manifestPath, ' '.repeat(1024 * 1024 + 1));
  assert.equal(await f.input('/skill:alpha'), undefined);
  fs.unlinkSync(f.manifestPath);
  assert.equal(await f.input('/skill:alpha'), undefined);
});

test('root identity mismatch and retargeted installation link never read an adaptation', async (t) => {
  const f = fixture(t), a = f.skill('alpha'), b = f.skill('beta'); f.load();
  a.entry.sourceIdentity = b.entry.sourceIdentity; f.save();
  const opened = traceOpens(t);
  assert.equal(await f.input('/skill:alpha'), undefined);
  a.entry.sourceIdentity = id(a.source); f.save();
  fs.unlinkSync(a.installed); fs.symlinkSync(b.source, a.installed, 'dir');
  assert.equal(await f.input('/skill:alpha'), undefined);
  assert.equal(await f.read(a), undefined);
  assert.equal(opened.includes(a.adap), false);
  assert.equal(opened.includes(b.adap), false);
});

test('direct physical installation is supported and a copied installation is rejected', async (t) => {
  const f = fixture(t), a = f.skill('alpha');
  a.entry.installationSkillPath = path.join(a.source, 'SKILL.md');
  a.command.sourceInfo.path = a.entry.installationSkillPath; f.save(); f.load();
  assert.ok(await f.input('/skill:alpha'));
  const copy = path.join(f.root, 'copy'); fs.mkdirSync(copy);
  fs.copyFileSync(a.entry.installationSkillPath, path.join(copy, 'SKILL.md'));
  a.entry.installationSkillPath = path.join(copy, 'SKILL.md');
  a.command.sourceInfo.path = a.entry.installationSkillPath; f.save();
  assert.equal(await f.input('/skill:alpha'), undefined);
});

test('data file links, manifest links and hard links still fail closed', async (t) => {
  for (const mode of ['adap-link', 'local-link', 'skill-link', 'hard-link', 'manifest-link']) {
    const f = fixture(t), a = f.skill('alpha'); f.load();
    if (mode === 'adap-link' || mode === 'skill-link' || mode === 'manifest-link') {
      const file = mode === 'adap-link' ? a.adap : mode === 'skill-link' ? path.join(a.source, 'SKILL.md') : f.manifestPath;
      fs.renameSync(file, file + '-target'); fs.symlinkSync(file + '-target', file);
    } else if (mode === 'local-link') {
      const dir = path.dirname(a.adap); fs.renameSync(dir, dir + '-target'); fs.symlinkSync(dir + '-target', dir, 'dir');
    } else if (mode === 'hard-link') fs.linkSync(a.adap, a.adap + '-hard');
    assert.equal(await f.input('/skill:alpha'), undefined, mode);
    assert.equal(await f.read(a), undefined, mode);
  }
});

function localInstallChain(f, s) {
  const oldRoot = path.join(f.root, 'old-library');
  const storage = path.join(oldRoot, 'skills', 'storage');
  fs.mkdirSync(path.dirname(storage), { recursive: true });
  fs.symlinkSync(path.relative(path.dirname(storage), s.source), storage, 'dir');
  fs.unlinkSync(s.installed);
  fs.symlinkSync(storage, s.installed, 'dir');
  return { oldRoot, storage };
}

test('compatible multi-level installation follows install -> library storage -> canonical source', async (t) => {
  const f = fixture(t), a = f.skill('alpha');
  localInstallChain(f, a); f.load();
  assert.equal(fs.realpathSync(a.installed), a.entry.canonicalSourceRoot);
  assert.match((await f.input('/skill:alpha')).text, /ADAP-alpha/);
  await f.emit('agent_end');
  assert.match((await f.read(a)).content[1].text, /ADAP-alpha/);
});

test('library migration oldroot -> newroot -> storage -> source retains the same authorized endpoint', async (t) => {
  const f = fixture(t), a = f.skill('alpha');
  const { oldRoot } = localInstallChain(f, a); f.load();
  assert.ok(await f.read(a));
  const newRoot = path.join(f.root, 'new-library');
  fs.renameSync(oldRoot, newRoot);
  fs.symlinkSync(newRoot, oldRoot, 'dir');
  assert.equal(fs.realpathSync(a.installed), a.source);
  // No manifest rewrite and no fake canonical library alias.
  assert.equal(a.entry.sourceIdentity, id(a.source));
  assert.match((await f.input('/skill:alpha')).text, /ADAP-alpha/);
  await f.emit('agent_end');
  assert.ok(await f.read(a));
});

test('a compat alias in installation ancestors is accepted only for the authorized endpoint', async (t) => {
  const f = fixture(t), a = f.skill('alpha');
  localInstallChain(f, a);
  const before = path.dirname(a.installed), after = before + '-new';
  fs.renameSync(before, after); fs.symlinkSync(after, before, 'dir');
  f.load();
  assert.ok(await f.input('/skill:alpha'));
  await f.emit('agent_end');
  assert.ok(await f.read(a));
});

test('retargeting an intermediate storage or compat link to another source never reads its ADAP', async (t) => {
  for (const mode of ['storage', 'compat']) {
    const f = fixture(t), a = f.skill('alpha'), b = f.skill('beta');
    const { oldRoot, storage } = localInstallChain(f, a); f.load();
    assert.ok(await f.read(a)); await f.emit('agent_end');
    if (mode === 'storage') {
      fs.unlinkSync(storage); fs.symlinkSync(b.source, storage, 'dir');
    } else {
      fs.renameSync(oldRoot, oldRoot + '-original');
      const other = path.join(f.root, 'other-library');
      fs.mkdirSync(path.join(other, 'skills'), { recursive: true });
      fs.symlinkSync(b.source, path.join(other, 'skills', 'storage'), 'dir');
      fs.symlinkSync(other, oldRoot, 'dir');
    }
    const opened = traceOpens(t);
    opened.length = 0; // Exclude this iteration's successful pre-retarget control read.
    assert.equal(await f.input('/skill:alpha'), undefined);
    assert.equal(await f.read(a), undefined);
    assert.equal(opened.includes(a.adap), false);
    assert.equal(opened.includes(b.adap), false);
  }
});

test('same pathname with a replaced canonical directory identity fails through the chain', async (t) => {
  const f = fixture(t), a = f.skill('alpha'); localInstallChain(f, a); f.load();
  fs.renameSync(a.source, a.source + '-old');
  fs.mkdirSync(path.join(a.source, 'local-data'), { recursive: true });
  fs.writeFileSync(path.join(a.source, 'SKILL.md'), 'NEW-SOURCE');
  fs.writeFileSync(a.adap, 'WRONG-ADAP');
  assert.equal(await f.input('/skill:alpha'), undefined);
  assert.equal(await f.read(a), undefined);
});

test('chain changes between call and result invalidate that observation; next read can use the new valid chain', async (t) => {
  const f = fixture(t), a = f.skill('alpha');
  const { storage } = localInstallChain(f, a); f.load();
  const c = f.call(a); await f.emit('tool_call', c);
  const relay = path.join(f.root, 'relay'); fs.symlinkSync(a.source, relay, 'dir');
  fs.unlinkSync(storage); fs.symlinkSync(relay, storage, 'dir');
  assert.equal(await f.emit('tool_result', f.result(c)), undefined);
  assert.ok(await f.read(a));
});

test('cycles and excessive installation chains fail boundedly', async (t) => {
  for (const mode of ['cycle', 'long']) {
    const f = fixture(t), a = f.skill('alpha');
    const { storage } = localInstallChain(f, a); f.load();
    fs.unlinkSync(storage);
    let target = mode === 'cycle' ? a.installed : a.source;
    if (mode === 'long') {
      for (let i = 0; i < 41; i++) {
        const relay = path.join(f.root, `relay-${i}`);
        fs.symlinkSync(target, relay, 'dir'); target = relay;
      }
    }
    fs.symlinkSync(target, storage, 'dir');
    assert.equal(await f.input('/skill:alpha'), undefined);
    assert.equal(await f.read(a), undefined);
  }
});

test('literal Unicode-space logical paths reach the exact file even with an ASCII-space namesake', async (t) => {
  for (const space of ['\u3000', '\u00a0', '\u202f']) {
    const f = fixture(t), a = f.skill(`project${space}one`, 'alpha'), b = f.skill('project one', 'beta');
    localInstallChain(f, a); f.load();
    assert.match((await f.input('/skill:alpha')).text, /ADAP-project/);
    await f.emit('agent_end');
    const c = f.call(a); await f.emit('tool_call', c);
    assert.ok(c.input.path.startsWith('file://'));
    // Model the verified native order: Unicode-space replacement, THEN URL decoding.
    // Read a real fixture through that result; no SDK execution.
    const nativePath = fileURLToPath(c.input.path.replace(/[\u00a0\u2000-\u200a\u202f\u205f\u3000]/gu, ' '));
    assert.equal(nativePath, a.entry.installationSkillPath);
    const body = fs.readFileSync(nativePath, 'utf8');
    assert.ok(body.includes(`BODY-project${space}one`));
    const opened = traceOpens(t);
    const r = await f.emit('tool_result', f.result(c, { content: [{ type: 'text', text: body }], structuredContent: body }));
    assert.ok(r.content[1].text.includes(`ADAP-project${space}one`));
    assert.equal(opened.includes(b.adap), false);
  }
});

test('Unicode normalization aliases without a literal managed match follow native resolution', async (t) => {
  const f = fixture(t), a = f.skill('project one', 'alpha'); f.load();
  const alias = a.entry.installationSkillPath.replace('project one', 'project\u3000one');
  assert.ok(await f.read(a, { input: { path: alias } }));
});

test('missing, empty, invalid UTF-8, NUL, oversized and non-file adaptations preserve original results', async (t) => {
  for (const mode of ['missing', 'empty', 'utf8', 'nul', 'large', 'directory']) {
    const f = fixture(t), a = f.skill('alpha'); f.load();
    if (mode === 'missing' || mode === 'directory') fs.unlinkSync(a.adap);
    if (mode === 'directory') fs.mkdirSync(a.adap);
    if (mode === 'empty') fs.writeFileSync(a.adap, ' \n');
    if (mode === 'utf8') fs.writeFileSync(a.adap, Buffer.from([0xff]));
    if (mode === 'nul') fs.writeFileSync(a.adap, 'a\0b');
    if (mode === 'large') fs.writeFileSync(a.adap, 'a'.repeat(64 * 1024 + 1));
    const c = f.call(a), original = f.result(c), snapshot = structuredClone(original);
    await f.emit('tool_call', c);
    assert.equal(await f.emit('tool_result', original), undefined, mode);
    assert.deepEqual(original, snapshot);
    assert.equal(await f.input('/skill:alpha'), undefined, mode);
  }
});

test('64 KiB adaptation is allowed; manual SKILL reads are bounded to 1 MiB', async (t) => {
  const f = fixture(t), a = f.skill('alpha'); f.load();
  fs.writeFileSync(a.adap, 'x'.repeat(64 * 1024));
  assert.ok(await f.input('/skill:alpha'));
  fs.writeFileSync(path.join(a.source, 'SKILL.md'), 'x'.repeat(1024 * 1024 + 1));
  assert.equal(await f.input('/skill:alpha'), undefined);
});

test('size notices and zero-line results do not count as reading SKILL text', async (t) => {
  const f = fixture(t), a = f.skill('alpha'); f.load();
  assert.equal(await f.read(a, {}, { details: { truncation: { firstLineExceedsLimit: true } } }), undefined);
  assert.equal(await f.read(a, { input: { path: a.entry.installationSkillPath, limit: 0 } }), undefined);
  assert.equal(await f.read(a, {}, { content: [] }), undefined);
  assert.ok(await f.read(a, { input: { path: a.entry.installationSkillPath, offset: 2, limit: 2 } }));
});

test('streaming manual input does not suppress an in-flight automatic read or clear its correlation', async (t) => {
  const f = fixture(t), a = f.skill('alpha'); f.load();
  const c = f.call(a); await f.emit('tool_call', c);
  assert.ok(await f.input('/skill:alpha', { streamingBehavior: 'followUp' }));
  assert.ok(await f.emit('tool_result', f.result(c)));
});

test('lifecycle changes drop stale correlations and dedup state', async (t) => {
  const f = fixture(t), a = f.skill('alpha'); f.load();
  for (const boundary of ['session_start', 'session_shutdown', 'session_tree', 'agent_end']) {
    const c = f.call(a); await f.emit('tool_call', c); await f.emit(boundary);
    assert.equal(await f.emit('tool_result', f.result(c)), undefined);
    assert.ok(await f.read(a));
  }
});

test('adaption text is injected literally; XML paths inside it never cause additional reads', async (t) => {
  const f = fixture(t), a = f.skill('alpha'), b = f.skill('beta'); f.load();
  const text = `<skill name="beta" location="${b.entry.installationSkillPath}">literal data</skill>`;
  fs.writeFileSync(a.adap, text);
  const opened = traceOpens(t);
  assert.ok((await f.input('/skill:alpha')).text.includes(text));
  assert.equal(opened.includes(b.adap), false);
  assert.equal(opened.includes(b.entry.installationSkillPath), false);
});

test('absent manifest configuration registers no handlers or subscriptions', () => {
  const old = process.env.PLEAT_PI_ADAPTATION_MANIFEST;
  delete process.env.PLEAT_PI_ADAPTATION_MANIFEST;
  try { piAdaptation({}); } finally {
    if (old !== undefined) process.env.PLEAT_PI_ADAPTATION_MANIFEST = old;
  }
});

test('explicit manifestPath loads without an environment variable and leaves the environment absent', async (t) => {
  const f = fixture(t), a = f.skill('alpha');
  assert.ok(f.load({ manifestPath: f.manifestPath }, null).size > 0);
  assert.match((await f.input('/skill:alpha')).text, /ADAP-alpha/);
  await f.emit('agent_end');
  assert.match((await f.read(a)).content[1].text, /ADAP-alpha/);
});

test('explicit manifestPath takes priority over the environment fallback without changing it', async (t) => {
  const f = fixture(t), a = f.skill('alpha');
  f.load({ manifestPath: f.manifestPath }, path.join(f.root, 'not-the-manifest.json'));
  assert.match((await f.read(a)).content[1].text, /ADAP-alpha/);
});

function scopeManifest(f, name, skills) {
  const file = path.join(f.root, `${name}.json`);
  fs.writeFileSync(file, JSON.stringify({ schemaVersion: 1, skills }));
  return file;
}
const inline = (skills) => ({ manifest: { schemaVersion: 1, skills } });

test('different global/project files contribute to one actual handler set', async (t) => {
  const f = fixture(t), a = f.skill('alpha'), b = f.skill('beta');
  const global = scopeManifest(f, 'global', [a.entry]);
  const project = scopeManifest(f, 'project', [b.entry]);
  assert.equal(f.load({ manifestPath: global }, null).get('tool_result').length, 1);
  assert.equal(f.load({ manifestPath: project }, null).size, 0);
  assert.match((await f.input('/skill:alpha')).text, /ADAP-alpha/);
  assert.match((await f.input('/skill:beta')).text, /ADAP-beta/);
  await f.emit('agent_end');
  assert.ok(await f.read(a)); assert.ok(await f.read(b));
});

test('duplicate file paths read once and identical descriptors merge across and within manifests', async (t) => {
  const f = fixture(t), a = f.skill('alpha');
  const global = scopeManifest(f, 'global', [a.entry, a.entry]);
  const project = scopeManifest(f, 'project', [Object.fromEntries(Object.entries(a.entry).reverse())]);
  f.load({ manifestPath: global }, null);
  f.load({ manifestPath: global }, null);
  f.load({ manifestPath: project }, null);
  const opened = traceOpens(t);
  const r = await f.input('/skill:alpha');
  assert.equal(r.text.split(marker).length - 1, 1);
  assert.equal(opened.filter((p) => p === global).length, 1);
  assert.equal(opened.filter((p) => p === project).length, 1);
});

test('removed or invalid project file cannot disable global; restored file contributes on the next hook', async (t) => {
  const f = fixture(t), a = f.skill('alpha'), b = f.skill('beta');
  const global = scopeManifest(f, 'global', [a.entry]);
  const project = scopeManifest(f, 'project', [b.entry]);
  f.load({ manifestPath: global }, null); f.load({ manifestPath: project }, null);
  assert.ok(await f.input('/skill:beta'));
  fs.unlinkSync(project);
  assert.ok(await f.input('/skill:alpha')); assert.equal(await f.input('/skill:beta'), undefined);
  for (const content of ['{', JSON.stringify({ schemaVersion: 9, skills: [] }), ' '.repeat(1024 * 1024 + 1)]) {
    fs.writeFileSync(project, content);
    assert.ok(await f.input('/skill:alpha')); assert.equal(await f.input('/skill:beta'), undefined);
  }
  scopeManifest(f, 'project', [b.entry]);
  assert.ok(await f.input('/skill:beta'));
});

test('conflicting installation descriptors exclude only that path, independent of scope order', async (t) => {
  for (const reverse of [false, true]) {
    const f = fixture(t), a = f.skill('alpha'), b = f.skill('beta');
    const global = scopeManifest(f, 'global', [a.entry]);
    const project = scopeManifest(f, 'project', [{ ...a.entry, sourceIdentity: '0:0' }, b.entry, a.entry]);
    for (const file of reverse ? [project, global] : [global, project]) f.load({ manifestPath: file }, null);
    assert.equal(await f.input('/skill:alpha'), undefined);
    assert.ok(await f.input('/skill:beta'));
    assert.equal(await f.read(a), undefined);
    fs.unlinkSync(project);
    assert.ok(await f.input('/skill:alpha'));
  }
});

test('a newly contributed conflict between call and result invalidates that call', async (t) => {
  const f = fixture(t), a = f.skill('alpha'); f.load(inline([a.entry]), null);
  const c = f.call(a); await f.emit('tool_call', c);
  f.load(inline([{ ...a.entry, sourceIdentity: '0:0' }]), null);
  assert.equal(await f.emit('tool_result', f.result(c)), undefined);
});

test('invalid owner file does not suppress a later valid scope', async (t) => {
  const f = fixture(t), a = f.skill('alpha');
  f.load({ manifestPath: path.join(f.root, 'missing.json') }, null);
  assert.equal(f.load(inline([a.entry]), null).size, 0);
  assert.ok(await f.input('/skill:alpha'));
});

test('reload releases all old contributions, even if removed-scope files remain on disk', async (t) => {
  const f = fixture(t), a = f.skill('alpha'), b = f.skill('beta'), c = f.skill('gamma');
  const global = scopeManifest(f, 'global', [a.entry]);
  const project = scopeManifest(f, 'project', [b.entry]);
  f.load({ manifestPath: global }, null); f.load({ manifestPath: project }, null); f.load(inline([c.entry]), null);
  assert.ok(await f.input('/skill:beta')); assert.ok(await f.input('/skill:gamma'));
  await f.emit('session_shutdown'); f.teardownRuntime();
  assert.ok(f.load({ manifestPath: global }, null).size > 0);
  assert.ok(await f.input('/skill:alpha'));
  assert.equal(await f.input('/skill:beta'), undefined); assert.equal(await f.input('/skill:gamma'), undefined);
  assert.deepEqual(f.busErrors, []);
});

test('at most eight distinct file manifests contribute; duplicates do not consume file slots', async (t) => {
  const f = fixture(t), a = f.skill('alpha'), b = f.skill('beta');
  const global = scopeManifest(f, 'global', [a.entry]);
  f.load({ manifestPath: global }, null);
  for (let i = 0; i < 10; i++) f.load({ manifestPath: global }, null);
  for (let i = 1; i < 8; i++) f.load({ manifestPath: scopeManifest(f, `empty-${i}`, []) }, null);
  const overflow = scopeManifest(f, 'overflow', [b.entry]);
  f.load({ manifestPath: overflow }, null);
  const opened = traceOpens(t);
  assert.ok(await f.input('/skill:alpha'));
  assert.equal(opened.filter((p) => p.endsWith('.json')).length, 8);
  assert.equal(opened.includes(overflow), false);
  assert.equal(await f.input('/skill:beta'), undefined);
});

const dummyEntry = (i) => ({ nativeName: `unused-${i}`, installationSkillPath: `/unused/${i}/SKILL.md`,
  canonicalSourceRoot: `/unused/source-${i}`, sourceIdentity: '1:1' });

for (const slots of [4095, 4096]) for (const kind of ['invalid', 'conflicted']) {
  test(`capacity ${slots}: ${kind} exact winner veto survives bounded merge`, async (t) => {
    const f = fixture(t), a = f.skill('alpha'), winner = nativeAlias(f, a);
    const full = [a.entry, ...Array.from({ length: slots - 1 }, (_, i) => dummyEntry(i))];
    const invalid = { ...winner.entry, sourceIdentity: '0:0' };
    const first = scopeManifest(f, 'full', full);
    const second = scopeManifest(f, 'exact', kind === 'conflicted' ? [winner.entry, invalid] : [invalid]);
    assert.ok(fs.statSync(first).size < 1024 * 1024);
    f.load({ manifestPath: first }, null); f.load({ manifestPath: second }, null);
    const opened = traceOpens(t);
    assert.equal(Boolean(await f.input('/skill:alpha')), false);
    await f.emit('agent_end');
    assert.equal(Boolean(await f.read(a, { input: { path: winner.file } })), false);
    assert.equal(opened.includes(a.adap), false);
  });
}

for (const mode of ['full-without-overflow', 'overflow-with-exact', 'overflow-without-exact']) {
  test(`capacity fallback control: ${mode}`, async (t) => {
    const f = fixture(t), a = f.skill('alpha'), winner = nativeAlias(f, a);
    const admitted = mode === 'overflow-with-exact' ? winner.entry : a.entry;
    f.load({ manifestPath: scopeManifest(f, 'full',
      [admitted, ...Array.from({ length: 4095 }, (_, i) => dummyEntry(i))]) }, null);
    if (mode !== 'full-without-overflow') {
      f.load({ manifestPath: scopeManifest(f, 'overflow', [dummyEntry(4095)]) }, null);
    }
    const allowed = mode !== 'overflow-without-exact';
    assert.equal(Boolean(await f.input('/skill:alpha')), allowed);
    await f.emit('agent_end');
    assert.equal(Boolean(await f.read(a, { input: { path: winner.file } })), allowed);
  });
}

test('merged installation slots cap at 4096 while conflicts from later records are still checked', async (t) => {
  const f = fixture(t), a = f.skill('alpha'), b = f.skill('beta');
  const full = [a.entry, ...Array.from({ length: 4095 }, (_, i) => dummyEntry(i))];
  f.load({ manifestPath: scopeManifest(f, 'full', full) }, null);
  f.load({ manifestPath: scopeManifest(f, 'overflow', [b.entry]) }, null);
  assert.ok(await f.input('/skill:alpha')); assert.equal(await f.input('/skill:beta'), undefined);
  f.load(inline([{ ...a.entry, sourceIdentity: '0:0' }]), null);
  assert.equal(await f.input('/skill:alpha'), undefined);
});

test('inline manifest is the product entry and wins over explicit path and environment', async (t) => {
  const f = fixture(t), a = f.skill('alpha'), b = f.skill('beta');
  const wrongScope = scopeManifest(f, 'other', [b.entry]);
  f.load({ ...inline([a.entry]), manifestPath: wrongScope }, wrongScope);
  const opened = traceOpens(t);
  assert.ok(await f.input('/skill:alpha')); assert.equal(await f.input('/skill:beta'), undefined);
  assert.equal(opened.includes(wrongScope), false);
  await f.emit('agent_end'); assert.ok(await f.read(a));
});

test('per-skill inline contributions deduplicate descriptors and exclude conflicts without extra handlers', async (t) => {
  const f = fixture(t), a = f.skill('alpha'), b = f.skill('beta');
  assert.ok(f.load(inline([a.entry]), null).size > 0);
  assert.equal(f.load(inline([a.entry]), null).size, 0);
  assert.equal(f.load(inline([b.entry, a.entry]), null).size, 0);
  assert.equal((await f.input('/skill:alpha')).text.split(marker).length - 1, 1);
  assert.ok(await f.input('/skill:beta'));
  f.load(inline([{ ...a.entry, canonicalSourceRoot: b.source }]), null);
  assert.equal(await f.input('/skill:alpha'), undefined);
  assert.ok(await f.input('/skill:beta'));
});

test('inline descriptors are initialization snapshots; ADAP still rereads and reload removes static scope', async (t) => {
  const f = fixture(t), a = f.skill('alpha');
  const config = inline([structuredClone(a.entry)]);
  f.load(config, null);
  config.manifest.skills[0].sourceIdentity = '0:0';
  assert.ok(await f.read(a));
  fs.writeFileSync(a.adap, 'NEW-INLINE-ADAP');
  assert.match((await f.read(a)).content[1].text, /NEW-INLINE-ADAP/);
  f.teardownRuntime();
  f.load(config, null); assert.equal(await f.input('/skill:alpha'), undefined);
});

test('invalid explicit inline config never falls back to another scope or poisons a valid owner', async (t) => {
  const f = fixture(t), a = f.skill('alpha');
  for (const manifest of [null, {}, { schemaVersion: 2, skills: [a.entry] }]) {
    assert.equal(f.load({ manifest, manifestPath: f.manifestPath }, f.manifestPath).size, 0);
  }
  assert.ok(f.load(inline([a.entry]), null).size > 0);
  assert.ok(await f.input('/skill:alpha'));
});

test('static contribution count is bounded and identical static repeats do not consume slots', async (t) => {
  const f = fixture(t), a = f.skill('alpha'), b = f.skill('beta');
  f.load(inline([a.entry]), null);
  for (let i = 0; i < 10; i++) f.load(inline([a.entry]), null);
  for (let i = 0; i < 4094; i++) f.load(inline([dummyEntry(i)]), null);
  f.load(inline([b.entry]), null); // Slot 4096, duplicates have not consumed it.
  assert.ok(await f.input('/skill:beta'));
  f.load(inline([{ ...a.entry, sourceIdentity: '0:0' }]), null); // Ignored, count cap.
  assert.ok(await f.input('/skill:alpha'));
});

test('aggregate static JSON bytes are capped independently of file scopes', async (t) => {
  const f = fixture(t);
  const skills = Array.from({ length: 9 }, (_, i) => f.skill(`control-${i}`));
  for (let i = 0; i < 9; i++) {
    const padding = { ...dummyEntry(i), canonicalSourceRoot: '/' + 'r'.repeat(950000) };
    f.load(inline([skills[i].entry, padding]), null);
  }
  assert.ok(await f.input('/skill:control-0')); assert.ok(await f.input('/skill:control-7'));
  assert.equal(await f.input('/skill:control-8'), undefined); // >8 MiB aggregate.
  // Rejected static bytes do not disable a small independent file scope.
  f.load({ manifestPath: scopeManifest(f, 'last', [skills[8].entry]) }, null);
  assert.ok(await f.input('/skill:control-8'));
});

test('current native name rebinds a renamed skill at the same managed path and source identity', async (t) => {
  const f = fixture(t), a = f.skill('alpha');
  f.load(inline([structuredClone(a.entry)]), null);
  fs.writeFileSync(path.join(a.source, 'SKILL.md'), '---\nname: renamed\ndescription: fixture\n---\nRENAMED-BODY');
  a.command.name = 'skill:renamed'; // Mock native rediscovery, not inferred from prompt text.
  assert.equal(a.entry.nativeName, 'alpha');
  assert.equal(id(a.source), a.entry.sourceIdentity);
  assert.equal(await f.input('/skill:alpha'), undefined);
  const manual = await f.input('/skill:renamed');
  assert.ok(manual.text.includes('[PLEAT managed adaptation: renamed]'));
  await f.emit('agent_end');
  assert.ok((await f.read(a)).content[1].text.includes('[PLEAT managed adaptation: renamed]'));
});

test('same-name foreign native winner cannot inherit the renamed managed source adaptation', async (t) => {
  const f = fixture(t), a = f.skill('alpha'), b = f.skill('external', 'renamed');
  f.load(inline([a.entry]), null);
  f.host.commands = [b.command];
  const opened = traceOpens(t);
  assert.equal(await f.input('/skill:renamed'), undefined);
  assert.equal(await f.read(a), undefined);
  assert.equal(await f.read(b), undefined);
  assert.equal(opened.includes(a.adap), false); assert.equal(opened.includes(b.adap), false);
});

function moveSourceLibrary(f, s) {
  const oldRoot = path.dirname(s.source), newRoot = path.join(f.root, 'moved-library');
  fs.renameSync(oldRoot, newRoot);
  fs.symlinkSync(newRoot, oldRoot, 'dir');
  return { oldRoot, newRoot, actualRoot: path.join(newRoot, path.basename(s.source)) };
}

test('MoveLibrary preserves an inline source locator and uses the actual root for manual and canonical reads', async (t) => {
  const f = fixture(t), a = f.skill('alpha');
  const descriptor = structuredClone(a.entry), localIdentity = id(path.dirname(a.adap));
  f.load(inline([a.entry]), null);
  assert.ok(await f.read(a));
  const { actualRoot } = moveSourceLibrary(f, a);
  assert.equal(id(actualRoot), descriptor.sourceIdentity);
  assert.equal(id(path.join(actualRoot, 'local-data')), localIdentity);
  assert.deepEqual(a.entry, descriptor); // Neither descriptor replacement nor reload.
  const opened = traceOpens(t);
  const manual = await f.input('/skill:alpha');
  assert.match(manual.text, /ADAP-alpha/);
  assert.ok(manual.text.includes(`Skill root: ${JSON.stringify(actualRoot)}`));
  assert.ok(manual.text.includes(`Adaptation directory: ${JSON.stringify(path.join(actualRoot, 'local-data'))}`));
  assert.equal(opened.includes(a.adap), false);
  assert.ok(opened.includes(path.join(actualRoot, 'local-data', 'ADAPTATION.md')));
  for (const file of [path.join(actualRoot, 'SKILL.md'), a.entry.installationSkillPath, path.join(a.source, 'SKILL.md')]) {
    await f.emit('agent_end');
    assert.match((await f.read(a, { input: { path: file } })).content[1].text, /ADAP-alpha/);
  }
});

test('a migrated source locator retargeted to a different root identity cannot inject', async (t) => {
  const f = fixture(t), a = f.skill('alpha');
  f.load(inline([a.entry]), null);
  const { oldRoot, actualRoot } = moveSourceLibrary(f, a);
  const foreignParent = path.join(f.root, 'foreign-library'), foreignRoot = path.join(foreignParent, 'alpha');
  fs.mkdirSync(path.join(foreignRoot, 'local-data'), { recursive: true });
  fs.writeFileSync(path.join(foreignRoot, 'SKILL.md'), 'FOREIGN-SKILL');
  fs.writeFileSync(path.join(foreignRoot, 'local-data', 'ADAPTATION.md'), 'FOREIGN-ADAP');
  fs.unlinkSync(oldRoot); fs.symlinkSync(foreignParent, oldRoot, 'dir');
  assert.notEqual(id(foreignRoot), a.entry.sourceIdentity);
  const opened = traceOpens(t);
  assert.equal(await f.input('/skill:alpha'), undefined);
  for (const file of [a.entry.installationSkillPath, path.join(actualRoot, 'SKILL.md'), path.join(foreignRoot, 'SKILL.md')]) {
    assert.equal(await f.read(a, { input: { path: file } }), undefined);
  }
  assert.equal(opened.some((file) => file.endsWith('ADAPTATION.md')), false);
});

test('source locator chain changes invalidate an in-flight canonical read even with a stable installation chain', async (t) => {
  const f = fixture(t), a = f.skill('alpha');
  f.load(inline([a.entry]), null);
  const { oldRoot, newRoot, actualRoot } = moveSourceLibrary(f, a);
  fs.unlinkSync(a.installed); fs.symlinkSync(actualRoot, a.installed, 'dir');
  const call = f.call(a, { input: { path: path.join(actualRoot, 'SKILL.md') } });
  await f.emit('tool_call', call);
  const relay = path.join(f.root, 'source-relay'); fs.symlinkSync(newRoot, relay, 'dir');
  fs.unlinkSync(oldRoot); fs.symlinkSync(relay, oldRoot, 'dir');
  assert.equal(fs.realpathSync(a.source), actualRoot);
  assert.equal(await f.emit('tool_result', f.result(call)), undefined);
  assert.match((await f.read(a, { input: call.input })).content[1].text, /ADAP-alpha/);
});

test('source locator replacement during ADAP opening discards the injection and preserves subsequent reads', async (t) => {
  const f = fixture(t), a = f.skill('alpha');
  f.load(inline([a.entry]), null);
  const { oldRoot, newRoot, actualRoot } = moveSourceLibrary(f, a);
  fs.unlinkSync(a.installed); fs.symlinkSync(actualRoot, a.installed, 'dir');
  const relay = path.join(f.root, 'source-relay'); fs.symlinkSync(newRoot, relay, 'dir');
  const original = fs.openSync;
  let changed = false;
  fs.openSync = function (file, ...args) {
    const fd = original.call(this, file, ...args);
    if (!changed && String(file) === path.join(actualRoot, 'local-data', 'ADAPTATION.md')) {
      changed = true;
      fs.unlinkSync(oldRoot); fs.symlinkSync(relay, oldRoot, 'dir');
    }
    return fd;
  };
  syncBuiltinESMExports();
  t.after(() => { fs.openSync = original; syncBuiltinESMExports(); });
  assert.equal(await f.read(a), undefined);
  assert.equal(changed, true);
  assert.match((await f.read(a)).content[1].text, /ADAP-alpha/);
});

test('installation alias metadata binds frontmatter alpha by native path for manual and automatic reads', async (t) => {
  const f = fixture(t), a = f.skill('alpha--12345678', 'alpha');
  a.entry.nativeName = 'alpha--12345678';
  f.load(inline([a.entry]), null);
  assert.match((await f.input('/skill:alpha')).text, /managed adaptation: alpha\]/);
  assert.equal(await f.input('/skill:alpha--12345678'), undefined);
  await f.emit('agent_end');
  assert.match((await f.read(a)).content[1].text, /ADAP-alpha--12345678/);
});

test('missing or invalid reader configuration has no filesystem fallback and cannot claim the bus', async (t) => {
  for (const config of [{}, { readerExecutable: 'relative' }, { readAdaptation: null }]) {
    const f = fixture(t), a = f.skill('alpha');
    assert.equal(f.load({ ...inline([a.entry]), readAdaptation: undefined, ...config }, null).size, 0);
    assert.equal(await f.input('/skill:alpha'), undefined);
    assert.equal(await f.read(a), undefined);
    assert.equal(f.load(inline([a.entry]), null).get('tool_result').length, 1);
    assert.ok(await f.read(a));
  }
});

test('reader responses require exact schema, identities, digest and bounded valid text', async (t) => {
  const invalid = [
    (r) => ({ ...r, schemaVersion: 2 }), (r) => ({ ...r, sourceRoot: '/foreign' }),
    (r) => ({ ...r, sourceIdentity: '0:0' }), (r) => ({ ...r, localDataIdentity: '0:0' }),
    (r) => ({ ...r, digest: '0'.repeat(64) }), (r) => ({ ...r, digest: r.digest.toUpperCase() }),
    (r) => readerResponse(r, 'x'.repeat(65537)), (r) => readerResponse(r, '界'.repeat(22000)),
    (r) => readerResponse(r, '\ud800'), (r) => readerResponse(r, 'a\0b'),
    () => undefined, (r) => Promise.resolve(r), () => { throw new Error('mock reader rejected'); },
  ];
  for (const change of invalid) {
    const f = fixture(t), a = f.skill('alpha');
    f.load({ ...inline([a.entry]), readAdaptation: (request) => change(readerResponse(request, 'OK')) }, null);
    assert.equal(await f.input('/skill:alpha'), undefined);
    const call = f.call(a), result = f.result(call), before = structuredClone(result);
    await f.emit('tool_call', call);
    assert.equal(await f.emit('tool_result', result), undefined);
    assert.deepEqual(result, before);
  }
});

test('executable reader uses bounded no-shell stdin/stdout protocol with a minimal environment', async (t) => {
  const f = fixture(t), a = f.skill('alpha'), executable = path.join(f.root, 'trusted reader');
  const original = childProcess.execFileSync;
  let calls = 0;
  childProcess.execFileSync = (file, args, options) => {
    calls++;
    assert.equal(file, executable);
    assert.deepEqual(args, ['--pleat-native-read-adaptation-v1']);
    assert.equal(options.shell, false);
    assert.deepEqual(options.stdio, ['pipe', 'pipe', 'ignore']);
    assert.deepEqual(options.env, { PATH: '/usr/bin:/bin', LANG: 'C.UTF-8' });
    assert.equal(options.maxBuffer, 262144); assert.equal(options.timeout, 3000);
    assert.ok(Buffer.byteLength(options.input) <= 16384);
    const request = JSON.parse(options.input);
    assert.deepEqual(request, { schemaVersion: 1, sourceRoot: a.source,
      sourceIdentity: a.entry.sourceIdentity, localDataIdentity: id(path.dirname(a.adap)) });
    return Buffer.from(JSON.stringify(readerResponse(request, 'READER-ADAP')));
  };
  syncBuiltinESMExports();
  t.after(() => { childProcess.execFileSync = original; syncBuiltinESMExports(); });
  f.load({ ...inline([a.entry]), readAdaptation: undefined, readerExecutable: executable }, null);
  const opened = traceOpens(t);
  assert.match((await f.input('/skill:alpha')).text, /READER-ADAP/);
  await f.emit('agent_end');
  assert.match((await f.read(a)).content[1].text, /READER-ADAP/);
  assert.equal(calls, 2);
  assert.equal(opened.includes(a.adap), false); // Node hook did not open ADAP.
});

test('executable reader errors, malformed bytes and oversized stdout preserve results without fallback', async (t) => {
  const original = childProcess.execFileSync;
  t.after(() => { childProcess.execFileSync = original; syncBuiltinESMExports(); });
  for (const produce of [() => { throw new Error('timeout/nonzero exit'); },
    () => Buffer.from('{'), () => Buffer.from([0xff]), () => Buffer.alloc(262145)]) {
    const f = fixture(t), a = f.skill('alpha');
    childProcess.execFileSync = produce; syncBuiltinESMExports();
    f.load({ ...inline([a.entry]), readAdaptation: undefined, readerExecutable: '/mock/reader' }, null);
    const opened = traceOpens(t); opened.length = 0;
    assert.equal(await f.input('/skill:alpha'), undefined);
    assert.equal(await f.read(a), undefined);
    assert.equal(opened.includes(a.adap), false);
  }
});

for (const mode of ['manual', 'automatic']) {
  test(`${mode} foreign-parent race rejects the trusted reader failure even after the original directory is restored`, async (t) => {
    const f = fixture(t), a = f.skill('alpha');
    const local = path.dirname(a.adap), held = local + '-held', foreign = path.join(f.root, 'foreign');
    fs.mkdirSync(foreign); fs.writeFileSync(path.join(foreign, 'ADAPTATION.md'), 'FOREIGN-ADAPTATION');
    let attempted = false;
    f.load({ ...inline([a.entry]), readAdaptation(request) {
      assert.equal(request.localDataIdentity, id(local));
      fs.renameSync(local, held); fs.symlinkSync(foreign, local, 'dir');
      try {
        attempted = true;
        // Model the native reader's anchored NOFOLLOW/identity rejection, not its implementation.
        assert.ok(fs.lstatSync(local).isSymbolicLink());
        throw new Error('mock FD reader rejected replaced parent');
      } finally { fs.unlinkSync(local); fs.renameSync(held, local); }
    } }, null);
    const opened = traceOpens(t);
    if (mode === 'manual') assert.equal(await f.input('/skill:alpha'), undefined);
    else {
      const call = f.call(a), result = f.result(call), before = structuredClone(result);
      await f.emit('tool_call', call);
      assert.equal(await f.emit('tool_result', result), undefined);
      assert.deepEqual(result, before);
    }
    assert.equal(attempted, true);
    assert.equal(opened.some((file) => file.endsWith('ADAPTATION.md')), false);
    assert.equal(fs.readFileSync(a.adap, 'utf8'), 'ADAP-alpha');
  });
}
