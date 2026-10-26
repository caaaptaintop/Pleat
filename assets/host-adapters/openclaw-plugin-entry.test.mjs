// Pure wrapper tests. The OpenClaw SDK specifiers below resolve to own stubs.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { pathToFileURL } from 'node:url';
import test from 'node:test';

async function fixture(run) {
  const root = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'pleat-openclaw-entry-mock-')));
  try {
    const sdk = path.join(root, 'node_modules/openclaw'), records = path.join(root, 'registrations');
    fs.mkdirSync(sdk, { recursive: true }); fs.mkdirSync(records);
    fs.writeFileSync(path.join(root, 'package.json'), '{"type":"module"}');
    fs.writeFileSync(path.join(sdk, 'package.json'), JSON.stringify({ type: 'module', exports: {
      './plugin-sdk/agent-runtime': './agent.js', './plugin-sdk/skill-commands-runtime': './skills.js' } }));
    fs.writeFileSync(path.join(sdk, 'agent.js'), 'export const resolveAgentWorkspaceDir = (cfg,id) => cfg.agents.entries[id].workspace;');
    fs.writeFileSync(path.join(sdk, 'skills.js'), 'export const listSkillCommandsForWorkspace = (params) => params.cfg.fixtureCommands;');
    fs.writeFileSync(path.join(root, 'adapter.js'), 'export default (api,config) => { api.bound = config; };');
    fs.copyFileSync(new URL('./openclaw-plugin-entry.js', import.meta.url), path.join(root, 'index.js'));
    fs.writeFileSync(path.join(root, 'reader.json'), JSON.stringify({ schemaVersion: 1,
      manager: 'pleat-native-openclaw-reader-v1', readerExecutable: '/owned/synthetic-reader-not-executed' }));
    const workspace = path.join(root, 'workspace'), source = path.join(root, 'source');
    fs.mkdirSync(workspace); fs.mkdirSync(source); fs.writeFileSync(path.join(source, 'SKILL.md'), 'owned mock');
    const installation = path.join(workspace, 'SKILL.md'); fs.symlinkSync(path.join(source, 'SKILL.md'), installation);
    const entry = { installationId: 'owned-id', installationSkillPath: installation,
      canonicalSourceRoot: source, sourceIdentity: '1:2', scope: 'workspace', workspaceRoot: workspace };
    const record = path.join(records, 'a'.repeat(24) + '.json');
    const write = (value = entry) => fs.writeFileSync(record, JSON.stringify({ schemaVersion: 1, manager: 'pleat-native-openclaw-v1', skills: [value] }));
    write();
    const api = { config: { agents: { entries: { one: { workspace } } },
      fixtureCommands: [{ skillFile: path.join(source, 'SKILL.md'), modelVisible: true }] } };
    const module = await import(pathToFileURL(path.join(root, 'index.js')));
    module.default(api); assert.ok(api.bound);
    await run({ root, records, record, entry, write, api, source, workspace, installation });
  } finally { fs.rmSync(root, { recursive: true, force: true }); }
}

test('metadata binds the actual agent workspace and native visible source; no body read', () => fixture(({ api, entry, workspace }) => {
  const value = api.bound.loadManifest({ agentId: 'one', workspaceDir: workspace });
  assert.deepEqual(value.skills, [{ installationId: entry.installationId, agentId: 'one', workspaceDir: workspace,
    installationSkillPath: entry.installationSkillPath, canonicalSourceRoot: entry.canonicalSourceRoot,
    sourceIdentity: entry.sourceIdentity, automaticAllowed: true }]);
  assert.equal(api.bound.loadManifest({ agentId: 'foreign', workspaceDir: workspace }).skills.length, 0);
  assert.equal(api.bound.loadManifest({ agentId: 'one', workspaceDir: '/foreign' }).skills.length, 0);
}));

test('manual native visibility is distinct from model visibility, and revoked source is absent', () => fixture(({ api, workspace, installation }) => {
  api.config.fixtureCommands[0].modelVisible = false;
  assert.equal(api.bound.loadManifest({ agentId: 'one', workspaceDir: workspace }).skills[0].automaticAllowed, false);
  api.config.fixtureCommands = [];
  assert.equal(api.bound.loadManifest({ agentId: 'one', workspaceDir: workspace }).skills.length, 0);
  fs.unlinkSync(installation);
  assert.equal(api.bound.loadManifest({ agentId: 'one', workspaceDir: workspace }).skills.length, 0);
}));

test('global scope follows actual workspace while workspace scope cannot leak to another root', () => fixture(({ api, entry, write, root, workspace }) => {
  const other = path.join(root, 'other'); fs.mkdirSync(other);
  api.config.agents.entries.two = { workspace: other };
  assert.equal(api.bound.loadManifest({ agentId: 'two', workspaceDir: other }).skills.length, 0);
  write({ ...entry, scope: 'global', workspaceRoot: null });
  assert.equal(api.bound.loadManifest({ agentId: 'two', workspaceDir: other }).skills.length, 1);
  api.config.plugins = { enabled: false };
  assert.equal(api.bound.loadManifest({ agentId: 'one', workspaceDir: workspace }).skills.length, 0);
}));

test('symlink or hardlink metadata is rejected, preserving the referenced file', () => fixture(({ api, record, root, workspace }) => {
  const target = path.join(root, 'preserved-record'); fs.renameSync(record, target);
  const original = fs.readFileSync(target);
  fs.symlinkSync(target, record);
  assert.equal(api.bound.loadManifest({ agentId: 'one', workspaceDir: workspace }).skills.length, 0);
  fs.unlinkSync(record); fs.linkSync(target, record);
  assert.equal(api.bound.loadManifest({ agentId: 'one', workspaceDir: workspace }).skills.length, 0);
  assert.deepEqual(fs.readFileSync(target), original);
}));

test('registration-directory aliases and oversized/foreign records fail closed', () => fixture(({ api, records, record, root, workspace }) => {
  fs.writeFileSync(record, ' '.repeat(16385));
  assert.equal(api.bound.loadManifest({ agentId: 'one', workspaceDir: workspace }).skills.length, 0);
  fs.writeFileSync(record, JSON.stringify({ schemaVersion: 1, manager: 'other', skills: [] }));
  assert.equal(api.bound.loadManifest({ agentId: 'one', workspaceDir: workspace }).skills.length, 0);
  const other = path.join(root, 'other-records'); fs.renameSync(records, other); fs.symlinkSync(other, records);
  assert.equal(api.bound.loadManifest({ agentId: 'one', workspaceDir: workspace }).skills.length, 0);
}));
