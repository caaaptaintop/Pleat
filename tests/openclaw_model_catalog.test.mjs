import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import vm from 'node:vm';
import { createHash, randomBytes } from 'node:crypto';
import { fileURLToPath, pathToFileURL } from 'node:url';

// Evaluate the current source candidate with only its import bindings stubbed.
// No installed adapter, OpenClaw runtime, executable reader, or model is started.
const adapterSource = fs.readFileSync(new URL('../assets/host-adapters/openclaw-adaptation.js', import.meta.url), 'utf8');
const entrySource = fs.readFileSync(new URL('../assets/host-adapters/openclaw-plugin-entry.js', import.meta.url), 'utf8');
const digest = (text) => createHash('sha256').update(text).digest('hex');
const identity = (file) => { const s = fs.lstatSync(file, { bigint: true }); return `${s.dev}:${s.ino}`; };
const xml = (value) => value.replaceAll('&', '&amp;').replaceAll('<', '&lt;').replaceAll('>', '&gt;')
  .replaceAll('"', '&quot;').replaceAll("'", '&apos;');
const catalogue = (location, name = 'model-only', description = true) => '<available_skills>\n  <skill>\n' +
  `    <name>${xml(name)}</name>\n` + (description ? '    <description>A &amp; B workflow</description>\n' : '') +
  `    <location>${xml(location)}</location>\n    <location_note>Read &quot;fully&quot;.</location_note>\n` +
  '  </skill>\n</available_skills>';
