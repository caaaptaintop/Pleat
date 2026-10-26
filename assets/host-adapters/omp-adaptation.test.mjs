// Built-ins only; no OMP/Pi SDK import or target Agent execution.
import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import os from 'node:os';
import { EventEmitter } from 'node:events';
import { createHash } from 'node:crypto';
import childProcess from 'node:child_process';
import { syncBuiltinESMExports } from 'node:module';
import ompAdaptation from './omp-adaptation.js';

const id = (p) => { const s = fs.statSync(p, { bigint: true }); return `${s.dev}:${s.ino}`; };
const response = (request, text) => ({ ...request, text, digest: createHash('sha256').update(text).digest('hex') });
function fixture(t) {
  const root = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'pleat-omp-mock-')));
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  const emitter = new EventEmitter(), extensions = [], readers = [], texts = new Map(), loaded = [], entries = [];
  let idle = true, queued = false, sequence = 0, runtimeReady = false, commandEnabled = true, moduleSkills;
  let tools = [{ name: 'read', sourceInfo: { source: 'builtin', path: '<builtin:read>', scope: 'temporary', origin: 'top-level' } }];
  const extraCommands = [];
  const ctx = { cwd: root, isIdle: () => idle, hasPendingMessages: () => queued };
  // Mirrors EventBus: listener starts synchronously; on returns an unsubscribe function.
  const bus = { emit: (channel, value) => emitter.emit(channel, value), on(channel, handler) {
    const safe = async (data) => { await handler(data); };
    emitter.on(channel, safe);
    return () => emitter.off(channel, safe);
  } };
  function skill(label, name = label) {
    const source = path.join(root, 'library', label), installed = path.join(root, 'installed', label);
    fs.mkdirSync(path.join(source, 'local-data'), { recursive: true });
    fs.mkdirSync(path.dirname(installed), { recursive: true });
    fs.writeFileSync(path.join(source, 'SKILL.md'), `---\nname: ${name.split('/').at(-1)}\ndescription: fixture\n---\nBODY-${label}`);
    fs.symlinkSync(source, installed, 'dir');
    const entry = { nativeName: name, installationSkillPath: path.join(installed, 'SKILL.md'),
      canonicalSourceRoot: source, sourceIdentity: id(source) };
    const native = { name, description: 'fixture', filePath: entry.installationSkillPath, baseDir: installed, source: 'native:project',
      _source: { provider: 'native', providerName: 'OMP', path: installed, level: 'project' } };
    texts.set(source, `ADAP-${label}`);
    entries.push(entry); loaded.push(native);
    return { entry, native, source, installed };
  }
  const reader = (request) => {
    readers.push(request);
    assert.equal(id(request.sourceRoot), request.sourceIdentity);
    assert.equal(id(path.join(request.sourceRoot, 'local-data')), request.localDataIdentity);
    // Trusted mock seam: bytes deliberately held in memory, never a weak pathname reader.
    if (!texts.has(request.sourceRoot)) throw new Error('mock reader rejected');
    return response(request, texts.get(request.sourceRoot));
  };
  function load(config = {}, selected = entries) {
    const handlers = new Map();
    const api = { events: bus, get pi() { return moduleSkills; }, on(type, handler) {
      const list = handlers.get(type) ?? []; list.push(handler); handlers.set(type, list);
    }, getCommands() {
      assert.ok(runtimeReady, 'factory cannot query uninitialized runtime');
      return [...extraCommands, ...(commandEnabled ? loaded.map((s) => ({ name: `skill:${s.name}`, source: 'skill', path: s.filePath })) : [])];
    }, getAllTools() { assert.ok(runtimeReady); return tools; } };
    const dispose = ompAdaptation(api, { manifest: { schemaVersion: 1, skills: selected }, readAdaptation: reader,
      getSkills: () => { assert.ok(runtimeReady); return loaded; }, ...config });
    extensions.push(handlers);
    return { handlers, dispose };
  }
  async function emit(type, event = {}) {
    runtimeReady = true;
    let current = { type, ...event }, output;
    for (const handlers of extensions) for (const handler of handlers.get(type) ?? []) {
      const result = await handler(current, ctx);
      if (!result) continue;
      if (type === 'input') current = { ...current, ...result };
      // OMP tool_result chains only content/details/isError, additionalContext is separate.
      if (type === 'tool_result') for (const key of ['content', 'details', 'isError']) {
        if (result[key] !== undefined) current = { ...current, [key]: result[key] };
      }
      output = { ...output, ...result };
    }
    return output;
  }
  const input = (text, extra = {}) => emit('input', { text, source: 'interactive', ...extra });
  const call = (s, raw = `skill://${s.native.name}`, extra = {}) => ({ toolName: 'read', toolCallId: `read-${++sequence}`,
    input: { path: raw }, ...extra });
  const result = (c, s, extra = {}) => ({ ...c, content: [{ type: 'text', text: `BODY-${s.native.name}` }], isError: false,
    details: { resolvedPath: c.input.path.startsWith('skill://') ? s.native.filePath : path.resolve(root, c.input.path), fileSize: 100, meta: { source: c.input.path.startsWith('skill://')
      ? { type: 'internal', value: c.input.path } : { type: 'path', value: path.resolve(root, c.input.path) } } }, ...extra });
  async function read(s, raw, extra) { const c = call(s, raw); await emit('tool_call', c); return emit('tool_result', result(c, s, extra)); }
  return { root, emitter, extensions, loaded, entries, readers, texts, extraCommands, skill, load, emit, input, call, result, read,
    setIdle: (value) => { idle = value; }, setQueued: (value) => { queued = value; },
    setTools: (value) => { tools = value; }, setCommands: (value) => { commandEnabled = value; },
    setModuleSkills: (value) => { moduleSkills = { getActiveSkills: () => { assert.ok(runtimeReady); return value; } }; } };
}

