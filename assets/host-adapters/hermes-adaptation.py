"""Hermes 5fc490e thin plugin. Generated __init__.py calls register(ctx, config).

Config is a schemaVersion=1, skills[] snapshot plus readerExecutable, or a
bounded zero-argument provider of that snapshot (descriptor loading is external).
Only the public Rust FD reader reads adaptation bytes. No config discovery,
tool override, prompt-path parsing, or process environment inheritance.
"""

import asyncio
import contextvars
import hashlib
import json
import os
import re
import selectors
import stat
import subprocess
import sys
import threading
import time
import types
from collections import OrderedDict, deque

MAX_ENTRIES = 4096
MAX_CONFIG = 8 * 1024 * 1024
MAX_NATIVE = 2 * 1024 * 1024
MAX_ADAPTATION = 64 * 1024
MAX_RESPONSE = 256 * 1024
MAX_SEEN = 512
MAX_PROFILES = 128
# The pinned native consumer concurrently expands at most 16 explicit refs.
# Admit a complete healthy message, while hung native calls retain their slots.
MAX_NATIVE_INFLIGHT = 16
_REFERENCE_BUS = "_pleat_hermes_adaptation_reference_bus_v1"
READER_SECONDS = 3.0
NATIVE_SECONDS = 2.0
BIND_SECONDS = 1.0
FIELD = "pleat_adaptation"
FLAG = "--pleat-native-read-adaptation-v1"
UNAVAILABLE = "Pleat adaptation unavailable; no input queued."


def _require(ok):
    if not ok:
        raise ValueError("Invalid managed adaptation resource")


def _path(value):
    return (isinstance(value, str) and 0 < len(value) <= 4096
            and not any(ord(c) < 32 or 127 <= ord(c) < 160 for c in value)
            and value.startswith("/") and not value.startswith("//")
            and os.path.normpath(value) == value)


def _name(value):
    return (isinstance(value, str) and 0 < len(value) <= 1024
            and not any(c.isspace() or ord(c) < 32 or 127 <= ord(c) < 160 for c in value))


def _identity(info):
    return f"{info.st_dev}:{info.st_ino}"


def _revision(info):
    return (info.st_dev, info.st_ino, info.st_size, info.st_mtime_ns,
            info.st_ctime_ns, info.st_nlink)


def _check_time(deadline):
    _require(time.monotonic() < deadline)


def _chain(logical, deadline):
    """Bounded OS-order link resolution; '..' in link targets is applied in order."""
    _require(_path(logical))
    pending, current, observed, links, steps = deque(logical.split("/")), "/", [], 0, 0
    while pending:
        _check_time(deadline)
        steps += 1
        _require(steps <= 512)
        part = pending.popleft()
        if part in ("", "."):
            continue
        if part == "..":
            current = os.path.dirname(current)
            continue
        path = os.path.join(current, part)
        info = os.lstat(path)
        if stat.S_ISLNK(info.st_mode):
            links += 1
            _require(links <= 40)
            target = os.readlink(path)
            _require(0 < len(target) <= 4096 and "\0" not in target)
            observed.append((path, _revision(info), target))
            if target.startswith("/"):
                current = "/"
            pending.extendleft(reversed(target.split("/")))
            _require(len(pending) <= 512)
        else:
            _require(stat.S_ISDIR(info.st_mode))
            observed.append((path, _identity(info), None))
            current = path
    root_info = os.lstat(current)
    _require(stat.S_ISDIR(root_info.st_mode))
    for path, stamp, target in observed:
        _check_time(deadline)
        now = os.lstat(path)
        _require((stat.S_ISDIR(now.st_mode) and _identity(now) == stamp) if target is None
                 else (stat.S_ISLNK(now.st_mode) and _revision(now) == stamp
                       and os.readlink(path) == target))
    return current, _identity(root_info), tuple(observed)


