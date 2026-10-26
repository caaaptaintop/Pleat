// Pi 1.3.0 extension. No SDK import; only the ExtensionAPI passed by Pi is used.
import * as fs from 'node:fs';
import * as path from 'node:path';
import { createHash } from 'node:crypto';
import { homedir } from 'node:os';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { execFileSync } from 'node:child_process';

const MANIFEST_LIMIT = 1024 * 1024;
const MAX_MANIFESTS = 8;
const MAX_ENTRIES = 4096;
const MAX_CONTRIBUTIONS = 4096;
const STATIC_BYTES_LIMIT = 8 * MANIFEST_LIMIT;
const ADAPTATION_LIMIT = 64 * 1024;
const CLAIM_CHANNEL = 'pleat:pi-adaptation:v1:claim';
const identity = (s) => `${s.dev}:${s.ino}`;
const revision = (s) => `${identity(s)}:${s.size}:${s.mtimeNs}:${s.ctimeNs}`;
const stat = (p) => fs.lstatSync(p, { bigint: true });
const canonicalAbsolute = (p) => typeof p === 'string' && !/[\x00-\x1f\x7f]/.test(p) &&
  path.isAbsolute(p) && path.resolve(p) === p;

function requireValue(ok) {
  // Do not expose file contents, paths, prompts or underlying filesystem errors.
  if (!ok) throw new Error('Invalid managed adaptation resource');
}

function directoriesWithoutLinks(dir) {
  for (let current = dir; ; current = path.dirname(current)) {
    requireValue(stat(current).isDirectory()); // lstat rejects symlink parents, too.
    if (path.dirname(current) === current) break;
  }
}

function readBounded(file, limit) {
  requireValue(Number.isInteger(fs.constants.O_NOFOLLOW) && fs.constants.O_NOFOLLOW !== 0);
  const before = stat(file);
  requireValue(before.isFile() && before.nlink === 1n && before.size <= BigInt(limit));
  const fd = fs.openSync(file, fs.constants.O_RDONLY | fs.constants.O_NOFOLLOW | fs.constants.O_NONBLOCK);
  try {
    const opened = fs.fstatSync(fd, { bigint: true });
    requireValue(opened.isFile() && revision(opened) === revision(before));
    const bytes = Buffer.alloc(limit + 1);
    let length = 0;
    while (length < bytes.length) {
      const count = fs.readSync(fd, bytes, length, bytes.length - length, length);
      if (!count) break;
      length += count;
    }
    requireValue(length <= limit && revision(fs.fstatSync(fd, { bigint: true })) === revision(opened));
    requireValue(revision(stat(file)) === revision(opened));
    const text = new TextDecoder('utf-8', { fatal: true }).decode(bytes.subarray(0, length));
    requireValue(!text.includes('\0'));
    return { text, revision: revision(opened) };
  } finally {
    fs.closeSync(fd);
  }
}

function loadManifest(file) {
  requireValue(canonicalAbsolute(file));
  directoriesWithoutLinks(path.dirname(file));
  const value = JSON.parse(readBounded(file, MANIFEST_LIMIT).text);
  return validateManifest(value);
}

function validateManifest(value) {
  requireValue(value?.schemaVersion === 1 && Array.isArray(value.skills) && value.skills.length <= MAX_ENTRIES);
  for (const entry of value.skills) {
    requireValue(entry && ['nativeName', 'installationSkillPath', 'canonicalSourceRoot', 'sourceIdentity']
      .every((key) => typeof entry[key] === 'string' && entry[key].length <= MANIFEST_LIMIT));
    // Installation aliases are metadata, never command/source authorization.
    requireValue(entry.nativeName.length > 0 && entry.nativeName.length <= 1024 &&
      !/[\x00-\x1f\x7f-\x9f]/u.test(entry.nativeName) && canonicalAbsolute(entry.installationSkillPath) &&
      path.basename(entry.installationSkillPath) === 'SKILL.md' && canonicalAbsolute(entry.canonicalSourceRoot) &&
      typeof entry.sourceIdentity === 'string' && /^\d+:\d+$/.test(entry.sourceIdentity));
  }
  // Malformed data invalidates only this scope. Duplicate/conflicting paths merge below.
  return value.skills;
}