function evaluate(source, bindings, name, url) {
  const transformed = source.replace(/^import .*;\n/gmu, '')
    .replaceAll('export default function ', 'function ').replaceAll('export function ', 'function ')
    .replaceAll('export const ', 'const ').replaceAll('import.meta.url', JSON.stringify(url));
  const context = vm.createContext({ ...bindings, Buffer, TextDecoder, TextEncoder });
  vm.runInContext(`${transformed}\nglobalThis.entryPoint = ${name};`, context, { filename: url });
  return context.entryPoint;
}
function fixture(t, options = {}) {
  const temp = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'pleat-openclaw-catalog-unit-')));
  const workspace = path.join(temp, 'workspace'), plugin = path.join(temp, 'plugin');
  const sourceRoot = path.join(temp, 'source & "quoted"'), installRoot = path.join(workspace, 'skills', 'installed');
  fs.mkdirSync(workspace, { recursive: true }); fs.mkdirSync(path.dirname(installRoot), { recursive: true });
  fs.mkdirSync(path.join(sourceRoot, 'local-data'), { recursive: true });
  fs.mkdirSync(path.join(plugin, 'registrations'), { recursive: true });
  fs.writeFileSync(path.join(sourceRoot, 'SKILL.md'), 'MODEL_ONLY_BODY');
  fs.writeFileSync(path.join(sourceRoot, 'local-data', 'ADAPTATION.md'), 'LOCAL_ADP');
  fs.symlinkSync(sourceRoot, installRoot, 'dir');
  const installationSkillPath = path.join(installRoot, 'SKILL.md');
  const registrationFile = path.join(plugin, 'registrations', `${'a'.repeat(24)}.json`);
  const descriptor = { installationId: 'private-model-only-id', scope: 'workspace', workspaceRoot: workspace,
    installationSkillPath, canonicalSourceRoot: sourceRoot, sourceIdentity: identity(sourceRoot) };
  fs.writeFileSync(registrationFile, JSON.stringify({ schemaVersion: 1, manager: 'pleat-native-openclaw-v1', skills: [descriptor] }));
  fs.writeFileSync(path.join(plugin, 'reader.json'), JSON.stringify({ schemaVersion: 1,
    manager: 'pleat-native-openclaw-reader-v1', readerExecutable: '/trusted/pleat-reader' }));
  const hooks = new Map(), touched = [], manifests = [];
  const f = { temp, workspace, plugin, sourceRoot, installationSkillPath, registrationFile, descriptor,
    reads: 0, commandCalls: 0, guardDenied: false, commands: options.commands ? [{ skillFile: installationSkillPath, modelVisible: true }] : [],
    config: { plugins: { enabled: true }, skills: { entries: {} } }, hooks, touched, manifests };
  const osStub = options.fixtureHome ? { homedir: () => temp } : os;
  const fsStub = new Proxy(fs, { get(target, key) {
    if (!['lstatSync', 'realpathSync', 'readlinkSync', 'openSync', 'opendirSync'].includes(key)) return target[key];
    return (file, ...args) => { touched.push(String(file)); return target[key](file, ...args); };
  } });
  const api = { config: f.config,
    on(name, callback) { assert.equal(hooks.has(name), false); hooks.set(name, callback); },
    registerTool(factory, opts) { assert.equal(opts.name, 'pleat_read_skill'); f.factory = factory; },
    registerCommand(command) { f.command = command; },
    lifecycle: { onDispose(callback) { f.dispose = callback; } } };
  const registerAdapter = evaluate(adapterSource, { fs: fsStub, path, os: osStub, createHash, randomBytes,
    execFileSync(executable, args, opts) {
      f.reads++;
      assert.equal(executable, '/trusted/pleat-reader');
      assert.deepEqual(Array.from(args), ['--pleat-native-read-adaptation-v1']);
      assert.equal(opts.shell, false);
      const request = JSON.parse(opts.input);
      assert.equal(request.sourceRoot, sourceRoot); assert.equal(request.sourceIdentity, identity(sourceRoot));
      const skillText = fs.readFileSync(path.join(sourceRoot, 'SKILL.md'), 'utf8');
      const text = request.localDataIdentity === undefined ? '' : fs.readFileSync(path.join(sourceRoot, 'local-data', 'ADAPTATION.md'), 'utf8');
      const response = { schemaVersion: 1, sourceRoot, sourceIdentity: request.sourceIdentity,
        ...(request.localDataIdentity === undefined ? {} : { localDataIdentity: request.localDataIdentity }),
        text, digest: digest(text), skillText, skillDigest: digest(skillText), skillIdentity: identity(path.join(sourceRoot, 'SKILL.md')) };
      f.onRead?.();
      return Buffer.from(JSON.stringify(response));
    } }, 'openClawAdaptation', 'current-openclaw-adaptation.js');
  const registerEntry = evaluate(entrySource, { fs: fsStub, path, os: osStub, fileURLToPath,
    resolveAgentWorkspaceDir(_cfg, agentId) { assert.equal(agentId, 'agent-a'); return workspace; },
    listSkillCommandsForWorkspace() { f.commandCalls++; return f.commands; },
    registerAdaptation(target, config) {
      f.entryConfig = config;
      f.registration = registerAdapter(target, { ...config, loadManifest(ctx, cleanCatalogue) {
        if (cleanCatalogue !== undefined) manifests.push(cleanCatalogue);
        return config.loadManifest(ctx, cleanCatalogue);
      } });
    } }, 'register', pathToFileURL(path.join(plugin, 'entry.js')).href);
  registerEntry(api);
  assert.ok(f.registration);
  f.scope = { agentId: 'agent-a', sessionKey: 'session-key', sessionId: 'physical-a', workspaceDir: workspace };
  f.runId = 'run-a';
  f.create = (overrides = {}) => f.factory.create({ ...f.scope, assertInvocationCurrent() {
    if (f.guardDenied) throw new Error('native guard rejected');
  }, ...overrides });
  f.run = (systemPrompt, overrides = {}, fields = {}) => hooks.get('before_agent_run')({ ...fields,
    ...(systemPrompt === undefined ? {} : { systemPrompt }) }, { ...f.scope, runId: f.runId, ...overrides });
  f.before = (params, callId = 'call-a', overrides = {}, eventOverrides = {}) => {
    const { workspaceDir: _workspace, ...scope } = f.scope;
    return hooks.get('before_tool_call')({ toolName: 'pleat_read_skill', runId: f.runId, toolCallId: callId,
      params, ...eventOverrides }, { ...scope, toolName: 'pleat_read_skill', runId: f.runId,
      toolCallId: callId, ...overrides });
  };
  f.close = (name, overrides = {}) => hooks.get(name)({ sessionId: f.scope.sessionId, success: true, messages: [] },
    { ...f.scope, runId: f.runId, ...overrides });
  t.after(() => { f.registration.dispose(); fs.rmSync(temp, { recursive: true, force: true }); });
  return f;
}
function success(result) {
  assert.equal(result.isError, false);
  assert.match(result.content[0].text, /MODEL_ONLY_BODY/);
  assert.match(result.content[0].text, /LOCAL_ADP/);
}
function denied(f, tool, params, callId = 'call-a', signal) {
  assert.equal(tool.execute(callId, params, signal).isError, true);
  assert.equal(f.reads, 0);
}

