// Pure mock protocol tests. No OpenClaw/SDK/CLI/Rust/installer is imported or run.
import test from 'node:test';
import assert from 'node:assert/strict';
import childProcess from 'node:child_process';
import { syncBuiltinESMExports } from 'node:module';
import { createHash } from 'node:crypto';
import fs from 'node:fs';
import path from 'node:path';
import os from 'node:os';
import openClawAdaptation, { capability, configurationContract, pluginManifest, createAdaptationReader } from './openclaw-adaptation.js';

const request = { schemaVersion: 1, sourceRoot: '/fixture/managed-source',
  sourceIdentity: '1:2', localDataIdentity: '1:3' };
const response = (req, text = 'fixture adaptation') => ({ ...req, text,
  digest: createHash('sha256').update(text).digest('hex') });
const encoded = (value) => Buffer.from(JSON.stringify(value));

function mockExec(t, fn) {
  const original = childProcess.execFileSync;
  childProcess.execFileSync = fn;
  syncBuiltinESMExports();
  t.after(() => { childProcess.execFileSync = original; syncBuiltinESMExports(); });
}

test('reader uses exact no-shell FD protocol with bounded bytes and a minimal environment', (t) => {
  let calls = 0;
  mockExec(t, (file, args, options) => {
    calls++;
    assert.equal(file, '/fixture/trusted reader');
    assert.deepEqual(args, ['--pleat-native-read-adaptation-v1']);
    assert.equal(options.shell, false);
    assert.deepEqual(options.stdio, ['pipe', 'pipe', 'ignore']);
    assert.equal(options.maxBuffer, 262144);
    assert.equal(options.timeout, 3000);
    // Never dump/deepEqual process.env or inherited secrets.
    assert.equal(Object.keys(options.env).length === 2, true);
    assert.equal(options.env.PATH === '/usr/bin:/bin', true);
    assert.equal(options.env.LANG === 'C.UTF-8', true);
    assert.equal(Object.hasOwn(options.env, 'HOME'), false);
    assert.equal(Object.hasOwn(options.env, 'NODE_OPTIONS'), false);
    assert.equal(Buffer.byteLength(options.input) <= 16384, true);
    assert.deepEqual(JSON.parse(options.input), request);
    return encoded(response(request));
  });
  const reader = createAdaptationReader({ readerExecutable: '/fixture/trusted reader' });
  const value = reader.read({ ...request, unrelated: 'must not be passed to Rust' });
  assert.equal(value.text, 'fixture adaptation');
  assert.equal(Object.isFrozen(value), true);
  assert.equal(calls, 1);
  assert.equal(configurationContract.maxConcurrentReaders, 1);
});

test('invalid executable or native request does not spawn or fall back to a path read', (t) => {
  mockExec(t, () => assert.fail('invalid request spawned reader'));
  for (const readerExecutable of [undefined, '', 'relative', '/a/../b', '/a\0b', '/a\nb']) {
    assert.equal(createAdaptationReader({ readerExecutable }), undefined);
  }
  const reader = createAdaptationReader({ readerExecutable: '/fixture/reader' });
  for (const input of [undefined, null, {}, { ...request, schemaVersion: 2 },
    { ...request, sourceRoot: '/a/../b' }, { ...request, sourceIdentity: 'invalid' },
    { ...request, localDataIdentity: '1:2:3' }, { ...request, sourceRoot: '/' + 'a'.repeat(16384) }]) {
    assert.equal(reader.read(input), undefined);
  }
});

test('aggregate UTF-8 stdin limit rejects multibyte paths before spawn', (t) => {
  mockExec(t, () => assert.fail('oversized stdin spawned'));
  const reader = createAdaptationReader({ readerExecutable: '/fixture/reader' });
  assert.equal(reader.read({ ...request, sourceRoot: '/' + '界'.repeat(6000) }), undefined);
});

test('strict response binding rejects different root, dev:ino, schema and digest', (t) => {
  let transform;
  mockExec(t, () => encoded(transform(response(request))));
  const reader = createAdaptationReader({ readerExecutable: '/fixture/reader' });
  for (transform of [
    (r) => ({ ...r, schemaVersion: 2 }), (r) => ({ ...r, sourceRoot: '/foreign' }),
    (r) => ({ ...r, sourceIdentity: '9:9' }), (r) => ({ ...r, localDataIdentity: '9:9' }),
    (r) => ({ ...r, digest: '0'.repeat(64) }), (r) => ({ ...r, digest: r.digest.toUpperCase() }),
    () => null, () => ({}),
  ]) assert.equal(reader.read(request), undefined);
});

test('reader rejects invalid Unicode, NUL and over-limit text', (t) => {
  let body;
  mockExec(t, () => encoded(response(request, body)));
  const reader = createAdaptationReader({ readerExecutable: '/fixture/reader' });
  for (body of ['\ud800', 'a\0b', 'a'.repeat(65537), '界'.repeat(22000)]) {
    assert.equal(reader.read(request), undefined);
  }
  body = 'a'.repeat(65536);
  assert.equal(reader.read(request).text.length, 65536);
});

test('malformed/oversized stdout, timeout and process errors fail without raw errors', (t) => {
  let produce;
  mockExec(t, () => produce());
  const reader = createAdaptationReader({ readerExecutable: '/fixture/reader' });
  for (produce of [() => Buffer.from('{'), () => Buffer.from([0xff]),
    () => Buffer.alloc(262145), () => encoded(response(request)).toString(),
    () => { throw new Error('private raw stderr must not escape'); }]) {
    assert.equal(reader.read(request), undefined);
  }
  produce = () => encoded(response(request));
  assert.equal(reader.read(request).text, 'fixture adaptation');
});

test('sync reader concurrency is bounded at one; reentrancy and disposal cannot publish', (t) => {
  let reader;
  let disposeInside = false;
  mockExec(t, () => {
    assert.equal(reader.read(request), undefined);
    if (disposeInside) reader.dispose();
    return encoded(response(request));
  });
  reader = createAdaptationReader({ readerExecutable: '/fixture/reader' });
  assert.equal(reader.read(request).text, 'fixture adaptation');
  disposeInside = true;
  assert.equal(reader.read(request), undefined);
  assert.equal(reader.read(request), undefined);
  reader.dispose();
});

test('same name at another source cannot borrow protocol identity', (t) => {
  mockExec(t, () => encoded(response(request)));
  const reader = createAdaptationReader({ readerExecutable: '/fixture/reader' });
  assert.equal(reader.read({ ...request, sourceRoot: '/another/source', sourceIdentity: '7:8' }), undefined);
});

const id = (file) => { const s = fs.lstatSync(file, { bigint: true }); return `${s.dev}:${s.ino}`; };
const sha = (value) => createHash('sha256').update(value).digest('hex');
// Fixed dist: installModelPromptProjection updates user metadata (attempt:3190-3202);
// settled messages retain it through builtin:4093-4101/1604-1609 and hooks:108-115.
// Ordinary content can still be the transcript text; never invent a public prompt field.
const endMessages = (projection, tools = []) => [{ role: 'user', content: 'ordinary task', timestamp: 1,
  __openclaw: { modelPromptProjection: { version: 1, text: projection } } }, ...tools,
{ role: 'assistant', content: [{ type: 'text', text: 'fixture answer' }], stopReason: 'stop', timestamp: 2 }];