function prepareContribution(config) {
  // Explicit inline data wins, even when invalid; never fall back from it to a different scope.
  if (config.manifest !== undefined) {
    const skills = validateManifest(config.manifest).map((entry) => ({
      nativeName: entry.nativeName, installationSkillPath: entry.installationSkillPath,
      canonicalSourceRoot: entry.canonicalSourceRoot, sourceIdentity: entry.sourceIdentity,
    }));
    requireValue(skills.reduce((bytes, entry) => bytes + Object.values(entry)
      .reduce((n, value) => n + Buffer.byteLength(value), 0), 0) <= MANIFEST_LIMIT);
    const text = JSON.stringify({ schemaVersion: 1, skills });
    const bytes = Buffer.byteLength(text);
    requireValue(bytes <= MANIFEST_LIMIT);
    return { kind: 'static', key: `static:${createHash('sha256').update(text).digest('hex')}`, skills, bytes };
  }
  const file = config.manifestPath ?? process.env.PLEAT_PI_ADAPTATION_MANIFEST;
  requireValue(canonicalAbsolute(file));
  return { kind: 'file', key: `file:${file}`, file };
}

function loadScopeManifests(contributions) {
  const merged = new Map();
  let overflow = false;
  // At most 8 * (1 MiB + one oversize sentinel byte) read per invocation, including failures.
  for (const contribution of contributions.values()) {
    let entries;
    try { entries = contribution.kind === 'static' ? contribution.skills : loadManifest(contribution.file); }
    catch { continue; }
    for (const entry of entries) {
      const key = entry.installationSkillPath;
      if (merged.has(key)) {
        // null is a sticky conflict for this fresh merge, never first/last-writer wins.
        if (!sameEntry(merged.get(key), entry)) merged.set(key, null);
      } else if (merged.size < MAX_ENTRIES) {
        merged.set(key, entry);
      } else {
        overflow = true;
      }
      // Overflow cannot evict earlier entries. Still inspect later records for conflicts
      // with admitted paths, so reaching the cap cannot hide a conflicting descriptor.
    }
  }
  // Keep conflict tombstones so alias fallback cannot bypass a denied winner path.
  return Object.assign([...merged].map(([installationSkillPath, entry]) =>
    entry ?? { installationSkillPath, conflicted: true }), { overflow });
}

function inspectDirectoryChain(logical, expectedRoot, expectedIdentity) {
  // Resolve components in filesystem order, including links in installation ancestors.
  // '..' in a link target is applied AFTER resolving preceding links, as the OS does.
  let current = path.parse(logical).root;
  let remaining = logical.slice(current.length).split(path.sep);
  let links = 0;
  let steps = 0;
  const observed = [];
  while (remaining.length) {
    requireValue(++steps <= 4096);
    const component = remaining.shift();
    if (!component || component === '.') continue;
    if (component === '..') { current = path.dirname(current); continue; }
    const next = path.join(current, component);
    const info = stat(next);
    if (info.isSymbolicLink()) {
      requireValue(++links <= 40);
      const target = fs.readlinkSync(next);
      observed.push({ file: next, revision: revision(info), target });
      const targetRoot = path.parse(target).root;
      if (targetRoot) current = targetRoot;
      remaining = [...target.slice(targetRoot.length).split(path.sep), ...remaining];
    } else {
      requireValue(info.isDirectory());
      observed.push({ file: next, identity: identity(info) });
      current = next;
    }
  }
  requireValue(current === expectedRoot && identity(stat(current)) === expectedIdentity &&
    fs.realpathSync(logical) === expectedRoot);
  // Bind this observation to the actual chain, not a guessed single readlink target.
  for (const item of observed) {
    const now = stat(item.file);
    requireValue(item.target === undefined
      ? now.isDirectory() && identity(now) === item.identity
      : now.isSymbolicLink() && revision(now) === item.revision && fs.readlinkSync(item.file) === item.target);
  }
  return JSON.stringify(observed);
}