test('file extension uses injected native loaded skills with commands disabled, without SDK closure or rescan', async (t) => {
  const f = fixture(t), s = f.skill('a'); f.setCommands(false); f.setModuleSkills(f.loaded);
  f.load({ getSkills: undefined });
  assert.equal(await f.input('/skill:a'), undefined);
  assert.match((await f.read(s)).additionalContext, /ADAP-a/);
});

test('injected native snapshot never substitutes its foreign winner for actual read provenance', async (t) => {
  const f = fixture(t), s = f.skill('a'), other = f.skill('other', 'a');
  f.setCommands(false); f.setModuleSkills([other.native]); f.load({ getSkills: undefined });
  const c = f.call(other); await f.emit('tool_call', c);
  assert.equal(await f.emit('tool_result', f.result(c, s)), undefined);
  assert.equal(f.readers.length, 0);
});

for (const rawName of ['alpha', 'native/alpha', 'custom:src/alpha~2']) {
  test(`native URI ${rawName}: provenance and original result preserved`, async (t) => {
    const f = fixture(t), s = f.skill('a', rawName); f.load();
    const c = f.call(s); const beforeCall = structuredClone(c);
    assert.equal(await f.emit('tool_call', c), undefined); assert.deepEqual(c, beforeCall);
    const r = f.result(c, s), before = structuredClone(r);
    const out = await f.emit('tool_result', r);
    assert.deepEqual(Object.keys(out), ['additionalContext']);
    assert.match(out.additionalContext, /ADAP-a/); assert.match(out.additionalContext, new RegExp(rawName.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')));
    assert.deepEqual(r, before);
  });
}

test('manual TUI/RPC whitespace/args/images/mid-token use native syntax and only FD adaptation', async (t) => {
  const f = fixture(t), s = f.skill('a', 'scope/alpha~2'); f.load();
  for (const [text, source] of [['  /skill:scope/alpha~2\t  arg one\n two  ', 'interactive'],
    ['do this /skill:scope/alpha~2 then that', 'rpc'], ['/skill:scope/alpha~2', 'rpc']]) {
    const images = [{ type: 'image', data: 'fixture', mimeType: 'image/png' }];
    const out = await f.input(text, { source, images });
    assert.ok(out.text.startsWith(text)); assert.match(out.text, /ADAP-a/);
    assert.equal(out.images, images); assert.equal(out.action, undefined); assert.equal(out.handled, undefined);
    assert.doesNotMatch(out.text, /BODY-a/); // Body is native buildSkillPromptMessage's job.
  }
  assert.equal(f.readers.length, 3);
});

test('native input local-execution gates and extension-origin prompts never inject', async (t) => {
  const f = fixture(t); f.skill('alpha'); f.load();
  for (const text of ['! echo /skill:alpha', '$ echo /skill:alpha', '$$\t/skill:alpha', '/other /skill:alpha',
    '<skill name="alpha">body</skill>', 'skill://alpha', '/skill:unknown']) assert.equal(await f.input(text), undefined);
  assert.equal(await f.input('/skill:alpha', { source: 'extension' }), undefined);
  assert.equal(f.readers.length, 0);
  assert.match((await f.input('${value} /skill:alpha')).text, /ADAP-alpha/); // Native ${...} exception.
});

test('foreign same-name winner cannot borrow a manifest source for manual or URI/local read', async (t) => {
  const f = fixture(t), managed = f.skill('managed', 'alpha'), foreign = f.skill('foreign', 'alpha');
  f.loaded.splice(0, 1); f.load({}, [managed.entry]);
  assert.equal(await f.input('/skill:alpha'), undefined);
  assert.equal(await f.read(foreign), undefined); assert.equal(await f.read(managed, managed.entry.installationSkillPath), undefined);
  assert.equal(f.readers.length, 0);
});

test('bare and namespaced different sources remain distinct; same source rebind dedups by identity', async (t) => {
  const f = fixture(t), a = f.skill('a', 'alpha'), b = f.skill('b', 'native/alpha'); f.load();
  assert.match((await f.read(a)).additionalContext, /ADAP-a/);
  assert.match((await f.read(b)).additionalContext, /ADAP-b/);
  assert.equal(await f.read(a), undefined);
  a.native.name = 'renamed/alpha';
  assert.equal(await f.read(a), undefined);
  f.texts.set(a.source, 'UPDATED');
  assert.match((await f.read(a)).additionalContext, /UPDATED/);
});

test('auto commands-disabled reads require actual session.skills callback; missing metadata fails closed', async (t) => {
  const f = fixture(t), s = f.skill('a'); f.setCommands(false); f.load();
  assert.match((await f.read(s)).additionalContext, /ADAP-a/);
  assert.equal(await f.input('/skill:a'), undefined);
  const g = fixture(t), u = g.skill('b'); g.setCommands(false); g.load({ getSkills: undefined });
  assert.equal(await g.read(u), undefined); assert.equal(g.readers.length, 0);
});

test('command-enabled actual getCommands.path is supported without a session callback', async (t) => {
  const f = fixture(t), s = f.skill('a'); f.load({ getSkills: undefined });
  assert.match((await f.read(s)).additionalContext, /ADAP-a/);
});

test('namespace URI wins over bare root; relative files/traversal/selectors/MCP misses do not trigger', async (t) => {
  const f = fixture(t), a = f.skill('a', 'native'), b = f.skill('b', 'native/alpha'); f.load();
  assert.match((await f.read(b)).additionalContext, /ADAP-b/);
  for (const raw of ['skill://native/alpha/file.txt', 'skill://native/../SKILL.md', 'skill://unknown',
    'skill://native/local-data/ADAPTATION.md', 'skill://native:raw', 'skill://native/%zz']) assert.equal(await f.read(a, raw), undefined);
  assert.equal(f.readers.length, 1);
});

test('local installed/canonical SKILL paths and explicit URI SKILL.md are accepted', async (t) => {
  const f = fixture(t), s = f.skill('a'); f.load();
  for (const raw of [s.entry.installationSkillPath, path.join(s.source, 'SKILL.md'), 'skill://a/SKILL.md']) {
    await f.emit('agent_end', { isTerminal: true });
    assert.match((await f.read(s, raw)).additionalContext, /ADAP-a/);
  }
});

test('foreign/missing native metadata, errors, images and injected context alone are never read evidence', async (t) => {
  const f = fixture(t), s = f.skill('a'), other = f.skill('b'); f.load();
  const bad = [
    { isError: true }, { content: [] }, { content: [{ type: 'image', data: 'x' }] },
    { details: undefined },
    { details: { resolvedPath: s.native.filePath, meta: { source: { type: 'internal', value: 'skill://a' } } } }, { details: { resolvedPath: s.native.filePath } },
    { details: { resolvedPath: other.native.filePath, meta: { source: { type: 'internal', value: 'skill://a' } } } },
    { details: { resolvedPath: s.native.filePath, meta: { source: { type: 'internal', value: 'skill://b' } } } },
    { details: { resolvedPath: s.native.filePath, meta: { source: { type: 'internal', value: 'skill://a' }, truncation: { partialLine: true } } } },
    { content: [], additionalContext: 'BODY-a ADAP-a' },
  ];
  for (const extra of bad) assert.equal(await f.read(s, undefined, extra), undefined);
  assert.equal(f.readers.length, 0);
});

test('native unbounded skill URI has full display rows instead of fileSize; notices and partial rows fail closed', async (t) => {
  const f = fixture(t), s = f.skill('a'); f.load();
  const details = { resolvedPath: s.native.filePath, totalLines: 2,
    displayContent: { text: 'native body\nsecond line', startLine: 1, lineNumbers: [1, 2] },
    meta: { source: { type: 'internal', value: 'skill://a' } } };
  for (const rejected of [
    { ...details, displayContent: undefined },
    { ...details, totalLines: 0 },
    { ...details, displayContent: { ...details.displayContent, text: '' } },
    { ...details, displayContent: { ...details.displayContent, startLine: 2 } },
    { ...details, displayContent: { ...details.displayContent, lineNumbers: [1, null] } },
    { ...details, displayContent: { ...details.displayContent, lineNumbers: [1] } },
    { ...details, truncation: { outputBytes: 10 } },
    { ...details, meta: { ...details.meta, truncation: { partialLine: true } } },
  ]) assert.equal(await f.read(s, undefined, { details: rejected }), undefined);
  assert.equal(f.readers.length, 0);
  assert.match((await f.read(s, undefined, { details })).additionalContext, /ADAP-a/);
});

test('intervening winner/input/identity changes invalidate observations', async (t) => {
  const f = fixture(t), s = f.skill('a'), other = f.skill('b', 'a'); f.loaded.pop(); f.load();
  let c = f.call(s); await f.emit('tool_call', c); f.loaded[0] = other.native;
  assert.equal(await f.emit('tool_result', f.result(c, s)), undefined); f.loaded[0] = s.native;
  c = f.call(s); await f.emit('tool_call', c);
  assert.equal(await f.emit('tool_result', { ...f.result(c, s), input: { path: 'skill://b' } }), undefined);
  c = f.call(s); await f.emit('tool_call', c);
  fs.unlinkSync(s.installed); fs.symlinkSync(other.source, s.installed, 'dir');
  assert.equal(await f.emit('tool_result', f.result(c, s)), undefined);
  assert.equal(f.readers.length, 0);
});

test('builtin provenance and extension command shadow are checked', async (t) => {
  const f = fixture(t), s = f.skill('a'); f.load();
  f.extraCommands.push({ name: 'skill:a', source: 'extension' });
  assert.equal(await f.input('/skill:a'), undefined);
  f.setTools([{ name: 'read', sourceInfo: { source: 'extension', path: '<builtin:read>' } }]);
  assert.equal(await f.read(s), undefined); assert.equal(f.readers.length, 0);
});

test('queued input and nonterminal agent_end retain in-flight association and seen state', async (t) => {
  const f = fixture(t), s = f.skill('a'); f.load(); f.setIdle(false);
  const c = f.call(s); await f.emit('tool_call', c);
  await f.input('next question'); await f.emit('agent_end', { isTerminal: false });
  assert.match((await f.emit('tool_result', f.result(c, s))).additionalContext, /ADAP-a/);
  await f.input('/skill:a'); // queued adaptation is not marked as delivered by input.
  await f.emit('agent_end', { isTerminal: false });
  assert.equal(await f.read(s), undefined);
  await f.emit('agent_end', { isTerminal: true });
  assert.match((await f.read(s)).additionalContext, /ADAP-a/);
});

test('pending messages are queued even when ctx.isIdle is true', async (t) => {
  const f = fixture(t), s = f.skill('a'); f.load();
  const c = f.call(s); await f.emit('tool_call', c); f.setQueued(true);
  await f.input('queued'); assert.match((await f.emit('tool_result', f.result(c, s))).additionalContext, /ADAP-a/);
});

test('duplicate live tool IDs are poisoned and unmatched result cannot inject', async (t) => {
  const f = fixture(t), s = f.skill('a'); f.load();
  const c = f.call(s); await f.emit('tool_call', c); await f.emit('tool_call', c);
  assert.equal(await f.emit('tool_result', f.result(c, s)), undefined);
  assert.equal(await f.emit('tool_result', f.result(f.call(s), s)), undefined);
});

test('global/project factories share one owner, shutdown releases bus, reload removes old scope', async (t) => {
  const f = fixture(t), a = f.skill('a'), b = f.skill('b');
  const owner = f.load({}, [a.entry]), sibling = f.load({}, [b.entry]);
  assert.equal(owner.handlers.get('tool_result').length, 1); assert.equal(sibling.handlers.size, 0);
  assert.equal(f.emitter.listenerCount('pleat:omp-adaptation:v1:claim'), 1);
  assert.match((await f.read(a)).additionalContext, /ADAP-a/); assert.match((await f.read(b)).additionalContext, /ADAP-b/);
  await f.emit('session_shutdown'); assert.equal(f.emitter.listenerCount('pleat:omp-adaptation:v1:claim'), 0);
  f.extensions.length = 0; f.load({}, [b.entry]);
  assert.equal(await f.read(a), undefined); assert.match((await f.read(b)).additionalContext, /ADAP-b/);
});

test('explicit disposer releases a bus before loader replacement; isolated buses have separate owners', async (t) => {
  const f = fixture(t), a = f.skill('a'); const owner = f.load(); owner.dispose(); owner.dispose();
  assert.equal(f.emitter.listenerCount('pleat:omp-adaptation:v1:claim'), 0);
  f.load(); assert.match((await f.read(a)).additionalContext, /ADAP-a/);
  const g = fixture(t), b = g.skill('b'); g.load(); assert.match((await g.read(b)).additionalContext, /ADAP-b/);
});

for (const event of ['session_start', 'session_tree', 'session_switch', 'session_branch']) {
  test(`${event} clears lifecycle state`, async (t) => {
    const f = fixture(t), s = f.skill('a'); f.load(); await f.read(s);
    await f.emit(event); assert.match((await f.read(s)).additionalContext, /ADAP-a/);
  });
}

test('missing reader/manifest and invalid reader identity/digest fail closed without claims', async (t) => {
  const f = fixture(t), s = f.skill('a');
  const absent = f.load({ readAdaptation: undefined }); assert.equal(absent.handlers.size, 0);
  const missing = f.load({ manifest: undefined }); assert.equal(missing.handlers.size, 0);
  f.load({ readAdaptation: (r) => ({ ...response(r, 'bad'), sourceIdentity: '0:0' }) });
  assert.equal(await f.read(s), undefined);
});

test('production execFileSync protocol uses trusted mock seam and never reads ADAP by pathname', async (t) => {
  const f = fixture(t), s = f.skill('a');
  const original = childProcess.execFileSync; let count = 0;
  childProcess.execFileSync = (file, args, opts) => {
    count++; assert.equal(file, '/trusted/mock-fd-reader');
    assert.deepEqual(args, ['--pleat-native-read-adaptation-v1']); assert.equal(opts.shell, false);
    assert.deepEqual(opts.env, { PATH: '/usr/bin:/bin', LANG: 'C.UTF-8' });
    assert.equal(opts.timeout, 3000); assert.equal(opts.maxBuffer, 256 * 1024);
    const r = JSON.parse(opts.input); assert.equal(r.sourceIdentity, id(s.source));
    return Buffer.from(JSON.stringify(response(r, 'MOCK-FD-BYTES')));
  };
  syncBuiltinESMExports();
  t.after(() => { childProcess.execFileSync = original; syncBuiltinESMExports(); });
  const envDigest = () => createHash('sha256').update(JSON.stringify({ ...process.env })).digest('hex');
  const envBefore = envDigest();
  f.load({ readAdaptation: undefined, readerExecutable: '/trusted/mock-fd-reader' });
  assert.match((await f.read(s)).additionalContext, /MOCK-FD-BYTES/);
  assert.ok(envDigest() === envBefore, 'environment changed'); assert.equal(count, 1);
  assert.ok(!fs.existsSync(path.join(s.source, 'local-data', 'ADAPTATION.md')));
});


test('compat source locator after MoveLibrary retains devino and canonical native result path', async (t) => {
  const f = fixture(t), s = f.skill('a'); f.load();
  const moved = path.join(f.root, 'moved');
  fs.renameSync(s.source, moved); fs.symlinkSync(moved, s.source, 'dir');
  f.texts.delete(s.source); f.texts.set(moved, 'MOVED-ADAP');
  const c = f.call(s); await f.emit('tool_call', c);
  const r = f.result(c, s); r.details.resolvedPath = path.join(moved, 'SKILL.md');
  assert.match((await f.emit('tool_result', r)).additionalContext, /MOVED-ADAP/);
  assert.equal(f.readers.at(-1).sourceRoot, moved);
  assert.equal(f.readers.at(-1).sourceIdentity, s.entry.sourceIdentity);
});

test('conflicting scope descriptors exclude the path without replacing the valid owner', async (t) => {
  const f = fixture(t), s = f.skill('a'); f.load({}, [s.entry]);
  f.load({}, [{ ...s.entry, sourceIdentity: '0:0' }]);
  assert.equal(await f.read(s), undefined); assert.equal(f.readers.length, 0);
});

test('every observed read rechecks reader; changed versions inject, unchanged versions dedup', async (t) => {
  const f = fixture(t), s = f.skill('a'); f.load();
  assert.ok(await f.read(s)); assert.equal(await f.read(s), undefined);
  assert.equal(f.readers.length, 2);
  f.texts.delete(s.source); assert.equal(await f.read(s), undefined);
  f.texts.set(s.source, 'RESTORED-NEW'); assert.match((await f.read(s)).additionalContext, /RESTORED-NEW/);
  assert.equal(f.readers.length, 4);
});

test('production reader failures and malformed replies preserve input/result and never fall back', async (t) => {
  const f = fixture(t), s = f.skill('a');
  const original = childProcess.execFileSync;
  let mode = 'throw';
  childProcess.execFileSync = () => {
    if (mode === 'throw') throw new Error('fixture reader failed');
    return Buffer.from('not-json');
  };
  syncBuiltinESMExports();
  t.after(() => { childProcess.execFileSync = original; syncBuiltinESMExports(); });
  f.load({ readAdaptation: undefined, readerExecutable: '/trusted/mock-fd-reader' });
  for (const next of ['throw', 'malformed']) {
    mode = next;
    const c = f.call(s); const beforeCall = structuredClone(c); await f.emit('tool_call', c);
    const r = f.result(c, s), before = structuredClone(r);
    assert.equal(await f.emit('tool_result', r), undefined);
    assert.deepEqual(c, beforeCall); assert.deepEqual(r, before);
    assert.equal(await f.input('/skill:a'), undefined);
  }
  assert.equal(f.readers.length, 0);
});


test('rejected result consumes its observation before a completed ID is reused', async (t) => {
  const f = fixture(t), s = f.skill('a'); f.load();
  const c = f.call(s); await f.emit('tool_call', c);
  assert.equal(await f.emit('tool_result', f.result(c, s, { details: undefined })), undefined);
  await f.emit('tool_call', c);
  assert.match((await f.emit('tool_result', f.result(c, s))).additionalContext, /ADAP-a/);
});


for (const continuation of [{ willContinue: true }, { willContinue: true, awaitingAsyncWork: true }, { awaitingAsyncWork: true }]) {
  test(`actual extension end preserves continuation ${JSON.stringify(continuation)}`, async (t) => {
    const f = fixture(t), s = f.skill('a'); f.load();
    const c = f.call(s); await f.emit('tool_call', c);
    await f.emit('agent_end', continuation);
    assert.match((await f.emit('tool_result', f.result(c, s))).additionalContext, /ADAP-a/);
    await f.emit('agent_end', continuation); assert.equal(await f.read(s), undefined);
    await f.emit('agent_end'); assert.match((await f.read(s)).additionalContext, /ADAP-a/);
  });
}