function fixture(t, options = {}) {
  const root = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'pleat-ocw-asset-')));
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  const workspace = path.join(root, 'workspace'); fs.mkdirSync(workspace);
  const records = [], hooks = new Map(), commands = new Map(options.commands ?? []), tools = new Map(options.tools ?? []);
  const ctx = { agentId: 'fixture-agent', sessionKey: 'agent:fixture-agent:probe', sessionId: 'physical-1',
    runId: 'run-1', workspaceDir: workspace, sandboxed: false };
  const manifest = { schemaVersion: 1, skills: [] };
  let calls = 0, loadCount = 0, disposedCallback;
  const wireRequests = [];
  const modelPrompts = new Map(), toolMessages = new Map();
  const turnKey = (context) => JSON.stringify([context.agentId, context.sessionKey, context.sessionId, context.runId]);
  const api = {
    on(name, fn) { const list = hooks.get(name) ?? []; list.push(fn); hooks.set(name, list); },
    registerCommand(command) {
      // Installed native registrar checks normalized invocation keys; existing owner wins.
      if (!commands.has(command.name.toLowerCase())) commands.set(command.name.toLowerCase(), command);
    },
    registerTool(factory, settings) {
      assert.equal(factory.contextVersion, 2);
      assert.equal(pluginManifest.contracts.tools.includes(settings.name), true);
      if (!tools.has(settings.name)) tools.set(settings.name, factory);
    },
    registerAgentToolResultMiddleware() { assert.fail('builtin read ownership is not yet proved'); },
    lifecycle: { onDispose(fn) { disposedCallback = fn; } },
  };
  function skill(label, adap = 'ADAP-' + label) {
    const source = path.join(root, 'library', label), installed = path.join(root, 'installed', label);
    fs.mkdirSync(source, { recursive: true }); fs.mkdirSync(path.dirname(installed), { recursive: true });
    const record = { source, installed, body: `---\nname: shared-name\ndescription: fixture\n---\nBODY-${label}`, adap };
    fs.writeFileSync(path.join(source, 'SKILL.md'), record.body);
    if (adap !== undefined && adap !== null) {
      fs.mkdirSync(path.join(source, 'local-data')); fs.writeFileSync(path.join(source, 'local-data', 'ADAPTATION.md'), adap);
    }
    fs.symlinkSync(source, installed, 'dir');
    record.entry = { installationId: label, agentId: ctx.agentId, workspaceDir: workspace,
      installationSkillPath: path.join(installed, 'SKILL.md'), canonicalSourceRoot: source, sourceIdentity: id(source), automaticAllowed: true };
    manifest.skills.push(record.entry); records.push(record); return record;
  }
  mockExec(t, (_file, args, settings) => {
    calls++; assert.deepEqual(args, ['--pleat-native-read-adaptation-v1']);
    const req = JSON.parse(settings.input); wireRequests.push(req);
    const record = records.find((r) => fs.realpathSync(r.source) === req.sourceRoot);
    assert.ok(record); assert.equal(id(req.sourceRoot), req.sourceIdentity);
    const skillPath = path.join(req.sourceRoot, 'SKILL.md'), s = fs.lstatSync(skillPath);
    assert.ok(s.isFile() && s.nlink === 1 && s.size <= 262144);
    const skillDigest = sha(record.body);
    if (req.expectedSkillDigest !== undefined && req.expectedSkillDigest !== skillDigest) throw new Error('mock FD expected digest mismatch');
    let text = '';
    if (req.localDataIdentity !== undefined) {
      const local = path.join(req.sourceRoot, 'local-data');
      assert.ok(fs.lstatSync(local).isDirectory()); assert.equal(id(local), req.localDataIdentity);
      assert.ok(fs.lstatSync(path.join(local, 'ADAPTATION.md')).isFile()); text = record.adap;
    } else assert.equal(req.includeSkillText, true);
    const result = { schemaVersion: 1, sourceRoot: req.sourceRoot, sourceIdentity: req.sourceIdentity,
      ...(req.localDataIdentity !== undefined ? { localDataIdentity: req.localDataIdentity } : {}), text, digest: sha(text),
      ...(Object.hasOwn(req, 'includeSkillText') || Object.hasOwn(req, 'expectedSkillDigest')
        ? { skillDigest, skillIdentity: id(skillPath) } : {}),
      ...(req.includeSkillText === true ? { skillText: record.body } : {}) };
    options.insideReader?.(req, result);
    return encoded(options.transformResponse ? options.transformResponse(result) : result);
  });
  const load = (config = {}) => openClawAdaptation(api, { readerExecutable: '/fixture/trusted reader',
    loadManifest(actual) { loadCount++; assert.equal(Object.hasOwn(actual, 'messages'), false);
      assert.equal(Object.hasOwn(actual, 'config'), false); return manifest; }, ...config });
  function emit(name, event = {}, context = ctx) {
    return (hooks.get(name) ?? []).map((fn) => fn(event, context)).filter(Boolean);
  }
  const select = (key, context = ctx) => commands.get('pleat-adaptation').handler({
    agentId: context.agentId, sessionKey: context.sessionKey, sessionId: context.sessionId,
    channel: 'fixture', isAuthorizedSender: true, args: key,
  });
  // Installed native consumer joins appendContext to the next model prompt. Never
  // substitute command handler text, unknown fields, prompt obedience or target imports.
  const build = (context = ctx) => {
    const values = emit('before_prompt_build', { prompt: 'ordinary task', messages: [] }, context);
    const prompt = ['ordinary task', ...values.map((v) => v.appendContext).filter(Boolean)].join('\n\n');
    modelPrompts.set(turnKey(context), prompt); return prompt;
  };
  const accept = (context = ctx, success = true, messages) => {
    emit('model_call_started', {}, context);
    emit('agent_end', { runId: context.runId, success,
      messages: messages ?? endMessages(modelPrompts.get(turnKey(context)) ?? 'ordinary task', toolMessages.get(turnKey(context))) }, context);
  };
  function update(record, body, adap) {
    if (body !== undefined) { record.body = body; fs.writeFileSync(path.join(record.source, 'SKILL.md'), body); }
    if (adap !== undefined) { record.adap = adap; fs.writeFileSync(path.join(record.source, 'local-data', 'ADAPTATION.md'), adap); }
  }
  const tool = (context = ctx, guard = () => {}) => {
    // Actual producer provides these fields, not runId or the tool arguments' claimed scope.
    const { agentId, sessionKey, sessionId, workspaceDir, sandboxed } = context;
    return tools.get('pleat_read_skill').create({ agentId, sessionKey, sessionId, workspaceDir, sandboxed,
      assertInvocationCurrent: guard });
  };
  const delivered = (result, record, call = 'call-1', context = ctx) => {
    const list = toolMessages.get(turnKey(context)) ?? [];
    list.push({ role: 'toolResult', toolName: 'pleat_read_skill', toolCallId: call,
      content: result.content, isError: result.isError, timestamp: 1 }); toolMessages.set(turnKey(context), list);
    emit('after_tool_call', { toolName: 'pleat_read_skill', toolCallId: call, runId: context.runId,
      params: { installationId: record.entry.installationId }, result,
      error: result.isError ? 'tool failure' : undefined },
    { agentId: context.agentId, sessionKey: context.sessionKey, sessionId: context.sessionId,
      toolName: 'pleat_read_skill', toolCallId: call, runId: context.runId });
  };
  return { root, workspace, ctx, manifest, records, hooks, commands, tools, api, skill, load, emit, select, build, accept, update,
    tool, delivered,
    wireRequests, calls: () => calls, loads: () => loadCount, nativeDispose: () => disposedCallback() };
}