function inspectSkill(entry) {
  // The originally canonical locator can acquire a compat ancestor after MoveLibrary.
  // Authorize the current endpoint by the unchanged root identity, never by path alone.
  const root = fs.realpathSync(entry.canonicalSourceRoot);
  directoriesWithoutLinks(root);
  requireValue(fs.realpathSync(root) === root && identity(stat(root)) === entry.sourceIdentity);
  const sourceChain = inspectDirectoryChain(entry.canonicalSourceRoot, root, entry.sourceIdentity);
  const installationRoot = path.dirname(entry.installationSkillPath);
  const chain = inspectDirectoryChain(installationRoot, root, entry.sourceIdentity);
  const skillPath = path.join(root, 'SKILL.md');
  const skill = stat(skillPath);
  requireValue(skill.isFile() && skill.nlink === 1n && fs.realpathSync(entry.installationSkillPath) === skillPath);
  // A native dedup winner can be another alias. It does not replace the original
  // authorized installation: BOTH chains must still exist and reach this source.
  const winner = entry.nativeSkillPath ?? entry.installationSkillPath;
  requireValue(canonicalAbsolute(winner) && path.basename(winner) === 'SKILL.md');
  const winnerChain = winner === entry.installationSkillPath ? chain
    : inspectDirectoryChain(path.dirname(winner), root, entry.sourceIdentity);
  requireValue(fs.realpathSync(winner) === skillPath);
  return { root, revision: JSON.stringify([root, revision(skill), sourceChain, chain, winnerChain]) };
}

function adaptationReader(config) {
  // Trusted synchronous test seam only. Product wrappers supply readerExecutable.
  if (config.readAdaptation !== undefined) {
    requireValue(typeof config.readAdaptation === 'function');
    return config.readAdaptation;
  }
  const executable = config.readerExecutable;
  requireValue(canonicalAbsolute(executable));
  return (request) => {
    const output = execFileSync(executable, ['--pleat-native-read-adaptation-v1'], {
      input: JSON.stringify(request), env: { PATH: '/usr/bin:/bin', LANG: 'C.UTF-8' },
      shell: false, stdio: ['pipe', 'pipe', 'ignore'], maxBuffer: 256 * 1024, timeout: 3000,
    });
    requireValue(output.byteLength <= 256 * 1024);
    return JSON.parse(new TextDecoder('utf-8', { fatal: true }).decode(output));
  };
}

function adaptation(entry, nativeName, readAdaptation) {
  const skillRevision = inspectSkill(entry);
  const localDir = path.join(skillRevision.root, 'local-data');
  const before = stat(localDir);
  requireValue(before.isDirectory());
  const request = Object.freeze({ schemaVersion: 1, sourceRoot: skillRevision.root,
    sourceIdentity: entry.sourceIdentity, localDataIdentity: identity(before) });
  requireValue(Buffer.byteLength(JSON.stringify(request)) <= 16 * 1024);
  // Only the native FD-relative reader may obtain ADAP bytes in production.
  // Path snapshots below detect lifecycle changes; they are not the read anchor.
  const response = readAdaptation(request);
  requireValue(response?.schemaVersion === 1 && response.sourceRoot === request.sourceRoot &&
    response.sourceIdentity === request.sourceIdentity && response.localDataIdentity === request.localDataIdentity &&
    typeof response.text === 'string' && response.text.length <= ADAPTATION_LIMIT &&
    typeof response.digest === 'string' && /^[a-f0-9]{64}$/.test(response.digest));
  const text = response.text;
  const bytes = Buffer.from(text, 'utf8');
  requireValue(bytes.length <= ADAPTATION_LIMIT && !text.includes('\0') &&
    new TextDecoder('utf-8', { fatal: true }).decode(bytes) === text);
  const digest = createHash('sha256').update(bytes).digest('hex');
  requireValue(response.digest === digest);
  requireValue(identity(stat(localDir)) === identity(before) && stat(localDir).isDirectory() &&
    inspectSkill(entry).revision === skillRevision.revision);
  if (!text.trim()) return undefined;
  return {
    digest,
    text: `\n\n[PLEAT managed adaptation: ${nativeName}]\n` +
      `Skill root: ${JSON.stringify(skillRevision.root)}\nAdaptation directory: ${JSON.stringify(localDir)}\n` +
      'Resolve adaptation-relative references against the adaptation directory. Apply this local adaptation ' +
      'over upstream defaults; current user instructions, authorization and safety rules take priority.\n' +
      text + `\n[End PLEAT managed adaptation: ${nativeName}]`,
  };
}

