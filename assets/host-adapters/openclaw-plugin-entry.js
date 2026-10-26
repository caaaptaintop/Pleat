// Native profile entry: only bounded Pleat identity records are read here.
import fs from 'node:fs';
import path from 'node:path';
import os from 'node:os';
import { fileURLToPath } from 'node:url';
import { resolveAgentWorkspaceDir } from 'openclaw/plugin-sdk/agent-runtime';
import { listSkillCommandsForWorkspace } from 'openclaw/plugin-sdk/skill-commands-runtime';
import registerAdaptation from './adapter.js';

const root = fs.realpathSync(path.dirname(fileURLToPath(import.meta.url)));
const stamp = (s) => `${s.dev}:${s.ino}:${s.size}:${s.mtimeNs}:${s.ctimeNs}`;
const stat = (p) => fs.lstatSync(p, { bigint: true });
const nofollow = fs.constants.O_NOFOLLOW;
function plainDirectory(p) {
  const s = stat(p);
  if (!s.isDirectory() || s.isSymbolicLink()) throw new Error('Managed registration unavailable');
  return stamp(s);
}
function metadata(p, budget = { remaining: 16384 }) {
  if (nofollow === undefined) throw new Error('Managed registration unavailable');
  const fd = fs.openSync(p, fs.constants.O_RDONLY | fs.constants.O_NONBLOCK | nofollow);
  try {
    const before = fs.fstatSync(fd, { bigint: true });
    if (!before.isFile() || before.nlink !== 1n || before.size > 16384n ||
        before.size > BigInt(budget.remaining)) throw new Error('Managed registration unavailable');
    const bytes = Buffer.alloc(Number(before.size) + 1);
    const length = fs.readSync(fd, bytes, 0, bytes.length, 0);
    if (length !== Number(before.size) || stamp(before) !== stamp(fs.fstatSync(fd, { bigint: true })) ||
        stamp(before) !== stamp(stat(p))) throw new Error('Managed registration unavailable');
    budget.remaining -= length;
    return JSON.parse(new TextDecoder('utf-8', { fatal: true }).decode(bytes.subarray(0, length)));
  } finally { fs.closeSync(fd); }
}
function records() {
  const rootBefore = plainDirectory(root), directory = path.join(root, 'registrations');
  const before = plainDirectory(directory), names = [];
  const dir = fs.opendirSync(directory);
  try {
    let entry;
    while ((entry = dir.readSync()) !== null) {
      if (names.length >= 4096) throw new Error('Managed registration unavailable');
      names.push(entry.name);
    }
  } finally { dir.closeSync(); }
  const skills = [], budget = { remaining: 8 * 1024 * 1024 }; let total = 0;
  for (const name of names.sort()) {
    if (!/^[a-f0-9]{24}\.json$/u.test(name)) continue;
    const value = metadata(path.join(directory, name), budget);
    if (value?.schemaVersion !== 1 || value.manager !== 'pleat-native-openclaw-v1' ||
        !Array.isArray(value.skills) || value.skills.length !== 1) throw new Error('Managed registration unavailable');
    total += Buffer.byteLength(JSON.stringify(value));
    if (total > 1024 * 1024) throw new Error('Managed registration unavailable');
    skills.push(value.skills[0]);
  }
  if (rootBefore !== plainDirectory(root) || before !== plainDirectory(directory)) throw new Error('Managed registration unavailable');
  return skills;
}

export default function register(api) {
  try {
    const reader = metadata(path.join(root, 'reader.json'));
    if (reader?.schemaVersion !== 1 || reader.manager !== 'pleat-native-openclaw-reader-v1') return;
    const loadCandidates = (ctx) => {
      try {
        if (!ctx?.agentId || !api.config || api.config.plugins?.enabled === false) return { schemaVersion: 1, skills: [] };
        const workspaceDir = fs.realpathSync(resolveAgentWorkspaceDir(api.config, ctx.agentId));
        if (ctx.workspaceDir !== undefined && fs.realpathSync(ctx.workspaceDir) !== workspaceDir) return { schemaVersion: 1, skills: [] };
        const skills = [];
        for (const entry of records()) {
          if (!entry || !['global', 'workspace'].includes(entry.scope)) continue;
          if (entry.scope === 'workspace' && (typeof entry.workspaceRoot !== 'string' ||
              fs.realpathSync(entry.workspaceRoot) !== workspaceDir)) continue;
          skills.push({ installationId: entry.installationId, agentId: ctx.agentId, workspaceDir,
            installationSkillPath: entry.installationSkillPath, canonicalSourceRoot: entry.canonicalSourceRoot,
            sourceIdentity: entry.sourceIdentity, automaticAllowed: false });
        }
        return { schemaVersion: 1, skills };
      } catch { return { schemaVersion: 1, skills: [] }; }
    };
    registerAdaptation(api, { readerExecutable: reader.readerExecutable, loadCandidates, loadManifest(ctx, catalogue) {
      try {
        const candidates = loadCandidates(ctx);
        if (!candidates.skills.length) return candidates;
        const workspaceDir = candidates.skills[0].workspaceDir;
        if (catalogue !== undefined) {
          if (!Array.isArray(catalogue) || catalogue.length > 4096) return { schemaVersion: 1, skills: [] };
          // Only compare strings with registered paths. Never resolve a path supplied
          // by the model catalogue (which may also contain remote/external locators).
          const visible = new Set();
          for (const item of catalogue) {
            if (typeof item?.name !== 'string' || typeof item.location !== 'string' ||
                item.location.length > 16384 || /[\x00-\x1f\x7f]/u.test(item.location)) continue;
            if (api.config.skills?.entries?.[item.name]?.enabled === false) continue;
            const location = item.location.startsWith('~/') ? path.join(os.homedir(), item.location.slice(2)) : item.location;
            if (path.isAbsolute(location) && path.resolve(location) === location) visible.add(location);
          }
          return { schemaVersion: 1, skills: candidates.skills.filter((entry) =>
            visible.has(entry.installationSkillPath) || visible.has(path.join(entry.canonicalSourceRoot, 'SKILL.md')))
            .map((entry) => ({ ...entry, automaticAllowed: true })) };
        }
        // Ordinary/manual IDs retain the public native command eligibility route.
        const visible = new Map(listSkillCommandsForWorkspace({ workspaceDir, cfg: api.config, agentId: ctx.agentId })
          .filter((command) => typeof command.skillFile === 'string')
          .map((command) => [fs.realpathSync(command.skillFile), command]));
        return { schemaVersion: 1, skills: candidates.skills.flatMap((entry) => {
          const command = visible.get(fs.realpathSync(entry.installationSkillPath));
          return command ? [{ ...entry, automaticAllowed: command.modelVisible === true }] : [];
        }) };
      } catch { return { schemaVersion: 1, skills: [] }; }
    } });
  } catch { /* No path-byte or source-body fallback. */ }
}