test('explicit installation ID reaches the mock next model request through appendContext, not handler display', (t) => {
  const f = fixture(t), a = f.skill('installation-a'); const registered = f.load();
  const displayed = f.select(a.entry.installationId);
  assert.equal(displayed.text.includes(a.body), false); assert.equal(displayed.text.includes(a.adap), false);
  assert.equal(f.calls(), 0);
  const modelPrompt = f.build();
  assert.ok(modelPrompt.includes(a.body)); assert.ok(modelPrompt.includes(a.adap));
  assert.equal(f.wireRequests[0].includeSkillText, true);
  assert.equal(f.wireRequests[0].localDataIdentity, id(path.join(a.source, 'local-data')));
  assert.equal(registered.manual, true); assert.equal(registered.automatic, true); assert.equal(registered.stockRead, false);
  assert.equal(registered.nativeAcceptance, 'NOT_RUN_NOT_PASSED');
  assert.equal(pluginManifest.id, 'pleat-openclaw-adaptation');
});

test('managed tool schema exposes scoped installation IDs; real execute-shaped result feeds the mock next model request', (t) => {
  const f = fixture(t), a = f.skill('tool-a'), b = f.skill('foreign-agent'); b.entry.agentId = 'other-agent';
  f.load(); const tool = f.tool();
  assert.equal(tool.name, 'pleat_read_skill');
  assert.deepEqual(tool.parameters.properties.installationId.enum, ['tool-a']);
  assert.equal(f.calls(), 0); assert.equal(f.build(), 'ordinary task'); // Schema is not delivery.
  const result = tool.execute('call-1', { installationId: a.entry.installationId });
  assert.equal(result.isError, false); assert.equal(typeof result.then, 'undefined');
  const nextRequest = { messages: [{ role: 'toolResult', toolCallId: 'call-1', toolName: tool.name, content: result.content }] };
  assert.equal(nextRequest.messages[0].content[0].text.includes(a.body), true);
  assert.equal(nextRequest.messages[0].content[0].text.includes(a.adap), true);
  assert.equal(f.calls(), 1); f.delivered(result, a); f.accept();
  assert.equal(f.build({ ...f.ctx, runId: 'next-run' }), 'ordinary task'); // Delivered pair is deduplicated.
  f.update(a, a.body + '\nupdated', 'UPDATED-ADAP');
  const refreshed = f.build({ ...f.ctx, runId: 'changed-run' });
  assert.equal(refreshed.includes(a.body), true); assert.equal(refreshed.includes('UPDATED-ADAP'), true);
});

test('managed tool selects same-named sources only by explicit ID and supports body-only', (t) => {
  const f = fixture(t), a = f.skill('tool-source-a', null), b = f.skill('tool-source-b'); f.load();
  const tool = f.tool(), result = tool.execute('call-a', { installationId: a.entry.installationId });
  assert.equal(result.isError, false); assert.equal(result.content[0].text.includes(a.body), true);
  assert.equal(result.content[0].text.includes(b.body), false);
  assert.equal(Object.hasOwn(f.wireRequests[0], 'localDataIdentity'), false);
  assert.equal(tool.execute('wrong-id', { installationId: 'shared-name' }).isError, true);
  assert.equal(f.calls(), 1);
});

test('native manual-only source is excluded from tool schema/execution but explicit manual selection and refresh work', (t) => {
  const f = fixture(t), manual = f.skill('manual-only'), automatic = f.skill('automatic-source');
  manual.entry.automaticAllowed = false; f.load(); const tool = f.tool();
  assert.deepEqual(tool.parameters.properties.installationId.enum, [automatic.entry.installationId]);
  assert.equal(tool.execute('hidden', { installationId: manual.entry.installationId }).isError, true);
  assert.equal(f.calls(), 0);
  f.select(manual.entry.installationId);
  assert.equal(f.build().includes(manual.body), true); f.accept();
  f.update(manual, manual.body + '\nmanual changed', 'MANUAL-UPDATED-ADAP');
  const refreshed = f.build({ ...f.ctx, runId: 'manual-refresh' });
  assert.equal(refreshed.includes(manual.body), true); assert.equal(refreshed.includes('MANUAL-UPDATED-ADAP'), true);
});

test('all manual-only entries yield no automatic tool; missing/nonboolean automaticAllowed fails manifest validation', (t) => {
  const f = fixture(t), a = f.skill('visibility'); a.entry.automaticAllowed = false; f.load();
  assert.equal(f.tool(), null);
  for (const value of [undefined, null, 'true', 'false', 1, 0]) {
    a.entry.automaticAllowed = value;
    assert.equal(f.tool(), null); f.select(a.entry.installationId);
    assert.equal(f.build(), 'ordinary task');
  }
  assert.equal(f.calls(), 0);
});

test('native model visibility change revokes a retained tool and invalidates a previous selection snapshot', (t) => {
  const f = fixture(t), a = f.skill('mode-change'); f.load(); const tool = f.tool();
  f.select(a.entry.installationId); a.entry.automaticAllowed = false;
  assert.equal(tool.execute('revoked-mode', { installationId: a.entry.installationId }).isError, true);
  assert.equal(f.build(), 'ordinary task'); assert.equal(f.calls(), 0);
  f.select(a.entry.installationId); assert.equal(f.build().includes(a.body), true);
});

test('visibility changed after the FD read cannot authorize automatic source refresh', (t) => {
  const f = fixture(t), a = f.skill('mode-after-read'); f.load();
  const result = f.tool().execute('call-1', { installationId: a.entry.installationId });
  a.entry.automaticAllowed = false; f.delivered(result, a); f.accept();
  assert.equal(f.build({ ...f.ctx, runId: 'later' }), 'ordinary task');
  assert.equal(f.calls(), 1);
});

test('tool factory requires real physical session/workspace and version-2 live authority', (t) => {
  const f = fixture(t); f.skill('authority'); f.load();
  for (const bad of [{ agentId: 'foreign' }, { sessionId: undefined }, { sessionKey: '' },
    { workspaceDir: f.root }, { sandboxed: true }]) assert.equal(f.tool({ ...f.ctx, ...bad }), null);
  assert.equal(f.tool(f.ctx, () => { throw new Error('not admitted'); }), null);
  const factory = f.tools.get('pleat_read_skill');
  assert.equal(factory.create({ ...f.ctx, assertInvocationCurrent: undefined }), null);
  assert.equal(f.calls(), 0);
});

test('retained tool denies removed/rebound installations and does not adopt newly authorized IDs', (t) => {
  const f = fixture(t), a = f.skill('live-a'), b = f.skill('live-b'); f.load(); const tool = f.tool();
  f.manifest.skills = f.manifest.skills.filter((entry) => entry !== a.entry);
  assert.equal(tool.execute('removed', { installationId: a.entry.installationId }).isError, true);
  b.entry.installationSkillPath = a.entry.installationSkillPath;
  assert.equal(tool.execute('rebound', { installationId: b.entry.installationId }).isError, true);
  const c = f.skill('new-id');
  assert.equal(tool.execute('new', { installationId: c.entry.installationId }).isError, true);
  assert.equal(f.calls(), 0);
});