def _snapshot(entry, native_dir, deadline):
    source = _chain(entry["canonicalSourceRoot"], deadline)
    installed = _chain(os.path.dirname(entry["installationSkillPath"]), deadline)
    winner = _chain(native_dir, deadline)
    _require(source[:2] == installed[:2] == winner[:2]
             and source[1] == entry["sourceIdentity"])
    skill = os.lstat(os.path.join(source[0], "SKILL.md"))
    installation = os.lstat(entry["installationSkillPath"])
    native = os.lstat(os.path.join(native_dir, "SKILL.md"))
    _require(all(stat.S_ISREG(s.st_mode) and s.st_nlink == 1
                 and _revision(s) == _revision(skill) for s in (skill, installation, native)))
    return source, installed, winner, _revision(skill)


def _unique_object(pairs):
    value = {}
    for key, item in pairs:
        _require(key not in value)
        value[key] = item
    return value


def _json(raw, limit):
    _require(isinstance(raw, (str, bytes)) and len(raw) <= limit)
    if isinstance(raw, bytes):
        raw = raw.decode("utf-8", errors="strict")
    _require(len(raw.encode("utf-8")) <= limit)
    def invalid_constant(_):
        raise ValueError("Invalid JSON constant")
    return json.loads(raw, object_pairs_hook=_unique_object, parse_constant=invalid_constant)


def _reader(executable, request):
    """Bound stdout while polling; communicate()/capture_output could grow without limit."""
    data = json.dumps(request, ensure_ascii=False).encode("utf-8")
    _require(len(data) <= 16 * 1024)
    deadline = time.monotonic() + READER_SECONDS
    with subprocess.Popen([executable, FLAG], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                          stderr=subprocess.DEVNULL, shell=False, close_fds=True,
                          env={"PATH": "/usr/bin:/bin", "LANG": "C.UTF-8"}) as process:
        try:
            with selectors.DefaultSelector() as selector:
                for stream, event in ((process.stdin, selectors.EVENT_WRITE),
                                      (process.stdout, selectors.EVENT_READ)):
                    os.set_blocking(stream.fileno(), False)
                    selector.register(stream, event)
                offset, output = 0, bytearray()
                while selector.get_map():
                    _check_time(deadline)
                    for key, _ in selector.select(max(0, deadline - time.monotonic())):
                        if key.fileobj is process.stdin:
                            offset += os.write(key.fd, data[offset:])
                            if offset == len(data):
                                selector.unregister(process.stdin)
                                process.stdin.close()
                        else:
                            chunk = os.read(key.fd, 4096)
                            if not chunk:
                                selector.unregister(process.stdout)
                            else:
                                output.extend(chunk)
                                _require(len(output) <= MAX_RESPONSE)
                _require(process.wait(timeout=max(0.001, deadline - time.monotonic())) == 0)
                return _json(bytes(output), MAX_RESPONSE)
        finally:
            if process.poll() is None:
                process.kill()
                process.wait()


def _config(config):
    _require(type(config) is dict and {"schemaVersion", "skills"} <= set(config)
             <= {"schemaVersion", "skills", "readerExecutable"}
             and type(config["schemaVersion"]) is int and config["schemaVersion"] == 1
             and type(config["skills"]) is list and len(config["skills"]) <= MAX_ENTRIES)
    shared = config.get("readerExecutable")
    _require("readerExecutable" not in config or _path(shared))
    entries = {}
    for item in config["skills"]:
        required = {"installationSkillPath", "canonicalSourceRoot", "sourceIdentity", "nativeName"}
        _require(type(item) is dict and required <= set(item) <= required | {"readerExecutable"})
        _require(_path(item["installationSkillPath"])
                 and os.path.basename(item["installationSkillPath"]) == "SKILL.md"
                 and _path(item["canonicalSourceRoot"]) and _name(item["nativeName"])
                 and isinstance(item["sourceIdentity"], str) and len(item["sourceIdentity"]) <= 64
                 and re.fullmatch(r"[0-9]+:[0-9]+", item["sourceIdentity"])
                 and _path(item.get("readerExecutable", shared)))
        copied = dict(item)
        old = entries.setdefault(item["installationSkillPath"], copied)
        _require(old == copied)  # Conflicting exact paths never borrow an alias's authorization.
    _require(len(json.dumps(config, ensure_ascii=False).encode("utf-8")) <= MAX_CONFIG)
    return tuple(entries.values()), shared