function nativeSkill(pi, entries, name, validName, preferredPath) {
  if (validName ? !validName(name) : (typeof name !== 'string' || name.length > 64 || !/^[a-z0-9]+(?:-[a-z0-9]+)*$/.test(name))) return undefined;
  const commands = pi.getCommands().filter((c) => c.name === `skill:${name}`);
  const skills = commands.filter((c) => c.source === 'skill');
  // The native list contains the collision winner. Never substitute a different same-name source.
  if (skills.length !== 1) return undefined;
  const winner = skills[0].sourceInfo?.path;
  if (!canonicalAbsolute(winner) || path.basename(winner) !== 'SKILL.md') return undefined;
  try {
    const root = fs.realpathSync(path.dirname(winner));
    const sourceIdentity = identity(stat(root));
    const bind = (entry) => ({ ...entry, nativeSkillPath: winner });
    const exact = entries.filter((e) => e.installationSkillPath === winner);
    // Omitted paths may contain an invalid/conflicting exact descriptor. A single
    // bounded flag denies fallback without storing an unbounded list of omissions.
    if (entries.overflow && exact.length === 0) return undefined;
    if (exact.some((entry) => entry.conflicted)) return undefined;
    // An invalid exact descriptor cannot borrow authorization from an alias.
    for (const entry of exact) inspectSkill(bind(entry));
    const sources = new Map();
    for (const entry of entries) {
      if (entry.sourceIdentity !== sourceIdentity) continue;
      try {
        const bound = bind(entry);
        const inspected = inspectSkill(bound);
        if (inspected.root !== root) continue;
        const key = JSON.stringify([entry.sourceIdentity, inspected.root]);
        const group = sources.get(key) ?? [];
        group.push(bound);
        sources.set(key, group);
      } catch { /* Uninstalled or invalid contributions never authorize a new alias. */ }
    }
    if (sources.size !== 1) return undefined;
    const candidates = [...sources.values()][0];
    // Group by verified source before selecting an authorizer; scope/load order
    // and nativeName are never source authority. Bind the read alias when present.
    candidates.sort((a, b) => a.installationSkillPath < b.installationSkillPath ? -1
      : a.installationSkillPath > b.installationSkillPath ? 1 : 0);
    return candidates.find((e) => e.installationSkillPath === preferredPath)
      ?? candidates.find((e) => e.installationSkillPath === winner) ?? candidates[0];
  } catch { return undefined; }
}

function currentNativeName(pi, entry, validName) {
  const commands = pi.getCommands().filter((c) => c.source === 'skill' &&
    c.sourceInfo?.path === entry.nativeSkillPath);
  if (commands.length !== 1 || !commands[0].name.startsWith('skill:')) return undefined;
  const name = commands[0].name.slice(6);
  return sameEntry(nativeSkill(pi, [entry], name, validName), entry) ? name : undefined;
}

function sameEntry(a, b) {
  return a && b && ['nativeName', 'installationSkillPath', 'canonicalSourceRoot', 'sourceIdentity']
    .every((key) => a[key] === b[key]);
}

function builtinRead(pi) {
  const readers = pi.getAllTools().filter((t) => t.name === 'read');
  return readers.length === 1 && readers[0].sourceInfo?.source === 'builtin' &&
    readers[0].sourceInfo.path === 'builtin:read';
}

function readPath(input, cwd, normalizeSpaces = true) {
  let p = input?.path;
  if (typeof p !== 'string' || p.includes('\0')) return undefined;
  // Pi utils/paths.js normalizePath order; no SDK import or fallback filename guessing.
  if (normalizeSpaces) p = p.replace(/[\u00a0\u2000-\u200a\u202f\u205f\u3000]/gu, ' ');
  if (p.startsWith('@')) p = p.slice(1);
  // Windows shell-drive aliases are intentionally unsupported by this Unix-tested hook.
  if (process.platform === 'win32') return undefined;
  if (p === '~') p = homedir();
  else if (p.startsWith('~/')) p = path.join(homedir(), p.slice(2));
  if (p.startsWith('file://')) p = fileURLToPath(p);
  else if (p.includes('://')) return undefined;
  return path.resolve(cwd, p);
}