test('retained tool fails after original link replacement, reset or disposer', (t) => {
  const f = fixture(t), a = f.skill('retained'); f.load(); const tool = f.tool();
  fs.unlinkSync(a.installed); fs.mkdirSync(a.installed);
  assert.equal(tool.execute('link', { installationId: a.entry.installationId }).isError, true);
  fs.rmdirSync(a.installed); fs.symlinkSync(a.source, a.installed, 'dir');
  f.emit('before_reset'); assert.equal(tool.execute('reset', { installationId: a.entry.installationId }).isError, true);
  const fresh = f.tool(); f.nativeDispose();
  assert.equal(fresh.execute('dispose', { installationId: a.entry.installationId }).isError, true);
  assert.equal(f.calls(), 0);
});

test('tool cancellation and revoked authority before/after FD release no source bytes', (t) => {
  let abort = false, revoke = false;
  const f = fixture(t, { insideReader() { abort = true; revoke = true; } }), a = f.skill('cancel'); f.load();
  const controller = new AbortController(); controller.abort();
  const tool = f.tool(f.ctx, () => { if (revoke) throw new Error('private authority error'); });
  assert.equal(tool.execute('pre', { installationId: a.entry.installationId }, controller.signal).isError, true);
  const result = tool.execute('mid', { installationId: a.entry.installationId }, { get aborted() { return abort; } });
  assert.equal(result.isError, true); assert.equal(result.content[0].text.includes(a.body), false);
  assert.equal(result.content[0].text.includes('private'), false); assert.equal(f.calls(), 1);
  assert.equal(f.build(), 'ordinary task');
});

test('tool same-FD revision/response failures remain generic and never downgrade invalid local-data', (t) => {
  const f = fixture(t, { transformResponse: (r) => ({ ...r, skillDigest: '0'.repeat(64) }) }), a = f.skill('bad-response');
  f.load(); const tool = f.tool(); const failed = tool.execute('bad', { installationId: a.entry.installationId });
  assert.equal(failed.isError, true); assert.equal(failed.content[0].text.includes(a.adap), false);
  fs.renameSync(path.join(a.source, 'local-data'), path.join(a.source, 'old-local'));
  fs.symlinkSync(path.join(a.source, 'old-local'), path.join(a.source, 'local-data'), 'dir');
  assert.equal(tool.execute('symlink', { installationId: a.entry.installationId }).isError, true);
  assert.equal(f.calls(), 1);
});

test('tool post-read cancellation independently blocks publication and does not register a source', (t) => {
  let aborted = false;
  const f = fixture(t, { insideReader() { aborted = true; } }), a = f.skill('abort-only'); f.load();
  const result = f.tool().execute('call-1', { installationId: a.entry.installationId }, { get aborted() { return aborted; } });
  assert.equal(result.isError, true); assert.equal(result.content[0].text.includes(a.body), false);
  f.delivered(result, a); assert.equal(f.build(), 'ordinary task'); assert.equal(f.calls(), 1);
});

test('tool rejects a source/install chain race during the same FD transaction', (t) => {
  let a;
  const f = fixture(t, { insideReader() { fs.unlinkSync(a.installed); fs.mkdirSync(a.installed); } });
  a = f.skill('tool-race'); f.load();
  const result = f.tool().execute('call-1', { installationId: a.entry.installationId });
  assert.equal(result.isError, true); assert.equal(result.content[0].text.includes(a.adap), false);
  assert.equal(f.calls(), 1);
});

test('uninstall after FD result prevents after_tool_call from authorizing a refresh', (t) => {
  const f = fixture(t), a = f.skill('uninstall-after'); f.load();
  const result = f.tool().execute('call-1', { installationId: a.entry.installationId });
  f.manifest.skills = []; f.delivered(result, a); f.accept();
  assert.equal(f.build({ ...f.ctx, runId: 'later' }), 'ordinary task');
  assert.equal(f.calls(), 1);
});

test('foreign event tool/call/run metadata cannot confer authority on a pending managed result', (t) => {
  const f = fixture(t), a = f.skill('event-binding'); f.load();
  const result = f.tool().execute('call-1', { installationId: a.entry.installationId });
  const event = { toolName: 'pleat_read_skill', toolCallId: 'call-1', runId: f.ctx.runId,
    params: { installationId: a.entry.installationId }, result };
  const context = { ...f.ctx, toolName: event.toolName, toolCallId: event.toolCallId };
  f.emit('after_tool_call', { ...event, toolName: 'read' }, context);
  f.emit('after_tool_call', { ...event, toolCallId: 'foreign-call' }, context);
  assert.equal(f.build(), 'ordinary task');
  f.emit('after_tool_call', event, { ...context, runId: 'foreign-run' });
  f.emit('after_tool_call', event, context); // Mismatched event already invalidated this pending receipt.
  f.accept(); assert.equal(f.build({ ...f.ctx, runId: 'later' }), 'ordinary task');
});

test('successful closure without matching complete after_tool_call never registers refresh or commits seen', (t) => {
  const f = fixture(t), a = f.skill('not-delivered'); f.load(); const tool = f.tool();
  const result = tool.execute('call-1', { installationId: a.entry.installationId });
  assert.equal(f.build(), 'ordinary task');
  f.delivered({ ...result, content: [{ type: 'text', text: 'short status' }] }, a);
  f.accept(); f.update(a, a.body + 'changed');
  assert.equal(f.build({ ...f.ctx, runId: 'later' }), 'ordinary task');
  assert.equal(f.calls(), 1);
});

test('after_tool_call has exact session/name/call/run binding; failure does not deduplicate the selected source', (t) => {
  const f = fixture(t), a = f.skill('tool-events'); f.load(); const tool = f.tool();
  const result = tool.execute('call-1', { installationId: a.entry.installationId });
  f.delivered(result, a, 'call-1', { ...f.ctx, sessionId: 'foreign-session' });
  assert.equal(f.build(), 'ordinary task');
  f.delivered(result, a); f.accept(f.ctx, false);
  assert.equal(f.build({ ...f.ctx, runId: 'retry' }).includes(a.body), true);
});

test('tool result dedup requires a subsequent model call and exact successful run end', (t) => {
  const f = fixture(t), a = f.skill('tool-accept'); f.load();
  const result = f.tool().execute('call-1', { installationId: a.entry.installationId });
  f.delivered(result, a); f.emit('agent_end', { success: true, runId: f.ctx.runId,
    messages: endMessages('ordinary task', [{ role: 'toolResult', toolName: 'pleat_read_skill',
      toolCallId: 'call-1', content: result.content, isError: false }]) }); // No subsequent model call.
  assert.equal(f.build({ ...f.ctx, runId: 'retry' }).includes(a.body), true);
});

test('existing third-party tool name keeps ownership; no native builtin replacement is registered', (t) => {
  const foreign = { create: () => ({ name: 'foreign' }) };
  const f = fixture(t, { tools: [['pleat_read_skill', foreign]] }); f.skill('tool-conflict'); f.load();
  assert.equal(f.tools.get('pleat_read_skill'), foreign); assert.equal(f.tools.has('read'), false);
  assert.equal(f.calls(), 0);
});