def _scope(session_id="", task_id="", session_key=""):
    for kind, value in (("session", session_id), ("task", task_id), ("route", session_key)):
        if _name(value):
            return kind, value
    return None  # No global/default session or environment fallback.


def _command_allowed(ctx):
    """Native loader/caller must bind this plugin's profile; never manufacture a grant."""
    try:
        from hermes_cli.config import load_config_readonly
        entry = load_config_readonly()["plugins"]["entries"][ctx.plugin_id]
        return entry.get("allow_gateway_injection") is True
    except Exception:
        return False


def _profile_key():
    from hermes_constants import get_hermes_home
    return _chain(os.fspath(get_hermes_home()), time.monotonic() + BIND_SECONDS)[:2]


def _reference_selection(bus):
    key = _profile_key()
    with bus.lock:
        selected = bus.profiles.get(key)
        _require(selected is not None and not selected[1].closed)
    return key, selected


def _reference_current(bus, key, selected):
    _require(_profile_key() == key)
    with bus.lock:
        _require(bus.profiles.get(key) is selected and not selected[1].closed)


def _reference_work(bus, target, limit=None):
    key, selected = _reference_selection(bus)
    core = selected[1]
    snapshot = core.snapshot()
    _reference_current(bus, key, selected)
    if limit is not None:
        from agent.context_references import ContextCompletionItem
        names = sorted({e["nativeName"] for e in snapshot[0] if e["nativeName"].startswith(target)})
        result = [ContextCompletionItem(name, display=name) for name in names[:max(0, min(limit, 32))]]
    else:
        payload, extra = core._load(core._native(target, None), snapshot)
        result = core._message(payload, extra)
    _reference_current(bus, key, selected)  # Reject a replaced/closed owner while work was in flight.
    return result


def _attach_reference(ctx, core):
    """Own module bus survives native per-profile plugin-module eviction; never patch Hermes."""
    from agent.context_references import ContextReferenceProvider, get_context_reference_providers

    candidate = types.ModuleType(_REFERENCE_BUS)
    candidate.plugin_id, candidate.protocol = ctx.plugin_id, 1
    candidate.lock, candidate.profiles, candidate.provider = threading.RLock(), {}, None
    bus = sys.modules.setdefault(_REFERENCE_BUS, candidate)
    _require(isinstance(bus, types.ModuleType) and bus.protocol == 1 and bus.plugin_id == ctx.plugin_id)
    key = _profile_key()

    class PleatSkillReference(ContextReferenceProvider):
        prefix = "pleat-skill"
        description = "Load a managed native skill and its adaptation"

        async def autocomplete(self, query, *, limit=10):
            if not isinstance(query, str) or len(query) > 1024 or type(limit) is not int:
                return []
            try:
                return await asyncio.to_thread(_reference_work, bus, query, limit)
            except Exception:
                return []

        async def expand(self, target):
            # No session/task identity or consumption receipt: never persist a dedup claim.
            try:
                _require(_name(target))
                return await asyncio.to_thread(_reference_work, bus, target)
            except Exception:
                return None

    with core.lock, bus.lock:
        _require(not core.closed)
        existing = get_context_reference_providers().get("pleat-skill")
        if existing is not None and existing is not bus.provider:
            return "foreign_prefix"  # Even matching attributes are not proof of object ownership.
        if bus.provider is None:
            bus.provider = PleatSkillReference()
        if existing is None:
            ctx.register_context_reference(bus.provider)
            if get_context_reference_providers().get("pleat-skill") is not bus.provider:
                return "registration_refused"
        _require(not core.closed and _profile_key() == key)
        # A recreated directory cannot inherit the deleted profile's authorization.
        for stale in [k for k in bus.profiles if k[0] == key[0] and k != key]:
            del bus.profiles[stale]
        if key not in bus.profiles and len(bus.profiles) >= MAX_PROFILES:
            return "profile_capacity"
        selected = (object(), core)
        bus.profiles[key] = selected

        def detach():
            with bus.lock:
                if bus.profiles.get(key) is selected:
                    del bus.profiles[key]

        core.detach_reference = detach
    return "active"