test('model-only: empty commands, generic shell, native catalogue -> BODY + ADP', (t) => {
  const f = fixture(t), tool = f.create(), params = { skillPath: f.installationSkillPath };
  assert.ok(tool); assert.equal(f.commands.length, 0);
  for (const secret of [f.descriptor.installationId, f.sourceRoot, f.installationSkillPath]) {
    assert.equal(JSON.stringify(tool.parameters).includes(secret), false);
    assert.equal(tool.description.includes(secret), false);
  }
  assert.equal(f.reads, 0);
  const callsBefore = f.commandCalls;
  f.run(`Private system preamble\n${catalogue(f.installationSkillPath)}\nPrivate system suffix`);
  assert.equal(f.before(params), undefined);
  success(tool.execute('call-a', params)); assert.equal(f.reads, 1);
  assert.equal(f.commandCalls, callsBefore, 'model admission/live checks must not use the commands API');
  assert.ok(f.manifests.length >= 3);
  for (const clean of f.manifests) for (const item of clean) {
    assert.equal(Object.keys(item).sort().join(','), 'location,name');
    assert.equal(JSON.stringify(item).includes('Private system'), false);
  }
  assert.equal(tool.execute('call-a', params).isError, true); assert.equal(f.reads, 1);
});

test('native system preamble mentions catalogue tags without adding another catalogue', (t) => {
  const f = fixture(t), tool = f.create(), params = { skillPath: f.installationSkillPath };
  f.run(`Scan <available_skills> for a matching workflow.\n${catalogue(f.installationSkillPath)}`);
  assert.equal(f.before(params), undefined);
  success(tool.execute('call-a', params)); assert.equal(f.reads, 1);
});

for (const [label, location] of [
  ['canonical source with escaped XML', (f) => path.join(f.sourceRoot, 'SKILL.md')],
  ['compact catalogue without description', (f) => f.installationSkillPath],
]) test(label, (t) => {
  const f = fixture(t), tool = f.create(), params = { skillPath: location(f) };
  f.run(catalogue(params.skillPath, 'name & "quoted"', label !== 'compact catalogue without description'));
  assert.equal(f.before(params), undefined); success(tool.execute('call-a', params));
});

test('tilde paths are compared lexically against registered paths', (t) => {
  const f = fixture(t, { fixtureHome: true }), tool = f.create();
  const location = `~/${path.relative(f.temp, f.installationSkillPath)}`, params = { skillPath: location };
  f.run(catalogue(location)); assert.equal(f.before(params), undefined); success(tool.execute('call-a', params));
});

const invalidCatalogues = [
  ['missing catalogue', () => undefined],
  ['plain system prompt', () => 'No native skills catalogue.'],
  ['inline catalogue mention only', () => 'Scan <available_skills> for a matching workflow.'],
  ['multiple catalogues', (f) => catalogue(f.installationSkillPath) + catalogue(f.installationSkillPath)],
  ['unclosed catalogue', (f) => catalogue(f.installationSkillPath).replace('</available_skills>', '')],
  ['extra catalogue syntax', (f) => catalogue(f.installationSkillPath).replace('<available_skills>', '<available_skills x="y">')],
  ['unknown XML field', (f) => catalogue(f.installationSkillPath).replace('  </skill>', '    <other>x</other>\n  </skill>')],
  ['unknown XML entity', (f) => catalogue(f.installationSkillPath).replace('A &amp;', 'A &bogus;')],
  ['invalid numeric XML entity', (f) => catalogue(f.installationSkillPath).replace('A &amp;', 'A &#0;')],
  ['malformed XML entity', (f) => catalogue(f.installationSkillPath).replace('A &amp;', 'A &amp')],
  ['raw XML in description', (f) => catalogue(f.installationSkillPath).replace('A &amp;', '<injected/>')],
  ['raw XML control character', (f) => catalogue(f.installationSkillPath).replace('A &amp;', '\x01')],
  ['duplicate skill entry', (f) => { const one = catalogue(f.installationSkillPath); return one.replace('</available_skills>', one.slice(one.indexOf('  <skill>'), one.indexOf('</available_skills>')) + '</available_skills>'); }],
  ['oversized system prompt', (f) => 'x'.repeat(2 * 1024 * 1024) + catalogue(f.installationSkillPath)],
  ['oversized catalogue', (f) => catalogue(f.installationSkillPath).replace('A &amp; B workflow', 'x'.repeat(262144))],
];
for (const [label, build] of invalidCatalogues) test(`${label} -> zero reads`, (t) => {
  const f = fixture(t), tool = f.create(), params = { skillPath: f.installationSkillPath };
  f.run(build(f)); assert.equal(f.before(params)?.block, true); denied(f, tool, params);
});