test('tool schema and pending deliveries have bounded IDs and bounded FD admission', (t) => {
  const f = fixture(t); for (let i = 0; i < 130; i++) f.skill('id-' + i); f.load(); const tool = f.tool();
  assert.equal(tool.parameters.properties.installationId.enum.length, 128);
  for (let i = 0; i < 16; i++) assert.equal(tool.execute('call-' + i, { installationId: 'id-' + i }).isError, false);
  assert.equal(tool.execute('overflow', { installationId: 'id-16' }).isError, true);
  assert.equal(tool.execute('call-0', { installationId: 'id-0' }).isError, true);
  assert.equal(f.calls(), 16);
});

test('explicit body-only loads a normal skill with no ADAPTATION in the same source FD transaction', (t) => {
  const f = fixture(t), a = f.skill('no-local-data', null); f.load(); f.select(a.entry.installationId);
  assert.ok(f.build().includes(a.body));
  assert.equal(Object.hasOwn(f.wireRequests[0], 'localDataIdentity'), false);
  assert.equal(f.wireRequests[0].includeSkillText, true);
});

test('normal local-data directory without ADAPTATION may use explicit body-only', (t) => {
  const f = fixture(t), a = f.skill('no-adaptation', null); fs.mkdirSync(path.join(a.source, 'local-data'));
  f.load(); f.select(a.entry.installationId); assert.ok(f.build().includes(a.body));
  assert.equal(Object.hasOwn(f.wireRequests[0], 'localDataIdentity'), false);
});

test('body-only protocol must omit local identity, return empty ADAP digest and checked skill text', (t) => {
  const req = { schemaVersion: 1, sourceRoot: request.sourceRoot, sourceIdentity: request.sourceIdentity, includeSkillText: true };
  let change = (r) => r;
  mockExec(t, (_file, _args, settings) => {
    assert.equal(settings.maxBuffer, 2097152); assert.deepEqual(JSON.parse(settings.input), req);
    return encoded(change({ ...req, text: '', digest: sha(''), skillText: 'BODY', skillDigest: sha('BODY'), skillIdentity: '1:4' }));
  });
  const reader = createAdaptationReader({ readerExecutable: '/fixture/reader' });
  assert.equal(reader.read(req).skillText, 'BODY');
  for (change of [(r) => ({ ...r, localDataIdentity: '1:3' }), (r) => ({ ...r, text: 'foreign', digest: sha('foreign') }),
    (r) => ({ ...r, skillDigest: '0'.repeat(64) }), (r) => ({ ...r, skillIdentity: 'bad' })]) assert.equal(reader.read(req), undefined);
});

test('expectedSkillDigest confirms full native visible bytes; status/partial/different body do not match', (t) => {
  const f = fixture(t), a = f.skill('proof');
  const reader = createAdaptationReader({ readerExecutable: '/fixture/reader' });
  const req = { schemaVersion: 1, sourceRoot: a.source, sourceIdentity: a.entry.sourceIdentity,
    localDataIdentity: id(path.join(a.source, 'local-data')), expectedSkillDigest: sha(a.body) };
  assert.equal(reader.read(req).skillDigest, sha(a.body));
  for (const text of ['Skill instructions were already served whole earlier in the current model context.',
    'Skill instructions cannot be partially served', a.body.slice(0, -1), 'FOREIGN-BODY']) {
    assert.equal(reader.read({ ...req, expectedSkillDigest: sha(text) }), undefined);
  }
});

test('invalid optional FD request fields and absent-local legacy requests never execute a reader', (t) => {
  mockExec(t, () => assert.fail('invalid optional protocol spawned'));
  const reader = createAdaptationReader({ readerExecutable: '/fixture/reader' });
  for (const req of [{ ...request, expectedSkillDigest: '0'.repeat(63) },
    { ...request, includeSkillText: 'true' }, { schemaVersion: 1, sourceRoot: request.sourceRoot,
      sourceIdentity: request.sourceIdentity }, { ...request, localDataIdentity: undefined, includeSkillText: true }]) {
    assert.equal(reader.read(req), undefined);
  }
});

test('identity/root/chain or same-name foreign installation cannot borrow source authority', (t) => {
  const f = fixture(t), a = f.skill('source-a'), b = f.skill('source-b'); f.load();
  fs.unlinkSync(a.installed); fs.symlinkSync(b.source, a.installed, 'dir');
  f.select(a.entry.installationId); assert.equal(f.build(), 'ordinary task'); assert.equal(f.calls(), 0);
  f.select(b.entry.installationId); assert.ok(f.build().includes(b.body)); assert.equal(f.build().includes(a.body), false);
});

test('local-data symlink, ADAPTATION symlink and FD error never downgrade to body-only', (t) => {
  const f = fixture(t), a = f.skill('protected'); f.load(); f.select(a.entry.installationId);
  const local = path.join(a.source, 'local-data'), moved = local + '-held';
  fs.renameSync(local, moved); fs.symlinkSync(moved, local, 'dir');
  assert.equal(f.build(), 'ordinary task'); assert.equal(f.calls(), 0);
  fs.unlinkSync(local); fs.renameSync(moved, local);
  const adap = path.join(local, 'ADAPTATION.md'); fs.renameSync(adap, adap + '.held'); fs.symlinkSync(adap + '.held', adap);
  f.select(a.entry.installationId); assert.equal(f.build(), 'ordinary task'); assert.equal(f.calls(), 0);
});

test('live manifest removal or descriptor rebinding cancels selection before reading bytes', (t) => {
  const f = fixture(t), a = f.skill('original'), b = f.skill('replacement'); f.load(); f.select(a.entry.installationId);
  f.manifest.skills[0] = { ...b.entry, installationId: a.entry.installationId };
  assert.equal(f.build(), 'ordinary task'); assert.equal(f.calls(), 0);
});

test('real agent/session/workspace scope is checked; command payload has no invented workspace field', (t) => {
  const f = fixture(t), a = f.skill('scoped'); f.load();
  f.select(a.entry.installationId, { ...f.ctx, agentId: 'foreign-agent' }); assert.equal(f.calls(), 0);
  f.select(a.entry.installationId);
  assert.equal(f.build({ ...f.ctx, sessionId: 'another-physical' }), 'ordinary task');
  const foreignWorkspace = path.join(f.root, 'foreign-workspace'); fs.mkdirSync(foreignWorkspace);
  assert.equal(f.build({ ...f.ctx, workspaceDir: foreignWorkspace }), 'ordinary task'); assert.equal(f.calls(), 0);
  f.select(a.entry.installationId); assert.equal(f.build({ ...f.ctx, sandboxed: true }), 'ordinary task');
  assert.ok(f.build().includes(a.body));
});

test('successful native model run commits source+digest dedup; failures/rebuilds do not lose context', (t) => {
  const f = fixture(t), a = f.skill('dedup'); f.load(); f.select(a.entry.installationId);
  assert.ok(f.build().includes(a.body)); assert.ok(f.build().includes(a.body));
  f.accept(f.ctx, false);
  const next = { ...f.ctx, runId: 'run-2' }; assert.ok(f.build(next).includes(a.body)); f.accept(next);
  const third = { ...f.ctx, runId: 'run-3' }; assert.equal(f.build(third), 'ordinary task');
  f.update(a, undefined, 'ADAP-UPDATED');
  assert.ok(f.build(third).includes('ADAP-UPDATED')); f.accept(third);
  const fourth = { ...f.ctx, runId: 'run-4' }; f.update(a, 'BODY-UPDATED');
  const text = f.build(fourth); assert.ok(text.includes('BODY-UPDATED')); assert.ok(text.includes('ADAP-UPDATED'));
});

