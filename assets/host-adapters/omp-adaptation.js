// OMP 18.12.2 protocol facade. No SDK import, source scanning or path-based ADAP reader.
import * as path from 'node:path';
import { homedir } from 'node:os';
import { fileURLToPath, pathToFileURL } from 'node:url';
import piAdaptation from './pi-adaptation.js';

const validName = (name) => typeof name === 'string' && name.length > 0 && name.length <= 1024 &&
  !/[\s\x00-\x1f\x7f-\x9f]/u.test(name);

// parseSkillInvocation / allowsSkillTokens, including the native local-execution gates.
// Names are subsequently bound to the actual runtime winner, never to manifest.nativeName.
function invocation(text) {
  if (typeof text !== 'string') return undefined;
  const start = text.trimStart();
  if (start.startsWith('/skill:')) {
    const space = start.search(/\s/u);
    return { name: space < 0 ? start.slice(7) : start.slice(7, space),
      args: space < 0 ? '' : start.slice(space + 1).trim() };
  }
  if (start.startsWith('/') || start.startsWith('!') || /^\${1,2}(?:[ \t\n\r]|$)/u.test(start)) return undefined;
  const match = /(^|\s)\/skill:([^\s/]+(?:\/[^\s/]+)?)(?=\s|$)/u.exec(text);
  if (!match) return undefined;
  return { name: match[2], args: [text.slice(0, match.index + match[1].length).trimEnd(),
    text.slice(match.index + match[0].length).trimStart()].filter(Boolean).join(' ').trim() };
}

function localPath(raw, cwd) {
  if (typeof raw !== 'string' || /[\x00-\x1f\x7f]/u.test(raw) || process.platform === 'win32') return undefined;
  // Conservative local subset; native metadata must confirm the path that actually ran.
  if (raw.startsWith('file://')) return fileURLToPath(raw);
  if (raw.includes('://')) return undefined;
  if (raw.startsWith('@')) raw = raw.slice(1);
  if (raw === '~') raw = homedir();
  else if (raw.startsWith('~/')) raw = path.join(homedir(), raw.slice(2));
  return path.resolve(cwd, raw);
}

function skillPath(raw, commands, cwd) {
  if (!raw?.startsWith?.('skill://')) return localPath(raw, cwd);
  // Exact instruction-file URIs only. No selector, traversal, MCP miss or relative sidecar inference.
  const tail = raw.slice(8);
  const parts = tail.split('/');
  let name = parts[0];
  let rest = parts.slice(1);
  if (parts.length > 1) {
    const namespaced = `${parts[0]}/${decodeURIComponent(parts[1])}`;
    if (commands.some((c) => c.source === 'skill' && c.name === `skill:${namespaced}`)) {
      name = namespaced;
      rest = parts.slice(2);
    }
  }
  if (rest.length && !(rest.length === 1 && (rest[0] === '' || rest[0] === 'SKILL.md'))) return undefined;
  const winners = commands.filter((c) => c.source === 'skill' && c.name === `skill:${name}`);
  return winners.length === 1 ? winners[0].sourceInfo?.path : undefined;
}

/**
 * Call from a trusted native extension factory: ompAdaptation(omp, { manifest,
 * readerExecutable, getSkills: () => session.skills }). getSkills is an ordinary JS
 * integration callback to the PUBLIC AgentSession.skills getter, NOT an OMP API.
 * Without a callback, the injected omp.pi.getActiveSkills() supplies the native
 * main-process snapshot. Actual built-in result provenance must still match it.
 * Older hosts without that API cover command-enabled metadata only; no rescan.
 * Return value is a local disposer for hosts that replace a loader without shutdown.
 */
