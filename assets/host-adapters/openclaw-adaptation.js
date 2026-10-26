// OpenClaw installed-dist contract; model catalogue hooks pinned to v2026.10.7.
import { execFileSync } from 'node:child_process';
import { createHash, randomBytes } from 'node:crypto';
import * as fs from 'node:fs';
import path from 'node:path';
import os from 'node:os';

const INPUT_LIMIT = 16384, LEGACY_OUTPUT_LIMIT = 262144, OUTPUT_LIMIT = 2 * 1024 * 1024;
const TEXT_LIMIT = 65536, SKILL_LIMIT = 262144, MANIFEST_LIMIT = 1024 * 1024;
const MAX_SESSIONS = 64, MAX_SOURCES = 16, READS_PER_PROMPT = 2, CONTEXT_LIMIT = 512 * 1024;
const COMMAND = 'pleat-adaptation';
const TOOL = 'pleat_read_skill', MAX_TOOL_IDS = 128;
const TOOL_MAPPING_ENTRY_LIMIT = 2048, TOOL_DESCRIPTION_LIMIT = 49152, TOOL_SCHEMA_LIMIT = 65536;
const RECEIPT_BUDGET = 192, END_MESSAGES_LIMIT = 4096, PROJECTION_LIMIT = 2 * 1024 * 1024;
const CATALOGUE_LIMIT = 262144, CATALOGUE_ENTRIES = 4096;
const owners = new WeakMap();
const hash = (value) => createHash('sha256').update(value).digest('hex');
const absolute = (p) => typeof p === 'string' && p.length <= INPUT_LIMIT &&
  !/[\x00-\x1f\x7f]/u.test(p) && path.isAbsolute(p) && path.resolve(p) === p;
const identity = (s) => `${s.dev}:${s.ino}`;
const validIdentity = (s) => typeof s === 'string' && s.length <= 128 && /^\d+:\d+$/u.test(s);
const validHash = (s) => typeof s === 'string' && /^[a-f0-9]{64}$/u.test(s);
const stat = (p) => fs.lstatSync(p, { bigint: true });
const revision = (s) => `${identity(s)}:${s.size}:${s.mtimeNs}:${s.ctimeNs}`;
const token = (s) => typeof s === 'string' && s.length > 0 && s.length <= 1024 && !/[\x00-\x1f\x7f]/u.test(s);
const installationId = (s) => typeof s === 'string' && /^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$/u.test(s);
function requireValue(ok) { if (!ok) throw new Error('Managed skill context unavailable'); }
function validText(text, limit) {
  return typeof text === 'string' && text.length <= limit && !text.includes('\0') &&
    Buffer.byteLength(text) <= limit && new TextDecoder('utf-8', { fatal: true }).decode(Buffer.from(text)) === text;
}
function skillPath(value) {
  if (!validText(value, INPUT_LIMIT) || /[\x00-\x1f\x7f]/u.test(value)) return undefined;
  const expanded = value.startsWith('~/') ? path.resolve(os.homedir(), value.slice(2)) : value;
  return absolute(expanded) ? expanded : undefined;
}
const entryHasPath = (entry, file) => file === entry.installationSkillPath ||
  file === path.join(entry.canonicalSourceRoot, 'SKILL.md');