test('agent_end without a native model_call_started observation cannot commit dedup', (t) => {
  const f = fixture(t), a = f.skill('no-model'); f.load(); f.select(a.entry.installationId); const projected = f.build(); assert.ok(projected.includes(a.body));
  f.emit('agent_end', { success: true, runId: f.ctx.runId, messages: endMessages(projected) });
  assert.ok(f.build({ ...f.ctx, runId: 'run-2' }).includes(a.body));
});

test('P2: frozen native user projection A cannot acknowledge rebuilt B; B deduplicates only after real projection delivery', (t) => {
  const f = fixture(t), a = f.skill('frozen-projection'); f.load(); f.select(a.entry.installationId);
  const frozenA = f.build(); assert.equal(frozenA.includes(a.adap), true); f.accept(f.ctx, false, endMessages(frozenA));
  f.update(a, a.body + '\nBODY-B', 'ADAP-B'); const rebuiltB = f.build();
  assert.equal(rebuiltB.includes('BODY-B'), true); assert.equal(rebuiltB.includes('ADAP-B'), true);
  const submittedFallback = frozenA ?? rebuiltB; // Actual consumer's frozen-first rule.
  assert.equal(submittedFallback.includes('ADAP-B'), false); f.accept(f.ctx, true, endMessages(submittedFallback));
  const fresh = { ...f.ctx, runId: 'fresh-after-fallback' }, firstActualB = f.build(fresh);
  assert.equal(firstActualB.includes('BODY-B'), true); assert.equal(firstActualB.includes('ADAP-B'), true);
  f.accept(fresh, true, endMessages(firstActualB));
  assert.equal(f.build({ ...f.ctx, runId: 'after-delivered-B' }), 'ordinary task');
});

test('tool routing map is one-to-one with authorized enum IDs and distinguishes same-named sources by both paths', (t) => {
  const f = fixture(t), a = f.skill('opaque-a'), b = f.skill('opaque-b'), hidden = f.skill('manual-hidden'), foreign = f.skill('foreign-hidden');
  hidden.entry.automaticAllowed = false; foreign.entry.agentId = 'foreign-agent'; f.load(); const tool = f.tool();
  const rows = JSON.parse(tool.description.split('\nAuthorized installations:\n')[1]);
  assert.deepEqual(rows.map((row) => row.installationId), tool.parameters.properties.installationId.enum);
  assert.equal(rows.every((row) => Buffer.byteLength(JSON.stringify(row)) <= configurationContract.maxToolMappingEntryBytes), true);
  assert.equal(Buffer.byteLength(tool.description) <= configurationContract.maxToolDescriptionBytes, true);
  assert.equal(Buffer.byteLength(JSON.stringify(tool)) <= configurationContract.maxToolSchemaBytes, true);
  assert.deepEqual(rows, [a, b].map(({ entry }) => ({ installationId: entry.installationId,
    installationSkillPath: entry.installationSkillPath, canonicalSourceRoot: entry.canonicalSourceRoot })));
  assert.equal(tool.description.includes(a.entry.installationSkillPath), true);
  assert.equal(tool.description.includes(b.entry.canonicalSourceRoot), true);
  assert.equal(tool.description.includes(hidden.source), false); assert.equal(tool.description.includes(foreign.source), false);
  assert.equal(f.calls(), 0);
});

test('manual help lists live authorized installation paths without selecting or reading bodies', (t) => {
  const f = fixture(t), automatic = f.skill('list-auto'), manual = f.skill('list-manual'), foreign = f.skill('list-foreign');
  manual.entry.automaticAllowed = false; foreign.entry.agentId = 'other-agent'; f.load();
  for (const args of ['', 'help', 'list']) {
    const reply = f.select(args).text;
    const value = JSON.parse(reply.slice(reply.indexOf('{')));
    assert.deepEqual(value.installations.map((r) => r.installationId), [automatic.entry.installationId, manual.entry.installationId]);
    assert.equal(value.installations[0].installationSkillPath, automatic.entry.installationSkillPath);
    assert.equal(value.installations[1].canonicalSourceRoot, manual.entry.canonicalSourceRoot);
    assert.equal(value.nextOffset, null); assert.equal(reply.includes(foreign.source), false);
  }
  assert.equal(f.calls(), 0); assert.equal(f.build(), 'ordinary task');
  assert.equal(f.select('list 2').text.includes(automatic.source), false);
});

test('manual listing is authorized, session scoped and bounded with a usable continuation offset', (t) => {
  const f = fixture(t); for (let i = 0; i < configurationContract.maxToolIds + 2; i++) f.skill('list-' + i);
  f.load(); const handler = f.commands.get('pleat-adaptation').handler;
  const denied = handler({ ...f.ctx, args: 'list', isAuthorizedSender: false }).text;
  assert.equal(denied.includes('installations'), false);
  assert.equal(handler({ args: 'list', isAuthorizedSender: true }).text.includes('installations'), false);
  const first = f.select('list').text, rows = JSON.parse(first.slice(first.indexOf('{')));
  assert.ok(rows.installations.length > 0 && rows.installations.length <= configurationContract.maxToolIds);
  assert.ok(rows.nextOffset > 0); assert.ok(Buffer.byteLength(first) <= configurationContract.maxToolDescriptionBytes);
  const next = f.select('list ' + rows.nextOffset).text, tail = JSON.parse(next.slice(next.indexOf('{')));
  assert.equal(tail.nextOffset, null);
  assert.equal(new Set([...rows.installations, ...tail.installations].map((r) => r.installationId)).size, configurationContract.maxToolIds + 2);
  assert.equal(f.select('list 9999').text.includes('installations'), false);
  assert.equal(f.calls(), 0); assert.equal(f.build(), 'ordinary task');
});

test('missing/forged/stale/partial receipts and non-user echoes cannot acknowledge offered context', (t) => {
  const f = fixture(t), a = f.skill('receipt-proof'); f.load(); f.select(a.entry.installationId);
  const cases = [
    () => undefined,
    (p) => [{ role: 'user', content: p }], // Transcript content is not the native model projection.
    (p) => [{ role: 'user', content: 'ordinary task', __openclaw: { modelPromptProjection: { version: 2, text: p } } }],
    (p) => endMessages(p.replace(a.body, 'truncated body')),
    (p) => endMessages(p.replace(/(receipt v1 )([a-f0-9])/u, (_m, prefix, first) => prefix + (first === '0' ? '1' : '0'))),
    (p) => endMessages(p.slice(p.lastIndexOf('[PLEAT context receipt'))), // Receipt alone is insufficient.
    (p) => endMessages(p + '\n' + p.slice(p.lastIndexOf('[PLEAT context receipt'))),
    (p) => [...endMessages(p), ...endMessages('newer user without receipt')],
    (p) => [endMessages('ordinary task')[0], { role: 'assistant', content: [{ type: 'text', text: p }] }],
    (p) => { const messages = endMessages(p); messages[0].excludeFromContext = true; return messages; },
    (p) => endMessages(p + 'x'.repeat(configurationContract.maxProjectionBytes)),
    (p) => Array(configurationContract.maxEndMessages + 1).fill(endMessages(p)[0]),
  ];
  for (let i = 0; i < cases.length; i++) {
    const context = { ...f.ctx, runId: 'negative-receipt-' + i }, offered = f.build(context);
    assert.equal(offered.includes(a.body), true); assert.equal(/receipt v1 [a-f0-9]{64} [a-f0-9]{64}\]/u.test(offered), true);
    f.emit('model_call_started', {}, context);
    f.emit('agent_end', { success: true, runId: context.runId, messages: cases[i](offered) }, context);
    assert.equal(f.build({ ...context, runId: context.runId + '-retry' }).includes(a.body), true, 'bad receipt must remain pending: ' + i);
  }
  const delivered = { ...f.ctx, runId: 'valid-receipt' }, actual = f.build(delivered);
  const messages = endMessages(actual); messages[0].content = [{ type: 'text', text: 'ordinary task' }, { type: 'image', data: 'owned-fixture' }];
  f.accept(delivered, true, messages);
  assert.equal(f.build({ ...f.ctx, runId: 'after-valid-receipt' }), 'ordinary task');
});