function matchingSkill(pi, entries, resolved, validName) {
  if (typeof resolved !== 'string') return undefined;
  const matches = [];
  for (const command of pi.getCommands().filter((c) => c.source === 'skill' && c.name.startsWith('skill:'))) {
    const entry = nativeSkill(pi, entries, command.name.slice(6), validName, resolved);
    if (!entry) continue;
    if (resolved === entry.nativeSkillPath || resolved === entry.installationSkillPath ||
        resolved === path.join(entry.canonicalSourceRoot, 'SKILL.md') ||
        resolved === path.join(inspectSkill(entry).root, 'SKILL.md')) matches.push(entry);
  }
  return matches.length === 1 ? matches[0] : undefined;
}

export default function piAdaptation(pi, config = {}, protocol = {}) {
  // Optional host protocol seam; omitted by Pi. Security/FD reads stay in this core.
  const claimChannel = protocol.claimChannel ?? CLAIM_CHANNEL;
  const sourceKey = protocol.sourceKey ?? ((entry) => entry.sourceIdentity);
  // Each managed wrapper contributes inline data or its scope's file, never prompt text.
  // config is this helper's ordinary JS argument, not a second argument supplied by Pi.
  let contribution;
  let readAdaptation;
  try {
    contribution = prepareContribution(config);
    readAdaptation = adaptationReader(config);
  } catch { return; }
  // Shared bus is scoped to this Pi resource loader, unlike process-global flags. Pi's bus invokes
  // the synchronous listener before emit returns, and removes subscriptions on runtime invalidation.
  const claim = { claimed: false, config: contribution.kind === 'static'
    ? { manifest: { schemaVersion: 1, skills: contribution.skills } } : { manifestPath: contribution.file } };
  pi.events.emit(claimChannel, claim);
  if (claim.claimed) return;
  const contributions = new Map([[contribution.key, contribution]]);
  let fileCount = contribution.kind === 'file' ? 1 : 0;
  let staticBytes = contribution.bytes ?? 0;
  pi.events.on(claimChannel, (probe) => {
    if (!probe || typeof probe !== 'object') return;
    probe.claimed = true;
    try {
      const next = prepareContribution(probe.config);
      if (contributions.has(next.key) || contributions.size >= MAX_CONTRIBUTIONS) return;
      if (next.kind === 'file' && fileCount >= MAX_MANIFESTS) return;
      if (next.kind === 'static' && staticBytes + next.bytes > STATIC_BYTES_LIMIT) return;
      contributions.set(next.key, next);
      if (next.kind === 'file') fileCount++;
      else staticBytes += next.bytes;
    } catch { /* A malformed contribution cannot disable the existing owner. */ }
  });

  const pending = new Map();
  const seen = new Map();
  const stillManaged = (entries, entry, name) => {
    const current = nativeSkill(pi, entries, name, protocol.validName, entry.installationSkillPath);
    return sameEntry(current, entry) && current.nativeSkillPath === entry.nativeSkillPath;
  };
  const clear = () => { pending.clear(); seen.clear(); };
  pi.on('session_start', clear);
  pi.on('agent_end', clear);
  pi.on('session_shutdown', clear);
  pi.on('session_tree', clear);

  pi.on('input', (event) => {
    // Queued input must not clear an in-flight read or mark its adaptation as delivered.
    if (!event.streamingBehavior) clear();
    try {
      // sendUserMessage defaults to expansion=false; input exposes no expansion flag.
      // Only standard interactive/RPC entry points are covered (see the integration protocol).
      if (event.source !== 'interactive' && event.source !== 'rpc') return;
      // Match _expandSkillCommand exactly: first ASCII space separates the name from arguments.
      // XML, bare names, arbitrary locations and commands embedded in prose are never triggers.
      if (!event.text.startsWith('/skill:')) return;
      const space = event.text.indexOf(' ');
      const name = space === -1 ? event.text.slice(7) : event.text.slice(7, space);
      const entries = loadScopeManifests(contributions);
      const entry = nativeSkill(pi, entries, name, protocol.validName);
      if (!entry || pi.getCommands().some((c) => c.name === `skill:${name}` && c.source === 'extension')) return;
      const before = inspectSkill(entry);
      // A real bounded SKILL read is required, not inference from the text of the prompt.
      readBounded(entry.nativeSkillPath, MANIFEST_LIMIT);
      requireValue(inspectSkill(entry).revision === before.revision && currentNativeName(pi, entry, protocol.validName) === name);
      const extra = adaptation(entry, name, readAdaptation);
      if (!extra || inspectSkill(entry).revision !== before.revision ||
          !stillManaged(entries, entry, name)) return;
      if (!event.streamingBehavior) seen.set(sourceKey(entry), extra.digest);
      // Preserve /skill:name as the leading command; Pi still performs its own native expansion.
      return { action: 'transform', text: `${event.text} ${extra.text}`, images: event.images };
    } catch {
      return; // No raw filesystem error, prompt or adaptation text is logged.
    }
  });

  pi.on('tool_call', (event, ctx) => {
    try {
      // A duplicate live ID is ambiguous; neither result may claim the other's observation.
      if (pending.has(event.toolCallId)) { pending.set(event.toolCallId, null); return; }
      if (event.toolName !== 'read' || typeof event.toolCallId !== 'string' || !builtinRead(pi)) return;
      const entries = loadScopeManifests(contributions);
      const literal = readPath(event.input, ctx.cwd, false);
      const normalized = readPath(event.input, ctx.cwd);
      // A literal managed filename wins over Pi's space-normalization alias.
      const literalEntry = matchingSkill(pi, entries, literal, protocol.validName);
      const entry = literalEntry ?? matchingSkill(pi, entries, normalized, protocol.validName);
      if (!entry) return;
      if (pending.size >= 2048) return;
      const observedRevision = inspectSkill(entry);
      const actualPath = literalEntry ? literal : normalized;
      if (literalEntry && literal !== normalized) {
        // Native normalization replaces Unicode spaces BEFORE decoding file URLs. Encode the
        // exact managed path so the real builtin read reaches that file, not an ASCII namesake.
        event.input.path = pathToFileURL(literal).href;
      }
      pending.set(event.toolCallId, { entry, actualPath, parentToolCallId: event.parentToolCallId,
        name: currentNativeName(pi, entry, protocol.validName), revision: observedRevision.revision });
    } catch {
      return; // Never block; exact-path rewriting happens only after successful identity checks.
    }
  });

  pi.on('tool_result', (event, ctx) => {
    const observed = pending.get(event.toolCallId);
    pending.delete(event.toolCallId);
    try {
      if (!observed || event.toolName !== 'read' || event.isError || !builtinRead(pi) ||
          event.parentToolCallId !== observed.parentToolCallId) return;
      // The native read tool can return a size notice without any SKILL text.
      if (event.details?.truncation?.firstLineExceedsLimit ||
          (event.input?.limit !== undefined && !(event.input.limit > 0)) ||
          !Array.isArray(event.content) ||
          !event.content.some((part) => part.type === 'text' && typeof part.text === 'string' && part.text.length > 0)) return;
      const entries = loadScopeManifests(contributions);
      const name = currentNativeName(pi, observed.entry, protocol.validName);
      const entry = nativeSkill(pi, entries, name, protocol.validName, observed.entry.installationSkillPath);
      const resultPath = protocol.resultPath
        ? protocol.resultPath(event, ctx) : readPath(event.input, ctx.cwd);
      requireValue(sameEntry(entry, observed.entry) && name === observed.name &&
        entry.nativeSkillPath === observed.entry.nativeSkillPath &&
        (resultPath === observed.actualPath || (protocol.resultPath &&
          resultPath === path.join(inspectSkill(entry).root, 'SKILL.md'))) &&
        inspectSkill(entry).revision === observed.revision);
      // Re-read on EVERY trigger; same-run dedup suppresses only an unchanged adaptation version.
      const extra = adaptation(entry, name, readAdaptation);
      if (!extra || inspectSkill(entry).revision !== observed.revision ||
          !stillManaged(entries, entry, name) ||
          seen.get(sourceKey(entry)) === extra.digest) return;
      const content = [...event.content, { type: 'text', text: extra.text }];
      seen.set(sourceKey(entry), extra.digest);
      // Native text reads expose the same text as structuredContent (also consumed by nested
      // tools). Append there too; retain any other structured value rather than dropping it.
      const structuredContent = typeof event.structuredContent === 'string'
        ? event.structuredContent + extra.text : event.structuredContent;
      return { content, ...(structuredContent === undefined ? {} : { structuredContent }) };
    } catch {
      return; // An adaptation failure must not overwrite a successful or failed read result.
    }
  });
}