export default function ompAdaptation(omp, config = {}) {
  if (config.manifest === undefined && config.manifestPath === undefined) return () => {};
  let commands = [];
  let disabled = false;
  const calls = new Map();
  const subscriptions = [];
  const release = () => {
    disabled = true;
    calls.clear();
    commands = [];
    for (const off of subscriptions.splice(0)) off();
  };
  const snapshot = (manual) => {
    const nativeCommands = omp.getCommands();
    if (!Array.isArray(nativeCommands)) throw new Error('Invalid OMP runtime metadata');
    let skills = nativeCommands.filter((c) => c.source === 'skill').map((c) => ({ name: c.name, path: c.path }));
    if (!manual && (config.getSkills !== undefined || typeof omp.pi?.getActiveSkills === 'function')) {
      // Native file extensions receive the host SDK module as api.pi. Its
      // loaded main-session snapshot is independent of slash-command display.
      // SDK callers can provide the more precise session.skills callback.
      // A foreign/stale snapshot cannot pass the actual result path check.
      const loaded = config.getSkills !== undefined ? config.getSkills() : omp.pi.getActiveSkills();
      if (!Array.isArray(loaded)) throw new Error('Invalid OMP loaded skills');
      skills = loaded.map((s) => ({ name: `skill:${s.name}`, path: s.filePath }));
    }
    commands = [...nativeCommands.filter((c) => c.source !== 'skill'),
      ...skills.map((s) => ({ name: s.name, source: 'skill', sourceInfo: { path: s.path } }))];
  };
  const facade = {
    events: {
      emit: (channel, data) => omp.events.emit(channel, data),
      on: (channel, handler) => {
        const off = omp.events.on(channel, handler);
        subscriptions.push(off);
        return off;
      },
    },
    getCommands: () => commands,
    getAllTools: () => omp.getAllTools().map((tool) => tool.name === 'read' &&
      tool.sourceInfo?.source === 'builtin' && tool.sourceInfo.path === '<builtin:read>'
      ? { ...tool, sourceInfo: { ...tool.sourceInfo, path: 'builtin:read' } } : tool),
    on(type, handler) {
      const wrapped = (event, ctx) => {
        if (disabled) return;
        try {
          // Extension notifications use willContinue; the session wire uses isTerminal.
          if (type === 'agent_end' && (event.isTerminal === false || event.willContinue === true ||
              event.awaitingAsyncWork === true || ctx.hasPendingMessages())) return;
          if (type === 'session_shutdown') {
            try { return handler(event, ctx); } finally { release(); }
          }
          if (type === 'session_start' || type === 'session_tree' || type === 'session_switch' || type === 'session_branch' || type === 'agent_end') calls.clear();
          if (type === 'input') {
            // Public ctx semantics, not a fictitious OMP input.streamingBehavior field.
            const queued = !ctx.isIdle() || ctx.hasPendingMessages();
            if (!queued) calls.clear();
            // Let the core clear idle state even for unrelated input.
            const parsed = invocation(event.text);
            snapshot(true);
            const text = parsed && validName(parsed.name)
              ? `/skill:${parsed.name}${parsed.args ? ` ${parsed.args}` : ''}` : '';
            const result = handler({ ...event, text, streamingBehavior: queued ? 'steer' : undefined }, ctx);
            if (!result) return;
            // Retain the user's spelling/whitespace and let native buildSkillPromptMessage
            // load SKILL.md. Only append the core's FD-verified adaptation to native args.
            return { text: event.text + result.text.slice(text.length), images: event.images };
          }
          if (type === 'tool_call') {
            // Preserve duplicate-ID poisoning in the core, including foreign duplicate calls.
            if (calls.has(event.toolCallId)) {
              calls.set(event.toolCallId, null);
              return handler(event, ctx);
            }
            if (event.toolName !== 'read' || typeof event.toolCallId !== 'string' || calls.size >= 2048) return;
            snapshot(false);
            const resolved = skillPath(event.input?.path, commands, ctx.cwd);
            if (!resolved || !path.isAbsolute(resolved)) return;
            calls.set(event.toolCallId, { raw: event.input.path, resolved });
            // Encode only the core's observation; never mutate OMP's actual tool input.
            return handler({ ...event, input: { ...event.input, path: pathToFileURL(resolved).href } }, ctx);
          }
          if (type === 'tool_result') {
            // Consume the core observation on every completed result, even rejected proof.
            const discard = () => handler({ ...event, isError: true }, ctx);
            const call = calls.get(event.toolCallId);
            calls.delete(event.toolCallId);
            if (!call || event.toolName !== 'read' || event.input?.path !== call.raw) return discard();
            snapshot(false);
            // Check ONLY native result provenance, before injecting anything. The facade's
            // additions never become content, details, meta.source or a subsequent read proof.
            const source = event.details?.meta?.source;
            if (call.raw.startsWith('skill://')
              ? source?.type !== 'internal' || source.value !== call.raw
              : source?.type !== 'path') return discard();
            const details = event.details;
            // skill:// instruction documents are unbounded native text reads:
            // buildInMemoryTextResult emits full display rows, not fileSize.
            // An EOF/empty notice has no full display rows and is not a body read.
            const display = details?.displayContent;
            const wholeInstruction = call.raw.startsWith('skill://') &&
              Number.isSafeInteger(details?.totalLines) && details.totalLines > 0 &&
              display?.startLine === 1 && typeof display.text === 'string' && display.text.length > 0 &&
              Array.isArray(display.lineNumbers) && display.lineNumbers.length === details.totalLines &&
              display.lineNumbers.every((line, index) => line === index + 1) &&
              !details.truncation && !details.meta?.truncation;
            if ((!wholeInstruction && (!Number.isSafeInteger(details?.fileSize) || details.fileSize <= 0)) ||
                details?.isDirectory || details?.meta?.truncation?.partialLine ||
                details?.meta?.truncation?.outputBytes === 0) return discard();
            const resolved = event.details?.resolvedPath ?? (source.type === 'path' ? source.value : undefined);
            if (typeof resolved !== 'string' || !path.isAbsolute(resolved) ||
                (source.type === 'path' && source.value !== resolved)) return discard();
            const result = handler({ ...event, input: { ...event.input, path: pathToFileURL(call.resolved).href } }, ctx);
            if (result) return { additionalContext: result.content.slice(event.content.length).map((p) => p.text).join('') };
            return;
          }
          return handler(event, ctx);
        } catch { return; } // No prompts, file bytes, paths or filesystem errors are logged.
      };
      omp.on(type, wrapped);
      if (type === 'session_start') {
        omp.on('session_switch', wrapped);
        omp.on('session_branch', wrapped);
      }
    },
  };
  piAdaptation(facade, config, {
    claimChannel: 'pleat:omp-adaptation:v1:claim', validName,
    sourceKey: (entry) => entry.sourceIdentity,
    resultPath: (event) => event.details?.resolvedPath ?? event.details?.meta?.source?.value,
  });
  return release;
}