test('receipt from before compaction, reset or another physical scope cannot commit a new offer', (t) => {
  const f = fixture(t), a = f.skill('late-receipt'); f.load(); f.select(a.entry.installationId);
  const old = f.build(); f.emit('after_compaction'); const rebuilt = f.build();
  assert.notEqual(rebuilt, old); f.accept(f.ctx, true, endMessages(old));
  const retry = { ...f.ctx, runId: 'after-late-compaction' }; assert.equal(f.build(retry).includes(a.body), true);
  f.emit('before_reset', {}, retry); f.select(a.entry.installationId, retry); const afterReset = f.build(retry);
  f.accept(retry, true, endMessages(old));
  const fresh = { ...f.ctx, runId: 'after-late-reset' }; assert.equal(f.build(fresh).includes(a.body), true);
  f.accept({ ...fresh, sessionId: 'foreign-physical' }, true, endMessages(afterReset));
  assert.equal(f.build(fresh).includes(a.body), true);
  f.emit('model_call_started', {}, fresh);
  f.emit('agent_end', { runId: 'different-run', success: true, messages: endMessages(f.build(fresh)) }, fresh);
  assert.equal(f.build({ ...fresh, runId: 'after-mismatched-run' }).includes(a.body), true);
});

test('tool delivery dedup also requires the exact own toolResult after the latest user in terminal messages', (t) => {
  const f = fixture(t), a = f.skill('tool-projection'); f.load();
  const result = f.tool().execute('call-1', { installationId: a.entry.installationId }); f.delivered(result, a);
  f.accept(f.ctx, true, endMessages('ordinary task')); // Complete after_tool_call alone does not prove model delivery.
  assert.equal(f.build({ ...f.ctx, runId: 'tool-no-message' }).includes(a.body), true);
  f.emit('before_reset'); f.load(); const another = f.tool().execute('call-2', { installationId: a.entry.installationId });
  f.delivered(another, a, 'call-2');
  f.accept(f.ctx, true, endMessages('ordinary task', [{ role: 'toolResult', toolName: 'pleat_read_skill',
    toolCallId: 'call-2', content: [{ type: 'text', text: 'short status' }], isError: false }]));
  assert.equal(f.build({ ...f.ctx, runId: 'tool-partial-message' }).includes(a.body), true);
});

test('tool routing aggregate budget fails closed without a partial enum/map or any FD call', (t) => {
  const f = fixture(t); for (let i = 0; i < 128; i++) f.skill('long-id-' + i + '-' + 'x'.repeat(100));
  f.load(); assert.equal(f.tool(), null); assert.equal(f.calls(), 0);
  const a = f.records[0]; f.select(a.entry.installationId); assert.equal(f.build().includes(a.body), true);
  assert.equal(f.calls(), 1); // Routing budget does not prevent explicit manual context.
});

test('tool routing per-entry path budget fails closed while live metadata remains valid for manual use', (t) => {
  const f = fixture(t), a = f.skill('long-route-' + 'x'.repeat(100)); f.load(); assert.ok(f.tool());
  const longPath = (base, target) => {
    let value = base;
    while (target - value.length > 1) value = path.join(value, 'p'.repeat(Math.min(200, target - value.length - 1)));
    return value;
  };
  const source = longPath(path.join(f.root, 'long-source'), 990);
  fs.mkdirSync(path.dirname(source), { recursive: true }); fs.renameSync(a.source, source); a.source = source;
  const installed = longPath(path.join(f.root, 'long-install'), 980);
  fs.mkdirSync(path.dirname(installed), { recursive: true }); fs.symlinkSync(source, installed, 'dir');
  a.entry.canonicalSourceRoot = source; a.entry.installationSkillPath = path.join(installed, 'SKILL.md');
  const row = { installationId: a.entry.installationId, installationSkillPath: a.entry.installationSkillPath, canonicalSourceRoot: source };
  assert.equal(Buffer.byteLength(JSON.stringify(row)) > configurationContract.maxToolMappingEntryBytes, true);
  assert.equal(f.tool(), null); assert.equal(f.calls(), 0);
  f.select(a.entry.installationId); assert.equal(f.build().includes(a.body), true);
});

test('alias IDs of the same source deduplicate; different sources with same native name do not', (t) => {
  const f = fixture(t), a = f.skill('alias-a'), b = f.skill('same-name-foreign');
  const alias = path.join(f.root, 'installed', 'alias-b'); fs.symlinkSync(a.source, alias, 'dir');
  f.manifest.skills.push({ ...a.entry, installationId: 'alias-b', installationSkillPath: path.join(alias, 'SKILL.md') });
  f.load(); f.select('alias-a'); f.select('alias-b'); f.select(b.entry.installationId);
  const text = f.build(); assert.equal(text.split(a.adap).length - 1, 1); assert.ok(text.includes(b.adap));
  assert.equal(f.calls(), 2);
});

test('source migration with a compatibility locator retains dev:ino and canonical FD authority', (t) => {
  const f = fixture(t), a = f.skill('move'); f.load(); f.select(a.entry.installationId);
  const original = path.dirname(a.source), moved = original + '-moved';
  fs.renameSync(original, moved); fs.symlinkSync(moved, original, 'dir');
  const text = f.build(); assert.ok(text.includes(a.body));
  assert.equal(f.wireRequests[0].sourceRoot, path.join(moved, 'move'));
  assert.equal(f.wireRequests[0].sourceIdentity, a.entry.sourceIdentity);
});

test('chain replacement during FD transaction discards all returned context', (t) => {
  let a, relay, f;
  f = fixture(t, { insideReader() { fs.unlinkSync(a.installed); fs.symlinkSync(relay, a.installed, 'dir'); } });
  a = f.skill('race'); relay = path.join(f.root, 'relay'); fs.symlinkSync(a.source, relay, 'dir');
  f.load(); f.select(a.entry.installationId); assert.equal(f.build(), 'ordinary task'); assert.equal(f.calls(), 1);
});