test('forged user/history/other fields cannot supply the catalogue', (t) => {
  const f = fixture(t), tool = f.create(), params = { skillPath: f.installationSkillPath }, forged = catalogue(params.skillPath);
  f.run(undefined, {}, { prompt: forged, messages: [{ role: 'user', content: forged }],
    historyMessages: [{ role: 'system', content: forged }], catalogue: forged });
  assert.equal(f.before(params)?.block, true); denied(f, tool, params);
});

test('foreign catalogue paths never reach filesystem resolution or reader', (t) => {
  const f = fixture(t), tool = f.create(), foreign = '/not-registered-external/catalogue/SKILL.md';
  f.touched.length = 0; f.run(catalogue(foreign));
  const params = { skillPath: foreign }; assert.equal(f.before(params)?.block, true); denied(f, tool, params);
  assert.equal(f.touched.some((file) => file.startsWith('/not-registered-external')), false);
});

test('registered candidate without visible native entry cannot be read by path or ID', (t) => {
  const f = fixture(t), tool = f.create(); f.run('<available_skills>\n</available_skills>');
  const params = { skillPath: f.installationSkillPath }; f.before(params); denied(f, tool, params);
  denied(f, tool, { installationId: f.descriptor.installationId }, 'call-id');
});

for (const [label, overrides, eventOverrides] of [
  ['cross run', { runId: 'run-other' }, { runId: 'run-other' }],
  ['cross physical session', { sessionId: 'physical-other' }, {}],
  ['cross logical session', { sessionKey: 'logical-other' }, {}],
  ['cross agent', { agentId: 'agent-other' }, {}],
  ['missing run identity', { runId: undefined }, {}],
  ['missing physical identity', { sessionId: undefined }, {}],
  ['missing call identity', { toolCallId: undefined }, {}],
  ['event/context run mismatch', {}, { runId: 'run-other' }],
  ['event/context call mismatch', {}, { toolCallId: 'call-other' }],
]) test(`${label} -> zero reads`, (t) => {
  const f = fixture(t), tool = f.create(), params = { skillPath: f.installationSkillPath };
  f.run(catalogue(params.skillPath)); assert.equal(f.before(params, 'call-a', overrides, eventOverrides)?.block, true);
  denied(f, tool, params);
});

test('execute without before_tool_call cannot borrow a run catalogue', (t) => {
  const f = fixture(t), tool = f.create(), params = { skillPath: f.installationSkillPath };
  f.run(catalogue(params.skillPath)); denied(f, tool, params);
});

test('new run replaces earlier call admission and failed catalogue clears it', (t) => {
  const f = fixture(t), tool = f.create(), params = { skillPath: f.installationSkillPath };
  f.run(catalogue(params.skillPath)); f.before(params); f.run('No catalogue.', { runId: 'run-new' });
  denied(f, tool, params);
});

for (const phase of ['before catalogue', 'after binding']) test(`disabled ${phase} -> zero reads`, (t) => {
  const f = fixture(t), tool = f.create(), params = { skillPath: f.installationSkillPath };
  if (phase === 'before catalogue') f.config.skills.entries['model-only'] = { enabled: false };
  f.run(catalogue(params.skillPath)); f.before(params);
  f.config.skills.entries['model-only'] = { enabled: false };
  denied(f, tool, params);
});

test('native plugin disable after binding -> zero reads', (t) => {
  const f = fixture(t), tool = f.create(), params = { skillPath: f.installationSkillPath };
  f.run(catalogue(params.skillPath)); f.before(params); f.config.plugins.enabled = false;
  denied(f, tool, params);
});