class _Adapter:
    def __init__(self, ctx, config):
        self.ctx, self.config = ctx, config
        self.lock, self.seen = threading.RLock(), OrderedDict()
        self.native_slot = threading.BoundedSemaphore(MAX_NATIVE_INFLIGHT)
        self.detach_reference = None
        self.reference_status = "unavailable"
        self.closed = False

    def snapshot(self):
        # Exactly one provider call per invocation; never reopen descriptors here.
        _require(not self.closed)
        deadline = time.monotonic() + BIND_SECONDS
        value = self.config() if callable(self.config) else self.config
        snapshot = _config(value)
        _check_time(deadline)
        return snapshot

    def reset(self, session_id="", deleted_ids=None, **_):
        # Hermes emits on_session_end after each message. Only the committed
        # delete/archive/reset event retires a durable session's claims.
        ids = {session_id} if _name(session_id) else set()
        if isinstance(deleted_ids, (list, tuple)):
            if len(deleted_ids) > MAX_SEEN:
                with self.lock:
                    self.seen.clear()  # Bounded safe repeat for an oversized native sweep.
                return
            ids.update(value for value in deleted_ids if _name(value))
        with self.lock:
            for key in list(self.seen):
                if key[0] is not None and key[0][0] == "session" and key[0][1] in ids:
                    del self.seen[key]

    def close(self):
        with self.lock:
            self.closed = True
            self.seen.clear()
            detach, self.detach_reference = self.detach_reference, None
        if detach is not None:
            detach()

    def _native(self, name, task_id):
        """Bounded native calls; a timed-out daemon never queues/injects anything."""
        _require(not self.closed and self.native_slot.acquire(blocking=False))
        done, outcome = threading.Event(), []
        def call():
            try:
                from tools.skills_tool import skill_view
                outcome.append(skill_view(name, task_id=task_id or None, preprocess=False))
            except Exception:
                pass
            finally:
                self.native_slot.release()
                done.set()
        try:
            threading.Thread(target=contextvars.copy_context().run, args=(call,), daemon=True,
                             name="pleat-hermes-skill").start()
        except Exception:
            self.native_slot.release()
            raise
        _require(done.wait(NATIVE_SECONDS) and outcome and not self.closed)
        return outcome[0]

    def _load(self, raw, snapshot):
        _require(not self.closed)
        entries, executable = snapshot
        payload = _json(raw, MAX_NATIVE)
        _require(type(payload) is dict and payload.get("success") is True
                 and not payload.get("error") and not payload.get("dedup")
                 and payload.get("content_returned") is not False
                 and payload.get("status") != "unchanged" and not payload.get("file")
                 and isinstance(payload.get("content"), str) and payload["content"].strip()
                 and _path(payload.get("skill_dir")) and FIELD not in payload)
        deadline = time.monotonic() + BIND_SECONDS
        native_dir = payload["skill_dir"]
        winner = _chain(native_dir, deadline)
        exact = tuple(e for e in entries
                      if e["installationSkillPath"] == os.path.join(native_dir, "SKILL.md"))
        candidates = exact or tuple(e for e in entries if e["sourceIdentity"] == winner[1])
        bound = None
        for entry in candidates:
            _check_time(deadline)
            try:
                snapshot = _snapshot(entry, native_dir, deadline)
                bound = entry, snapshot
                break
            except (OSError, ValueError):
                if exact:
                    raise
        _require(bound is not None)
        entry, before = bound
        root = before[0][0]
        local = os.lstat(os.path.join(root, "local-data"))
        _require(stat.S_ISDIR(local.st_mode))
        request = {"schemaVersion": 1, "sourceRoot": root,
                   "sourceIdentity": entry["sourceIdentity"], "localDataIdentity": _identity(local)}
        response = _reader(entry.get("readerExecutable", executable), request)
        _require(type(response) is dict and type(response.get("schemaVersion")) is int
                 and all(response.get(k) == v for k, v in request.items())
                 and isinstance(response.get("text"), str))
        text = response["text"]
        data = text.encode("utf-8", errors="strict")
        digest = hashlib.sha256(data).hexdigest()
        _require(0 < len(data) <= MAX_ADAPTATION and text.strip() and "\0" not in text
                 and response.get("digest") == digest)
        after_local = os.lstat(os.path.join(root, "local-data"))
        _require(stat.S_ISDIR(after_local.st_mode) and _identity(after_local) == request["localDataIdentity"]
                 and _snapshot(entry, native_dir, time.monotonic() + BIND_SECONDS) == before)
        return payload, {**request, "digest": digest, "text": text,
                         "adaptationDirectory": os.path.join(root, "local-data"),
                         "instructions": "Apply over upstream defaults; current user instructions, "
                         "authorization and safety rules take priority. Resolve adaptation-relative "
                         "references against adaptationDirectory."}

    def _emit(self, scope, extra, emit):
        # ponytail: bounded per-registration LRU; eviction permits a safe repeat.
        with self.lock:
            _require(not self.closed)
            key = (scope, extra["sourceIdentity"]) if scope else None
            duplicate = key is not None and self.seen.get(key) == extra["digest"]
            value = emit(None if duplicate else extra)
            if key is not None and value is not None and value is not False:
                self.seen[key] = extra["digest"]
                self.seen.move_to_end(key)
                while len(self.seen) > MAX_SEEN:
                    self.seen.popitem(last=False)
            return value

    def transform(self, tool_name=None, args=None, result=None, status=None,
                  session_id="", task_id="", **_):
        if (tool_name != "skill_view" or status not in (None, "ok")
                or type(args) is not dict or args.get("file_path")):
            return None
        try:
            payload, extra = self._load(result, self.snapshot())
            return self._emit(_scope(session_id, task_id), extra, lambda addition:
                json.dumps({**payload, FIELD: addition}, ensure_ascii=False) if addition else None)
        except Exception:
            return None  # Hermes preserves the exact original result; never log exception data.

    @staticmethod
    def _message(payload, extra, instruction=""):
        message = "[Pleat skill invocation]\nSkill directory: " + json.dumps(payload["skill_dir"])
        message += "\n\n" + payload["content"]
        if extra:
            message += ("\n\n[PLEAT managed adaptation]\nAdaptation directory: "
                        + json.dumps(os.path.join(extra["sourceRoot"], "local-data"))
                        + "\nApply this adaptation over upstream defaults; current user instructions, "
                        "authorization and safety rules take priority. Resolve adaptation-relative "
                        "references against the adaptation directory.\n" + extra["text"]
                        + "\n[End PLEAT managed adaptation]")
        if instruction:
            message += "\n\nUser instruction:\n" + instruction
        return message

    def command(self, raw_args, *, session_key="", session_id="", **_):
        try:
            _require(_command_allowed(self.ctx) and _name(session_key)
                     and isinstance(raw_args, str) and len(raw_args) <= MAX_NATIVE)
            parts = raw_args.split(None, 1)
            _require(parts and _name(parts[0]))
            snapshot = self.snapshot()
            payload, extra = self._load(self._native(parts[0], session_id), snapshot)
            accepted = self._emit(_scope(session_id=session_id, session_key=session_key), extra,
                lambda addition: self.ctx.inject_message(
                    self._message(payload, addition, parts[1] if len(parts) > 1 else ""),
                    role="user", session_key=session_key))
            return "Pleat skill input accepted for scheduling." if accepted is True else UNAVAILABLE
        except Exception:
            return UNAVAILABLE


def register(ctx, config):
    """One shared plugin; each invocation uses one fresh bounded descriptor snapshot."""
    if not callable(config):
        entries, executable = _config(config)
        config = {"schemaVersion": 1, "skills": list(entries),
                  **({"readerExecutable": executable} if executable is not None else {})}
    adapter = _Adapter(ctx, config)

    ctx.register_hook("transform_tool_result", adapter.transform)
    if _command_allowed(ctx):
        ctx.register_command("pleat-skill", adapter.command,
                             description="Load a managed native skill and its adaptation",
                             args_hint="<native-name> [instruction]")
    ctx.register_hook("on_session_delete", adapter.reset)
    ctx.on_unload(adapter.close)
    try:
        adapter.reference_status = _attach_reference(ctx, adapter)
    except Exception:
        adapter.reference_status = "unavailable"
    return adapter