test('reset/end/start producers use physical session identity; late old-session end cannot clear new state', (t) => {
  const f = fixture(t), a = f.skill('reset'); f.load(); f.select(a.entry.installationId);
  f.emit('before_reset'); assert.equal(f.build(), 'ordinary task');
  const newer = { ...f.ctx, sessionId: 'physical-2', runId: 'new-run' };
  f.select(a.entry.installationId, newer); f.emit('session_start', { sessionId: newer.sessionId }, newer);
  f.emit('session_end', { sessionId: f.ctx.sessionId }, f.ctx);
  assert.ok(f.build(newer).includes(a.body));
  f.emit('session_start', { sessionId: newer.sessionId }, newer); // same-id detached start is not a reset.
  assert.ok(f.build(newer).includes(a.body)); f.accept(newer);
  f.emit('after_compaction', { compactedCount: 1 }, newer); assert.ok(f.build(newer).includes(a.body));
  f.emit('session_end', { sessionId: newer.sessionId }, newer); assert.equal(f.build(newer), 'ordinary task');
});

test('third-party command/hooks keep ownership; no native/user override or automatic-name trust flag exists', (t) => {
  let invoked = 0;
  const foreign = { handler() { invoked++; return { text: 'foreign command' }; } };
  const f = fixture(t, { commands: [['pleat-adaptation', foreign]] }), a = f.skill('conflict');
  f.hooks.set('before_prompt_build', [() => ({ appendContext: 'foreign context' })]);
  const r = f.load({ builtinRead: true, automatic: true, nativeWinner: a.entry });
  f.select(a.entry.installationId); assert.equal(invoked, 1);
  assert.equal(f.commands.get('pleat-adaptation'), foreign);
  assert.equal(f.build(), 'ordinary task\n\nforeign context'); assert.equal(f.calls(), 0);
  assert.equal(r.stockRead, false); assert.equal(capability.stockRead, false);
  assert.equal(f.hooks.has('tool_result_persist'), false);
});

test('duplicate descriptors, async manifest suppliers and invalid native session fields fail closed', (t) => {
  const f = fixture(t), a = f.skill('invalid'); f.manifest.skills.push({ ...a.entry }); f.load();
  f.select(a.entry.installationId); assert.equal(f.build(), 'ordinary task');
  const g = fixture(t), b = g.skill('async'); g.load({ loadManifest: () => Promise.resolve(g.manifest) });
  g.select(b.entry.installationId); assert.equal(g.build(), 'ordinary task');
});

test('per-prompt reads stay bounded and native disposer disables retained callbacks', (t) => {
  const f = fixture(t), records = ['cap-a', 'cap-b', 'cap-c'].map((name) => f.skill(name));
  const r = f.load(); assert.equal(f.load(), r);
  for (const record of records) f.select(record.entry.installationId);
  const first = f.build(); assert.equal(f.calls(), 2); f.accept();
  const second = f.build({ ...f.ctx, runId: 'run-2' });
  assert.equal(f.calls(), 4); assert.ok([first, second].join('').includes(records[2].body));
  f.nativeDispose(); assert.equal(f.build(), 'ordinary task'); assert.equal(f.calls(), 4); r.dispose();
});

test('the documented test command supplies explicit env-i PATH/HOME/LANG without exposing environment values', () => {
  assert.equal(process.env.PATH === '/opt/homebrew/bin:/usr/bin:/bin', true);
  assert.equal(process.env.HOME === '/tmp', true);
  assert.equal(process.env.LANG === 'C.UTF-8', true);
  assert.equal(Object.hasOwn(process.env, 'NODE_OPTIONS'), false);
});

test('combined FD failure makes no source-only retry and preserves the next prompt', (t) => {
  const f = fixture(t, { insideReader() { throw new Error('mock native anchored transaction rejected'); } });
  const a = f.skill('no-downgrade'); f.load(); f.select(a.entry.installationId);
  assert.equal(f.build(), 'ordinary task'); assert.equal(f.calls(), 1);
  assert.equal(f.wireRequests[0].localDataIdentity, id(path.join(a.source, 'local-data')));
});

test('a discarded prompt rebuild cannot commit a previously offered digest', (t) => {
  const f = fixture(t), a = f.skill('discarded'); f.load(); f.select(a.entry.installationId);
  assert.ok(f.build().includes(a.body));
  const descriptor = f.manifest.skills[0]; f.manifest.skills[0] = { ...descriptor, sourceIdentity: '0:0' };
  assert.equal(f.build(), 'ordinary task'); f.accept(); f.manifest.skills[0] = descriptor;
  f.select(a.entry.installationId); assert.ok(f.build({ ...f.ctx, runId: 'run-2' }).includes(a.body));
});

test('extended stdout and SKILL bounds reject oversized/malformed responses', (t) => {
  const req = { ...request, includeSkillText: true };
  let output;
  mockExec(t, () => output);
  const reader = createAdaptationReader({ readerExecutable: '/fixture/reader' });
  const result = { ...response(request), skillIdentity: '1:4', skillText: 'x'.repeat(262144), skillDigest: sha('x'.repeat(262144)) };
  output = encoded(result); assert.equal(reader.read(req).skillText.length, 262144);
  output = encoded({ ...result, skillText: 'x'.repeat(262145), skillDigest: sha('x'.repeat(262145)) });
  assert.equal(reader.read(req), undefined);
  output = Buffer.alloc(2097153); assert.equal(reader.read(req), undefined);
  output = encoded({ ...result, skillIdentity: 'invalid' }); assert.equal(reader.read(req), undefined);
});

test('session and selected-installation capacity reject overflow without evicting authorized state', (t) => {
  const f = fixture(t), a = f.skill('sessions'); f.load();
  for (let i = 0; i < 64; i++) f.select(a.entry.installationId, { ...f.ctx, sessionId: `physical-${i}` });
  const overflow = { ...f.ctx, sessionId: 'overflow' };
  assert.equal(f.select(a.entry.installationId, overflow).text.includes('无法选择'), true);
  assert.equal(f.build(overflow), 'ordinary task'); assert.ok(f.build({ ...f.ctx, sessionId: 'physical-0' }).includes(a.body));
  const g = fixture(t), skills = Array.from({ length: 17 }, (_, i) => g.skill(`source-${i}`)); g.load();
  for (const s of skills.slice(0, 16)) assert.equal(g.select(s.entry.installationId).text.includes('已选择'), true);
  assert.equal(g.select(skills[16].entry.installationId).text.includes('无法选择'), true);
});

test('bounded source window stays stable across same-run prompt rebuilds', (t) => {
  const f = fixture(t), records = ['stable-a', 'stable-b', 'stable-c'].map((name) => f.skill(name)); f.load();
  for (const r of records) f.select(r.entry.installationId);
  const first = f.build(), rebuilt = f.build();
  assert.equal(rebuilt, first); assert.equal(f.calls(), 4);
  assert.equal(first.includes(records[1].body), false);
  assert.equal(first.includes(records[2].body), true);
});

test('a rejected same-session scope rebuild invalidates the old offer before model lifecycle events', (t) => {
  const f = fixture(t), a = f.skill('scope-rebuild'); f.load(); f.select(a.entry.installationId);
  assert.ok(f.build().includes(a.body));
  const rejected = { ...f.ctx, sandboxed: true }; assert.equal(f.build(rejected), 'ordinary task'); f.accept(rejected);
  assert.ok(f.build({ ...f.ctx, runId: 'run-2' }).includes(a.body));
});