for (const change of ['source update', 'adaptation update', 'uninstall', 'registration identity replacement', 'installation retarget']) {
  test(`${change} after binding cannot reuse admission`, (t) => {
    const f = fixture(t), tool = f.create(), params = { skillPath: f.installationSkillPath };
    f.run(catalogue(params.skillPath)); f.before(params);
    if (change === 'source update') fs.writeFileSync(path.join(f.sourceRoot, 'SKILL.md'), 'UPDATED_BODY_DIFFERENT_LENGTH');
    if (change === 'adaptation update') fs.writeFileSync(path.join(f.sourceRoot, 'local-data', 'ADAPTATION.md'), 'UPDATED_ADP_DIFFERENT_LENGTH');
    if (change === 'uninstall') fs.unlinkSync(f.registrationFile);
    if (change === 'registration identity replacement') {
      fs.writeFileSync(f.registrationFile, JSON.stringify({ schemaVersion: 1, manager: 'pleat-native-openclaw-v1',
        skills: [{ ...f.descriptor, sourceIdentity: '0:0' }] }));
    }
    if (change === 'installation retarget') {
      fs.unlinkSync(path.dirname(f.installationSkillPath)); fs.symlinkSync(f.workspace, path.dirname(f.installationSkillPath));
    }
    denied(f, tool, params);
  });
}

test('source update before binding is rejected', (t) => {
  const f = fixture(t), tool = f.create(), params = { skillPath: f.installationSkillPath };
  f.run(catalogue(params.skillPath)); fs.writeFileSync(path.join(f.sourceRoot, 'SKILL.md'), 'UPDATED_BODY');
  assert.equal(f.before(params)?.block, true); denied(f, tool, params);
});

test('native guard rejection -> zero reads', (t) => {
  const f = fixture(t), tool = f.create(), params = { skillPath: f.installationSkillPath };
  f.run(catalogue(params.skillPath)); f.before(params); f.guardDenied = true;
  denied(f, tool, params);
  f.guardDenied = false;
  denied(f, tool, params);
});

test('duplicate before_tool_call revokes its existing call admission', (t) => {
  const f = fixture(t), tool = f.create(), params = { skillPath: f.installationSkillPath };
  f.run(catalogue(params.skillPath)); assert.equal(f.before(params), undefined);
  assert.equal(f.before(params)?.block, true); denied(f, tool, params);
});

test('cross-run reuse of an already-bound call is revoked', (t) => {
  const f = fixture(t), tool = f.create(), params = { skillPath: f.installationSkillPath };
  f.run(catalogue(params.skillPath)); f.before(params);
  assert.equal(f.before(params, 'call-a', { runId: 'run-other' }, { runId: 'run-other' })?.block, true);
  denied(f, tool, params);
});

test('changed execute path consumes its token without reading either path', (t) => {
  const f = fixture(t), tool = f.create(), params = { skillPath: f.installationSkillPath };
  f.run(catalogue(params.skillPath)); f.before(params);
  denied(f, tool, { skillPath: path.join(f.sourceRoot, 'SKILL.md') });
  denied(f, tool, params);
});

test('new run cannot execute the preceding run call even with the same catalogue', (t) => {
  const f = fixture(t), tool = f.create(), params = { skillPath: f.installationSkillPath };
  f.run(catalogue(params.skillPath)); f.before(params);
  f.run(catalogue(params.skillPath), { runId: 'run-next' }); denied(f, tool, params);
});

test('late agent_end for an old run does not remove current-run admission', (t) => {
  const f = fixture(t), tool = f.create(), params = { skillPath: f.installationSkillPath };
  f.runId = 'run-next'; f.run(catalogue(params.skillPath)); f.before(params);
  f.close('agent_end', { runId: 'run-old' }); success(tool.execute('call-a', params));
});

test('factory requires the native guard, matching workspace, and unsandboxed scope', (t) => {
  const f = fixture(t);
  assert.equal(f.create({ assertInvocationCurrent: undefined }), null);
  assert.equal(f.create({ sandboxed: true }), null);
  assert.equal(f.create({ workspaceDir: f.sourceRoot }), null);
  assert.equal(f.reads, 0);
});

test('no registration means no generic shell', (t) => {
  const f = fixture(t); fs.unlinkSync(f.registrationFile);
  assert.equal(f.create(), null); assert.equal(f.reads, 0);
});