function decodeCatalogueField(value) {
  requireValue(!/[<>\x00-\x08\x0b\x0c\x0e-\x1f\ufffe\uffff]/u.test(value) &&
    !/&(?!(?:amp|lt|gt|quot|apos|#[0-9]+|#x[0-9a-fA-F]+);)/u.test(value));
  const entities = { amp: '&', lt: '<', gt: '>', quot: '"', apos: "'" };
  const decoded = value.replace(/&(amp|lt|gt|quot|apos|#[0-9]+|#x[0-9a-fA-F]+);/gu, (_match, entity) => {
    if (Object.hasOwn(entities, entity)) return entities[entity];
    const code = entity.startsWith('#x') ? Number.parseInt(entity.slice(2), 16) : Number(entity.slice(1));
    requireValue(code === 9 || code === 10 || code === 13 || (code >= 32 && code <= 0xd7ff) ||
      (code >= 0xe000 && code <= 0xfffd) || (code >= 0x10000 && code <= 0x10ffff));
    return String.fromCodePoint(code);
  });
  requireValue(validText(decoded, CATALOGUE_LIMIT));
  return decoded;
}
// Accept only the fixed renderer's catalogue, never a user/history field or an
// arbitrary XML document. Consume every byte; notes and descriptions grant nothing.
function parseCatalogue(systemPrompt) {
  requireValue(validText(systemPrompt, PROJECTION_LIMIT));
  const open = '<available_skills>', close = '</available_skills>';
  // Native instructions also mention the opening tag inline; only the renderer's
  // standalone delimiter lines mark a catalogue. Multiple real catalogues fail.
  const openings = [...systemPrompt.matchAll(/^<available_skills>(?=\n)/gmu)];
  const closings = [...systemPrompt.matchAll(/^<\/available_skills>(?=\n|$)/gmu)];
  requireValue(openings.length === 1 && closings.length === 1);
  const start = openings[0].index, end = closings[0].index;
  requireValue(end > start);
  const body = systemPrompt.slice(start + open.length, end);
  requireValue(Buffer.byteLength(body) <= CATALOGUE_LIMIT && body.startsWith('\n'));
  const block = /  <skill>\n    <name>([^<>]*)<\/name>\n(?:    <description>([^<>]*)<\/description>\n)?    <location>([^<>]*)<\/location>\n(?:    <location_note>([^<>]*)<\/location_note>\n)?  <\/skill>/uy;
  const catalogue = [], names = new Set(), locations = new Set();
  let cursor = 1;
  while (cursor < body.length) {
    block.lastIndex = cursor;
    const match = block.exec(body);
    requireValue(match && catalogue.length < CATALOGUE_ENTRIES && body[block.lastIndex] === '\n');
    const name = decodeCatalogueField(match[1]), location = decodeCatalogueField(match[3]);
    if (match[2] !== undefined) decodeCatalogueField(match[2]);
    if (match[4] !== undefined) decodeCatalogueField(match[4]);
    requireValue(token(name) && validText(location, INPUT_LIMIT) && location.length > 0 &&
      !/[\x00-\x1f\x7f]/u.test(location) && !names.has(name) && !locations.has(location));
    names.add(name); locations.add(location);
    catalogue.push(Object.freeze({ name, location }));
    cursor = block.lastIndex + 1;
  }
  return Object.freeze(catalogue);
}

export const capability = Object.freeze({
  status: 'MANAGED_TOOL_AND_MANUAL_CONTEXT_CANDIDATE',
  automatic: true, automaticRoute: 'managed-installation-id-tool', stockRead: false,
  manual: true, nativeAcceptance: 'NOT_RUN_NOT_PASSED',
  automaticGap: 'Stock read ownership remains unproved; managed tool execution and next model request await controller acceptance.',
});
export const configurationContract = Object.freeze({ schemaVersion: 1, command: COMMAND, tool: TOOL,
  manifest: 'manifest or synchronous loadManifest({agentId,sessionKey,sessionId,workspaceDir?}, cleanCatalogue?); schemaVersion=1, skills[]; optional loadCandidates(scope) is not admission',
  descriptorFields: Object.freeze(['installationId', 'agentId', 'workspaceDir', 'installationSkillPath', 'canonicalSourceRoot', 'sourceIdentity', 'automaticAllowed']),
  readerExecutable: 'absolute trusted headless Rust executable', readerFlag: '--pleat-native-read-adaptation-v1',
  maxConcurrentReaders: 1, maxInputBytes: INPUT_LIMIT, maxOutputBytes: OUTPUT_LIMIT,
  maxLegacyOutputBytes: LEGACY_OUTPUT_LIMIT, maxTextBytes: TEXT_LIMIT, maxSkillBytes: SKILL_LIMIT,
  timeoutMs: 3000, maxSessions: MAX_SESSIONS, maxSourcesPerSession: MAX_SOURCES, readsPerPrompt: READS_PER_PROMPT,
  maxToolIds: MAX_TOOL_IDS, maxPendingToolResultsPerSession: MAX_SOURCES,
  maxToolMappingEntryBytes: TOOL_MAPPING_ENTRY_LIMIT, maxToolDescriptionBytes: TOOL_DESCRIPTION_LIMIT,
  maxToolSchemaBytes: TOOL_SCHEMA_LIMIT, maxEndMessages: END_MESSAGES_LIMIT, maxProjectionBytes: PROJECTION_LIMIT,
});
// Direct factory example only. The controller's Rust glue emits the actual native
// manifest/marker; its wrapper supplies reader+manifest, not api.pluginConfig.
export const pluginManifest = Object.freeze({ id: 'pleat-openclaw-adaptation', name: 'Pleat skill context',
  description: 'Managed installation-ID skill tool and explicit next-turn skill context.',
  contracts: { tools: [TOOL] },
  configSchema: { type: 'object', additionalProperties: false, required: ['readerExecutable', 'manifest'],
    properties: { readerExecutable: { type: 'string' }, manifest: { type: 'object' } } },
});

/** Trusted FD reader only; no path byte reads or fallback. Old V1 requests stay old V1. */
export function createAdaptationReader(config = {}) {
  const readerExecutable = config?.readerExecutable;
  if (!absolute(readerExecutable)) return undefined;
  let busy = false, disposed = false;
  const read = (request) => {
    if (disposed || busy) return undefined;
    busy = true;
    try {
      requireValue(request?.schemaVersion === 1 && absolute(request.sourceRoot) && validIdentity(request.sourceIdentity));
      const hasLocal = Object.hasOwn(request, 'localDataIdentity');
      const hasExpected = Object.hasOwn(request, 'expectedSkillDigest');
      const hasInclude = Object.hasOwn(request, 'includeSkillText');
      requireValue(hasLocal ? validIdentity(request.localDataIdentity) : request.includeSkillText === true);
      requireValue(!hasExpected || validHash(request.expectedSkillDigest));
      requireValue(!hasInclude || typeof request.includeSkillText === 'boolean');
      const inputRequest = { schemaVersion: 1, sourceRoot: request.sourceRoot, sourceIdentity: request.sourceIdentity,
        ...(hasLocal ? { localDataIdentity: request.localDataIdentity } : {}),
        ...(hasExpected ? { expectedSkillDigest: request.expectedSkillDigest } : {}),
        ...(hasInclude ? { includeSkillText: request.includeSkillText } : {}) };
      const input = JSON.stringify(inputRequest), skillRequested = hasExpected || hasInclude;
      requireValue(Buffer.byteLength(input) <= INPUT_LIMIT);
      const limit = skillRequested ? OUTPUT_LIMIT : LEGACY_OUTPUT_LIMIT;
      const output = execFileSync(readerExecutable, ['--pleat-native-read-adaptation-v1'], {
        input, env: { PATH: '/usr/bin:/bin', LANG: 'C.UTF-8' }, shell: false,
        stdio: ['pipe', 'pipe', 'ignore'], maxBuffer: limit, timeout: 3000,
      });
      requireValue(!disposed && Buffer.isBuffer(output) && output.byteLength <= limit);
      const response = JSON.parse(new TextDecoder('utf-8', { fatal: true }).decode(output));
      requireValue(response?.schemaVersion === 1 && response.sourceRoot === inputRequest.sourceRoot &&
        response.sourceIdentity === inputRequest.sourceIdentity &&
        Object.hasOwn(response, 'localDataIdentity') === hasLocal &&
        (!hasLocal || response.localDataIdentity === inputRequest.localDataIdentity) &&
        validText(response.text, TEXT_LIMIT) && validHash(response.digest) && hash(response.text) === response.digest);
      requireValue(hasLocal || response.text === '');
      if (skillRequested) {
        requireValue(validHash(response.skillDigest) && validIdentity(response.skillIdentity));
        requireValue(!hasExpected || response.skillDigest === inputRequest.expectedSkillDigest);
        requireValue(request.includeSkillText === true ? validText(response.skillText, SKILL_LIMIT) &&
          hash(response.skillText) === response.skillDigest : !Object.hasOwn(response, 'skillText'));
      } else requireValue(!Object.hasOwn(response, 'skillDigest') && !Object.hasOwn(response, 'skillIdentity') &&
        !Object.hasOwn(response, 'skillText'));
      return Object.freeze({ schemaVersion: 1, sourceRoot: response.sourceRoot, sourceIdentity: response.sourceIdentity,
        ...(hasLocal ? { localDataIdentity: response.localDataIdentity } : {}), text: response.text, digest: response.digest,
        ...(skillRequested ? { skillDigest: response.skillDigest, skillIdentity: response.skillIdentity } : {}),
        ...(request.includeSkillText === true ? { skillText: response.skillText } : {}) });
    } catch { return undefined; } finally { busy = false; }
  };
  return Object.freeze({ read, dispose() { disposed = true; } });
}

// Reuse Pi's component-order chain observation, not its loader, command or event logic.
function inspectChain(logical, root, expectedIdentity) {
  let current = path.parse(logical).root, remaining = logical.slice(current.length).split(path.sep), links = 0, steps = 0;
  const observed = [];
  while (remaining.length) {
    requireValue(++steps <= 4096);
    const component = remaining.shift();
    if (!component || component === '.') continue;
    if (component === '..') { current = path.dirname(current); continue; }
    const next = path.join(current, component), info = stat(next);
    if (info.isSymbolicLink()) {
      requireValue(++links <= 40);
      const target = fs.readlinkSync(next), targetRoot = path.parse(target).root;
      observed.push({ file: next, revision: revision(info), target });
      if (targetRoot) current = targetRoot;
      remaining = [...target.slice(targetRoot.length).split(path.sep), ...remaining];
    } else {
      requireValue(info.isDirectory()); observed.push({ file: next, identity: identity(info) }); current = next;
    }
  }
  requireValue(current === root && identity(stat(root)) === expectedIdentity && fs.realpathSync(logical) === root);
  for (const item of observed) {
    const now = stat(item.file);
    requireValue(item.target === undefined ? now.isDirectory() && identity(now) === item.identity
      : now.isSymbolicLink() && revision(now) === item.revision && fs.readlinkSync(item.file) === item.target);
  }
  return observed;
}
function optionalStat(file) {
  try { return stat(file); } catch (error) { if (error.code === 'ENOENT') return undefined; throw error; }
}
function inspect(entry, workspaceDir) {
  const root = fs.realpathSync(entry.canonicalSourceRoot);
  requireValue(absolute(root) && stat(root).isDirectory() && identity(stat(root)) === entry.sourceIdentity);
  const sourceChain = inspectChain(entry.canonicalSourceRoot, root, entry.sourceIdentity);
  const installChain = inspectChain(path.dirname(entry.installationSkillPath), root, entry.sourceIdentity);
  const skillPath = path.join(root, 'SKILL.md'), skill = stat(skillPath);
  requireValue(skill.isFile() && skill.nlink === 1n && skill.size <= BigInt(SKILL_LIMIT) &&
    fs.realpathSync(entry.installationSkillPath) === skillPath);
  // Command payload has no workspace field. Bind the descriptor now; prompt supplies the real field later.
  requireValue(absolute(entry.workspaceDir) && fs.realpathSync(entry.workspaceDir) === entry.workspaceDir);
  const workspace = stat(entry.workspaceDir);
  requireValue(workspace.isDirectory());
  if (workspaceDir !== undefined) requireValue(absolute(workspaceDir) && fs.realpathSync(workspaceDir) === entry.workspaceDir);
  const workspaceChain = inspectChain(workspaceDir ?? entry.workspaceDir, entry.workspaceDir, identity(workspace));
  const localDir = path.join(root, 'local-data'), local = optionalStat(localDir);
  let localDataIdentity, localState = 'absent';
  if (local) {
    requireValue(local.isDirectory()); // Never downgrade symlink/invalid local-data to body-only.
    const adap = optionalStat(path.join(localDir, 'ADAPTATION.md'));
    if (adap) {
      requireValue(adap.isFile() && adap.nlink === 1n && adap.size <= BigInt(TEXT_LIMIT));
      localDataIdentity = identity(local); localState = [localDataIdentity, revision(adap)];
    } else localState = [identity(local), 'no-adaptation'];
  }
  return { root, skillIdentity: identity(skill), localDataIdentity,
    sourceKey: JSON.stringify([root, entry.sourceIdentity]),
    revision: JSON.stringify([root, revision(skill), sourceChain, installChain, workspaceChain, localState]) };
}
function loadEntries(config, ctx, catalogue, candidates = false) {
  const scope = Object.freeze({
    agentId: ctx.agentId, sessionKey: ctx.sessionKey, sessionId: ctx.sessionId,
    ...(ctx.workspaceDir !== undefined ? { workspaceDir: ctx.workspaceDir } : {}),
  });
  const value = candidates && typeof config.loadCandidates === 'function' ? config.loadCandidates(scope) :
    typeof config.loadManifest === 'function' ? config.loadManifest(scope, catalogue) : config.manifest;
  requireValue(value?.schemaVersion === 1 && Array.isArray(value.skills) && value.skills.length <= 4096);
  const entries = new Map(); let bytes = 0;
  for (const raw of value.skills) {
    requireValue(raw && configurationContract.descriptorFields.every((field) =>
      typeof raw[field] === (field === 'automaticAllowed' ? 'boolean' : 'string')));
    requireValue(installationId(raw.installationId) && token(raw.agentId) && absolute(raw.workspaceDir) &&
      absolute(raw.installationSkillPath) && path.basename(raw.installationSkillPath) === 'SKILL.md' &&
      absolute(raw.canonicalSourceRoot) && validIdentity(raw.sourceIdentity));
    const entry = Object.fromEntries(configurationContract.descriptorFields.map((key) => [key, raw[key]]));
    bytes += Buffer.byteLength(JSON.stringify(entry)); requireValue(bytes <= MANIFEST_LIMIT);
    requireValue(!entries.has(entry.installationId)); entries.set(entry.installationId, Object.freeze(entry));
  }
  return entries;
}
const sameEntry = (a, b) => a && b && configurationContract.descriptorFields.every((key) => a[key] === b[key]);
const sessionKey = (ctx) => token(ctx?.agentId) && token(ctx.sessionKey) && token(ctx.sessionId)
  ? JSON.stringify([ctx.agentId, ctx.sessionKey, ctx.sessionId]) : undefined;
function contextText(entry, observation, result) {
  return `\n[PLEAT managed skill context: ${entry.installationId}]\nSkill root: ${JSON.stringify(observation.root)}\n` +
    result.skillText + (result.localDataIdentity === undefined ? '' :
      `\n\n[PLEAT managed adaptation]\nAdaptation directory: ${JSON.stringify(path.join(observation.root, 'local-data'))}\n` +
      'Resolve local references against the adaptation directory. Local adaptation overrides upstream defaults; ' +
      'current user instructions, authorization and safety rules take priority.\n' + result.text) +
    `\n[End PLEAT managed skill context: ${entry.installationId}]`;
}

function latestUserIndex(messages) {
  if (!Array.isArray(messages) || messages.length > END_MESSAGES_LIMIT) return -1;
  for (let i = messages.length - 1; i >= 0; i--) if (messages[i]?.role === 'user') return i;
  return -1;
}
// Fixed installed producer: attempt:3190-3202 retains the selected projection
// on the user message; builtin:1604-1609/4093-4101 passes it to agent_end.
// Read this observed hook payload slot only. Never mutate/project private state,
// infer it from ordinary transcript content, or accept an older user's receipt.
function consumedOffer(messages, userIndex, offer) {
  const user = messages?.[userIndex], projection = user?.__openclaw?.modelPromptProjection;
  if (userIndex < 0 || user.excludeFromContext === true || projection?.version !== 1 ||
      typeof projection.text !== 'string' || projection.text.length > PROJECTION_LIMIT ||
      Buffer.byteLength(projection.text) > PROJECTION_LIMIT) return false;
  const text = projection.text, index = text.indexOf(offer.receipt), end = index - 1, start = end - offer.textLength;
  return start >= 0 && text[end] === '\n' &&
    (index + offer.receipt.length === text.length || text[index + offer.receipt.length] === '\n') &&
    text.indexOf(offer.receipt, index + offer.receipt.length) === -1 && hash(text.slice(start, end)) === offer.textDigest;
}
function consumedTool(messages, userIndex, callId, pending) {
  if (userIndex < 0 || messages[userIndex].excludeFromContext === true) return false;
  for (let i = userIndex + 1; i < messages.length; i++) {
    const message = messages[i];
    if (message?.role !== 'toolResult' || message.toolName !== TOOL || message.toolCallId !== callId) continue;
    const content = message.content;
    return message.isError === false && Array.isArray(content) && content.length === 1 && content[0]?.type === 'text' &&
      validText(content[0].text, CONTEXT_LIMIT) && hash(content[0].text) === pending.textDigest;
  }
  return false;
}

/**
 * Explicit /pleat-adaptation INSTALLATION_ID -> next native before_prompt_build.
 * Selection is metadata only; live body+ADAP bytes are read in the prompt hook.
 * No stock /skill:name expansion or by-name native winner is claimed.
 */
export default function openClawAdaptation(api, config = {}) {
  if (!api || typeof api !== 'object' || !config || typeof config !== 'object') return undefined;
  if (owners.has(api)) return owners.get(api);
  const reader = createAdaptationReader(config);
  if (!reader || typeof api?.on !== 'function' || typeof api.registerCommand !== 'function' || typeof api.registerTool !== 'function' ||
      (config.loadManifest !== undefined && typeof config.loadManifest !== 'function') ||
      (config.loadCandidates !== undefined && typeof config.loadCandidates !== 'function')) return undefined;
  let disposed = false;
  const sessions = new Map();
  const dispose = () => {
    disposed = true;
    for (const state of sessions.values()) clearCatalogue(state);
    sessions.clear(); reader.dispose(); if (owners.get(api) === registration) owners.delete(api);
  };
  const registration = Object.freeze({ ...capability, command: COMMAND, tool: TOOL, dispose });
  const getSession = (ctx, create = false) => {
    const key = sessionKey(ctx); if (!key || disposed) return undefined;
    if (!sessions.has(key) && create) {
      if (sessions.size >= MAX_SESSIONS) return undefined;
      sessions.set(key, { selections: new Map(), seen: new Map(), deliveries: new Map(), offer: undefined, window: undefined, cursor: 0 });
    }
    return sessions.get(key);
  };
  const readSource = (entry, ctx, observation = inspect(entry, ctx.workspaceDir), catalogue, assertCurrent = () => {}) => {
    assertCurrent();
    const result = reader.read({ schemaVersion: 1, sourceRoot: observation.root, sourceIdentity: entry.sourceIdentity,
      ...(observation.localDataIdentity !== undefined ? { localDataIdentity: observation.localDataIdentity } : {}),
      includeSkillText: true });
    assertCurrent();
    requireValue(result && !disposed && inspect(entry, ctx.workspaceDir).revision === observation.revision &&
      result.skillIdentity === observation.skillIdentity && sameEntry(loadEntries(config, ctx, catalogue).get(entry.installationId), entry));
    requireValue(inspect(entry, ctx.workspaceDir).revision === observation.revision);
    return { text: contextText(entry, observation, result),
      pair: hash(JSON.stringify([result.skillDigest, result.digest, result.localDataIdentity ?? null])) };
  };
  const clearCatalogue = (state) => {
    if (!state) return;
    if (state.catalogueRun) {
      state.catalogueRun.active = false;
      state.catalogueRun.paths.clear(); state.catalogueRun.calls.clear();
      state.catalogueRun = undefined;
    }
    for (const [id, pending] of state.deliveries) if (pending.skillPath !== undefined) state.deliveries.delete(id);
  };
  const clearExact = (_event, ctx) => {
    const key = sessionKey(ctx);
    if (key) { clearCatalogue(sessions.get(key)); sessions.delete(key); }
  };
  const onBeforeAgentRun = (event, ctx) => {
    const state = getSession(ctx, true);
    clearCatalogue(state);
    try {
      requireValue(state && token(ctx.runId) && absolute(ctx.workspaceDir) && ctx.sandboxed !== true);
      const catalogue = parseCatalogue(event?.systemPrompt);
      const entries = [...loadEntries(config, ctx, catalogue).values()].filter((entry) =>
        entry.agentId === ctx.agentId && entry.automaticAllowed === true);
      const paths = new Map();
      for (const item of catalogue) {
        const file = skillPath(item.location);
        if (!file) continue;
        const matches = entries.filter((entry) => entryHasPath(entry, file));
        if (matches.length !== 1 || paths.has(file)) continue;
        requireValue(paths.size < MAX_TOOL_IDS);
        const entry = matches[0], observation = inspect(entry, ctx.workspaceDir);
        paths.set(file, { entry, observation });
      }
      requireValue(!disposed && getSession(ctx) === state);
      // Retain clean identities and revisions only, never the complete system prompt.
      state.catalogueRun = { runId: ctx.runId, workspaceDir: ctx.workspaceDir, catalogue, paths,
        calls: new Map(), active: true };
    } catch { clearCatalogue(state); }
  };
  const onBeforeToolCall = (event, ctx) => {
    if (event?.toolName !== TOOL || !Object.hasOwn(event.params ?? {}, 'skillPath')) return;
    const state = getSession(ctx), run = state?.catalogueRun;
    try {
      requireValue(!disposed && run?.active && token(ctx.runId) && ctx.runId === run.runId &&
        ctx.runId === event.runId && ctx.toolName === TOOL && token(ctx.toolCallId) &&
        ctx.toolCallId === event.toolCallId && !ctx.abortSignal?.aborted &&
        !run.calls.has(ctx.toolCallId) && run.calls.size < MAX_SOURCES);
      // Reserve even a rejected call ID; a second attempt cannot borrow its admission.
      run.calls.set(ctx.toolCallId, { used: true });
      const file = skillPath(event.params.skillPath), admitted = run.paths.get(file);
      requireValue(Object.keys(event.params).length === 1 && file && admitted);
      const checkCtx = { ...ctx, workspaceDir: run.workspaceDir };
      requireValue(sameEntry(loadEntries(config, checkCtx, run.catalogue).get(admitted.entry.installationId), admitted.entry) &&
        inspect(admitted.entry, run.workspaceDir).revision === admitted.observation.revision);
      run.calls.set(ctx.toolCallId, { ...admitted, skillPath: file, used: false, signal: ctx.abortSignal });
    } catch {
      const previous = run?.calls.get(ctx?.toolCallId);
      if (previous) previous.used = true;
      return { block: true, blockReason: 'Managed skill unavailable.' };
    }
  };
  const onPrompt = (_event, ctx) => {
    try {
      const state = getSession(ctx); if (!state) return;
      const previousOffer = state.offer;
      state.offer = undefined; // A failed/rejected rebuild must not commit an older offered context.
      if (!token(ctx.runId) || ctx.sandboxed === true || !absolute(ctx.workspaceDir)) return;
      ctx.hookInvocation?.assertActive();
      const entries = loadEntries(config, ctx), candidates = new Map();
      for (const [id, selected] of state.selections) {
        const entry = entries.get(id);
        if (!sameEntry(entry, selected) || entry.agentId !== ctx.agentId) { state.selections.delete(id); continue; }
        try { const observation = inspect(entry, ctx.workspaceDir);
          if (!candidates.has(observation.sourceKey)) candidates.set(observation.sourceKey, { entry, observation });
        } catch { state.selections.delete(id); }
      }
      const list = [...candidates.values()], blocks = [], pairs = new Map(); let bytes = 0;
      if (!list.length) { state.offer = undefined; return; }
      if (state.window?.runId !== ctx.runId) {
        const start = state.cursor % list.length;
        const priority = list.find(({ entry }) => entry.sourceIdentity === state.selections.get(state.priorityId)?.sourceIdentity);
        const keys = priority ? [priority.observation.sourceKey] : [];
        let visited = 0;
        while (visited < list.length && keys.length < READS_PER_PROMPT) {
          const key = list[(start + visited++) % list.length].observation.sourceKey;
          if (!keys.includes(key)) keys.push(key);
        }
        state.window = { runId: ctx.runId, keys, priorityKey: priority?.observation.sourceKey };
        state.cursor = (start + visited) % list.length;
      }
      for (const key of state.window.keys) {
        const candidate = candidates.get(key); if (!candidate) continue;
        const { entry, observation } = candidate;
        const { text: block, pair } = readSource(entry, ctx, observation);
        requireValue(sessions.get(sessionKey(ctx)) === state);
        if (state.seen.get(observation.sourceKey) === pair) continue;
        const length = Buffer.byteLength(block);
        const separator = blocks.length ? 2 : 0;
        if (bytes + separator + length + RECEIPT_BUDGET > CONTEXT_LIMIT) continue;
        blocks.push(block); bytes += separator + length; pairs.set(observation.sourceKey, pair);
      }
      if (!blocks.length) return;
      // Fallback may submit an older frozen user projection despite a new hook
      // build. Fresh nonces bind end-of-run evidence to this exact offered body.
      const text = blocks.join('\n\n'), textDigest = hash(text);
      const receipt = previousOffer?.runId === ctx.runId && previousOffer.textDigest === textDigest
        ? previousOffer.receipt : `[PLEAT context receipt v1 ${randomBytes(32).toString('hex')} ${textDigest}]`;
      ctx.hookInvocation?.assertActive();
      state.offer = { runId: ctx.runId, pairs, priorityKey: state.window.priorityKey, started: false,
        receipt, textLength: text.length, textDigest };
      return { appendContext: `${text}\n${receipt}` };
    } catch { return undefined; }
  };
  try {
    api.registerTool({ contextVersion: 2, create(ctx) {
      try {
        requireValue(!disposed && sessionKey(ctx) && absolute(ctx.workspaceDir) && ctx.sandboxed !== true &&
          typeof ctx.assertInvocationCurrent === 'function');
        ctx.assertInvocationCurrent();
        const allowed = new Map();
        for (const entry of loadEntries(config, ctx).values()) {
          if (entry.agentId !== ctx.agentId || entry.automaticAllowed !== true) continue;
          try { inspect(entry, ctx.workspaceDir); allowed.set(entry.installationId, entry); } catch { continue; }
          if (allowed.size >= MAX_TOOL_IDS) break;
        }
        const hasCandidates = allowed.size > 0 || [...loadEntries(config, ctx, undefined, true).values()].some((entry) => {
          if (entry.agentId !== ctx.agentId) return false;
          try { inspect(entry, ctx.workspaceDir); return true; } catch { return false; }
        });
        if (!hasCandidates) return null;
        const rows = [...allowed.values()].map((entry) => ({ installationId: entry.installationId,
          installationSkillPath: entry.installationSkillPath, canonicalSourceRoot: entry.canonicalSourceRoot }));
        requireValue(rows.every((row) => Buffer.byteLength(JSON.stringify(row)) <= TOOL_MAPPING_ENTRY_LIMIT));
        const description = 'Read a registered skill body and its local adaptation together. Use skillPath with the exact location listed in the current native available_skills catalogue, or use an installationId from the authorized mapping below.' +
          (rows.length ? '\nAuthorized installations:\n' + JSON.stringify(rows) : '');
        const label = 'Read managed skill';
        const parameters = { type: 'object', additionalProperties: false,
          oneOf: [...(allowed.size ? [{ required: ['installationId'] }] : []), { required: ['skillPath'] }],
          properties: { ...(allowed.size ? { installationId: { type: 'string', enum: [...allowed.keys()] } } : {}),
            skillPath: { type: 'string', minLength: 1, maxLength: INPUT_LIMIT } } };
        requireValue(Buffer.byteLength(description) <= TOOL_DESCRIPTION_LIMIT &&
          Buffer.byteLength(JSON.stringify({ name: TOOL, label, description, parameters })) <= TOOL_SCHEMA_LIMIT);
        const factoryState = getSession(ctx, true);
        requireValue(factoryState);
        return { name: TOOL, label,
          description, parameters,
          execute(toolCallId, params, signal) {
            try {
              requireValue(!disposed && token(toolCallId) && params &&
                Object.keys(params).length === 1);
              if (Object.hasOwn(params, 'skillPath')) {
                const state = getSession(ctx), run = state?.catalogueRun, binding = run?.calls.get(toolCallId);
                requireValue(state === factoryState && run?.active && binding && !binding.used);
                binding.used = true; // Single use, including attempts rejected by a later check.
                const assertCurrent = () => {
                  ctx.assertInvocationCurrent();
                  requireValue(!disposed && !signal?.aborted && !binding.signal?.aborted && run.active &&
                    getSession(ctx) === state && state.catalogueRun === run);
                };
                assertCurrent();
                requireValue(binding.skillPath === skillPath(params.skillPath) &&
                  sameEntry(loadEntries(config, ctx, run.catalogue).get(binding.entry.installationId), binding.entry) &&
                  inspect(binding.entry, ctx.workspaceDir).revision === binding.observation.revision &&
                  !state.deliveries.has(toolCallId) && state.deliveries.size < MAX_SOURCES);
                const result = readSource(binding.entry, ctx, binding.observation, run.catalogue, assertCurrent);
                assertCurrent();
                requireValue(Buffer.byteLength(result.text) <= CONTEXT_LIMIT);
                state.deliveries.set(toolCallId, { entry: binding.entry, workspaceDir: ctx.workspaceDir,
                  observation: binding.observation, pair: result.pair, textDigest: hash(result.text), started: false,
                  skillPath: binding.skillPath, catalogueRun: run });
                return { content: [{ type: 'text', text: result.text }], details: {}, isError: false };
              }
              ctx.assertInvocationCurrent();
              requireValue(!signal?.aborted && installationId(params.installationId));
              const entry = allowed.get(params.installationId), live = loadEntries(config, ctx).get(params.installationId);
              requireValue(sameEntry(entry, live) && entry.agentId === ctx.agentId);
              const state = getSession(ctx);
              requireValue(state === factoryState && !state.deliveries.has(toolCallId) && state.deliveries.size < MAX_SOURCES &&
                (state.selections.has(entry.installationId) || state.selections.size < MAX_SOURCES));
              const observation = inspect(entry, ctx.workspaceDir), result = readSource(entry, ctx, observation);
              ctx.assertInvocationCurrent();
              requireValue(!signal?.aborted && sessions.get(sessionKey(ctx)) === state && Buffer.byteLength(result.text) <= CONTEXT_LIMIT);
              // Store only hashes/metadata. Do not treat factory/schema/prompt exposure as a successful read.
              state.deliveries.set(toolCallId, { entry, workspaceDir: ctx.workspaceDir, observation,
                pair: result.pair, textDigest: hash(result.text), started: false });
              return { content: [{ type: 'text', text: result.text }], details: {}, isError: false };
            } catch { return { content: [{ type: 'text', text: 'Managed skill unavailable.' }], details: {}, isError: true }; }
          } };
      } catch { return null; }
    } }, { name: TOOL }); // Native resolver preserves an existing normalized tool name; no override API.
    api.registerCommand({ name: COMMAND, description: '列出受管技能，或为下一次模型请求选择安装 ID；list [offset] 查看后续条目', acceptsArgs: true,
      requireAuth: true, handler(ctx) {
        try {
          requireValue(!disposed && ctx.isAuthorizedSender === true && sessionKey(ctx));
          const args = ctx.args ?? '';
          requireValue(typeof args === 'string');
          const listing = /^(?:help|list(?: (0|[1-9][0-9]{0,3}))?)?$/u.exec(args);
          const entries = loadEntries(config, ctx);
          if (listing) {
            let offset = Number(listing[1] ?? 0), bytes = 0;
            const all = [...entries.values()], rows = [];
            requireValue(offset <= all.length);
            while (offset < all.length && rows.length < MAX_TOOL_IDS) {
              const entry = all[offset];
              if (entry.agentId !== ctx.agentId) { offset++; continue; }
              try { inspect(entry); } catch { offset++; continue; }
              const row = { installationId: entry.installationId,
                installationSkillPath: entry.installationSkillPath, canonicalSourceRoot: entry.canonicalSourceRoot };
              const size = Buffer.byteLength(JSON.stringify(row));
              requireValue(size <= TOOL_DESCRIPTION_LIMIT - 1024);
              if (bytes + size > TOOL_DESCRIPTION_LIMIT - 1024) break;
              rows.push(row); bytes += size; offset++;
            }
            return { text: '用法：/pleat-adaptation <installationId> 选择技能，下一次模型请求加载正文与本机适配。' +
              '\n/pleat-adaptation list [offset] 查看条目；nextOffset 非空时可继续翻页。\n' +
              JSON.stringify({ installations: rows, nextOffset: offset < all.length ? offset : null }) };
          }
          requireValue(installationId(args));
          const entry = entries.get(args);
          requireValue(entry?.agentId === ctx.agentId);
          inspect(entry); // Live original install and source; no source/ADAP bytes read yet.
          const state = getSession(ctx, true);
          requireValue(state && (state.selections.has(entry.installationId) || state.selections.size < MAX_SOURCES));
          state.selections.set(entry.installationId, entry);
          state.priorityId = entry.installationId; state.window = undefined;
          return { text: '已选择该安装 ID；下一次模型请求会重新验证并加载正文。' };
        } catch { return { text: '无法选择该安装 ID；未加载技能上下文。' }; }
      } }); // Native registrar rejects reserved names/duplicate aliases; no overriding API is used.
    api.on('before_prompt_build', onPrompt, { registrationId: 'pleat-adaptation-context-v1' });
    api.on('before_agent_run', onBeforeAgentRun);
    api.on('before_tool_call', onBeforeToolCall);
    api.on('after_tool_call', (event, ctx) => {
      const state = getSession(ctx), pending = state?.deliveries.get(event.toolCallId);
      if (!pending || event.toolName !== TOOL || ctx.toolName !== TOOL) return;
      try {
        const modelPath = pending.skillPath !== undefined;
        requireValue(token(ctx.runId) && ctx.runId === event.runId && ctx.toolCallId === event.toolCallId &&
          event.error === undefined && event.result?.isError !== true && (modelPath ?
            pending.catalogueRun === state.catalogueRun && pending.catalogueRun.active &&
              pending.catalogueRun.runId === ctx.runId && skillPath(event.params?.skillPath) === pending.skillPath :
            event.params?.installationId === pending.entry.installationId));
        const content = event.result?.content;
        requireValue(Array.isArray(content) && content.length === 1 && content[0].type === 'text' &&
          validText(content[0].text, CONTEXT_LIMIT) && hash(content[0].text) === pending.textDigest);
        requireValue(sameEntry(loadEntries(config, { ...ctx, workspaceDir: pending.workspaceDir },
          modelPath ? pending.catalogueRun.catalogue : undefined).get(pending.entry.installationId), pending.entry) &&
          inspect(pending.entry, pending.workspaceDir).revision === pending.observation.revision);
        requireValue(modelPath || state.selections.has(pending.entry.installationId) || state.selections.size < MAX_SOURCES);
        pending.runId = ctx.runId;
        if (!modelPath) state.selections.set(pending.entry.installationId, pending.entry);
      } catch { state.deliveries.delete(event.toolCallId); }
    });
    api.on('model_call_started', (_event, ctx) => {
      const state = getSession(ctx); if (state?.offer?.runId === ctx.runId) state.offer.started = true;
      for (const pending of state?.deliveries.values() ?? []) if (pending.runId === ctx.runId) pending.started = true;
    });
    api.on('agent_end', (event, ctx) => {
      const state = getSession(ctx); if (!state) return;
      const offer = state.offer;
      const userIndex = latestUserIndex(event.messages);
      const successful = event.success === true && (event.runId === undefined || event.runId === ctx.runId);
      for (const [id, pending] of state?.deliveries ?? []) {
        if (pending.catalogueRun && pending.catalogueRun.runId !== ctx.runId) continue;
        if (pending.runId !== undefined && pending.runId !== ctx.runId) continue;
        if (successful && pending.started && pending.runId === ctx.runId && consumedTool(event.messages, userIndex, id, pending) &&
            (state.seen.has(pending.observation.sourceKey) || state.seen.size < MAX_SOURCES)) {
          state.seen.set(pending.observation.sourceKey, pending.pair);
        }
        state.deliveries.delete(id);
      }
      if (state.catalogueRun?.runId === ctx.runId) clearCatalogue(state);
      if (!offer || offer.runId !== ctx.runId) return;
      if (successful && offer.started && consumedOffer(event.messages, userIndex, offer)) {
        for (const [key, pair] of offer.pairs) {
          if (state.seen.has(key) || state.seen.size < MAX_SOURCES) state.seen.set(key, pair);
        }
        if (offer.pairs.has(offer.priorityKey)) state.priorityId = undefined;
      }
      state.offer = undefined;
    });
    api.on('before_reset', clearExact);
    api.on('session_end', clearExact);
    api.on('session_start', (event, ctx) => {
      if (disposed || !token(ctx?.agentId) || !token(ctx.sessionKey) || !token(event.sessionId) ||
          event.sessionId !== ctx.sessionId) return;
      for (const key of sessions.keys()) {
        const [agent, logical, physical] = JSON.parse(key);
        if (agent === ctx.agentId && logical === ctx.sessionKey && physical !== event.sessionId) {
          clearCatalogue(sessions.get(key)); sessions.delete(key);
        }
      }
    });
    api.on('before_compaction', (_event, ctx) => { clearCatalogue(getSession(ctx)); });
    api.on('after_compaction', (_event, ctx) => {
      const state = getSession(ctx); if (state) { clearCatalogue(state); state.seen.clear(); state.deliveries.clear(); state.offer = undefined; state.window = undefined; }
    });
    api.lifecycle?.onDispose?.(dispose);
    owners.set(api, registration);
    return registration;
  } catch { dispose(); return undefined; }
}