test('reader-time uninstall rejects output and the consumed token cannot retry', (t) => {
  const f = fixture(t), tool = f.create(), params = { skillPath: f.installationSkillPath };
  f.run(catalogue(params.skillPath)); f.before(params); f.onRead = () => fs.unlinkSync(f.registrationFile);
  assert.equal(tool.execute('call-a', params).isError, true); assert.equal(f.reads, 1);
  assert.equal(tool.execute('call-a', params).isError, true); assert.equal(f.reads, 1);
});

for (const boundary of ['before_tool_call', 'execute']) test(`cancellation at ${boundary} -> zero reads`, (t) => {
  const f = fixture(t), tool = f.create(), params = { skillPath: f.installationSkillPath }, abort = new AbortController();
  f.run(catalogue(params.skillPath));
  if (boundary === 'before_tool_call') abort.abort();
  f.before(params, 'call-a', { abortSignal: abort.signal }); abort.abort();
  denied(f, tool, params, 'call-a', abort.signal);
});

test('cancellation during trusted reader rejects output and cannot retry', (t) => {
  const f = fixture(t), tool = f.create(), params = { skillPath: f.installationSkillPath }, abort = new AbortController();
  f.run(catalogue(params.skillPath)); f.before(params, 'call-a', { abortSignal: abort.signal });
  f.onRead = () => abort.abort();
  assert.equal(tool.execute('call-a', params, abort.signal).isError, true); assert.equal(f.reads, 1);
  assert.equal(tool.execute('call-a', params).isError, true); assert.equal(f.reads, 1);
});

for (const hook of ['agent_end', 'before_reset', 'before_compaction', 'after_compaction', 'session_end']) {
  test(`${hook} clears path/call admission`, (t) => {
    const f = fixture(t), tool = f.create(), params = { skillPath: f.installationSkillPath };
    f.run(catalogue(params.skillPath)); f.before(params); f.close(hook); denied(f, tool, params);
  });
}

test('session_start replaces physical session admission', (t) => {
  const f = fixture(t), tool = f.create(), params = { skillPath: f.installationSkillPath };
  f.run(catalogue(params.skillPath)); f.before(params);
  f.hooks.get('session_start')({ sessionId: 'physical-new' }, { ...f.scope, sessionId: 'physical-new' });
  denied(f, tool, params);
});

test('dispose clears admission', (t) => {
  const f = fixture(t), tool = f.create(), params = { skillPath: f.installationSkillPath };
  f.run(catalogue(params.skillPath)); f.before(params); f.dispose(); denied(f, tool, params);
});

test('normal installation ID retains its commands API and reader route', (t) => {
  const f = fixture(t, { commands: true }), tool = f.create();
  assert.ok(tool.description.includes(f.descriptor.installationId));
  success(tool.execute('legacy-id-call', { installationId: f.descriptor.installationId }));
  assert.equal(f.reads, 1); assert.ok(f.commandCalls >= 3);
});

test('manual selection retains commands eligibility and next prompt BODY + ADP', (t) => {
  const f = fixture(t, { commands: true });
  const chosen = f.command.handler({ ...f.scope, isAuthorizedSender: true, args: f.descriptor.installationId });
  assert.match(chosen.text, /已选择/); assert.equal(f.reads, 0);
  const result = f.hooks.get('before_prompt_build')({}, { ...f.scope, runId: f.runId });
  assert.match(result.appendContext, /MODEL_ONLY_BODY/); assert.match(result.appendContext, /LOCAL_ADP/);
  assert.equal(f.reads, 1);
});

test('model-only success is not promoted to a manual next-run selection', (t) => {
  const f = fixture(t), tool = f.create(), params = { skillPath: f.installationSkillPath };
  f.run(catalogue(params.skillPath)); f.before(params); const result = tool.execute('call-a', params); success(result);
  f.hooks.get('after_tool_call')({ toolName: 'pleat_read_skill', toolCallId: 'call-a', runId: f.runId, params, result },
    { ...f.scope, runId: f.runId, toolName: 'pleat_read_skill', toolCallId: 'call-a' });
  f.close('agent_end');
  assert.equal(f.hooks.get('before_prompt_build')({}, { ...f.scope, runId: 'run-next' }), undefined);
  assert.equal(f.reads, 1);
});
