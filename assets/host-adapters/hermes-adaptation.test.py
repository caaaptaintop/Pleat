"""stdlib-only mocks: no Hermes, Rust binary, SDK, CLI, network, or user config."""

import abc
import asyncio
import contextvars
import copy
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import threading
import time
import types
import unittest
from unittest import mock


ASSET = Path(__file__).with_name("hermes-adaptation.py")
spec = importlib.util.spec_from_file_location("pleat_hermes_asset_under_test", ASSET)
adapter = importlib.util.module_from_spec(spec)
spec.loader.exec_module(adapter)  # Only our asset; native imports below are synthetic modules.


class Reference(abc.ABC):
    @abc.abstractmethod
    async def autocomplete(self, query, *, limit=10):
        pass

    @abc.abstractmethod
    async def expand(self, target):
        pass


class Completion:
    def __init__(self, text, display="", meta=""):
        self.text, self.display, self.meta = text, display or text, meta


class Context:
    plugin_id = "pleat-native-adaptation"

    def __init__(self, registry=None):
        self.hooks, self.commands, self.references, self.unloaders = {}, {}, [], []
        self.registry = {} if registry is None else registry
        self.inject_message = mock.Mock(return_value=True)

    def register_hook(self, name, handler):
        self.hooks[name] = handler
        return object()

    def register_command(self, name, handler, **kwargs):
        self.commands[name] = handler
        return object()

    def register_context_reference(self, provider):
        if not isinstance(provider, Reference):
            raise TypeError("wrong native base class")
        if provider.prefix in self.registry:
            return  # Native ctx catches ValueError and leaves the original prefix untouched.
        self.registry[provider.prefix] = provider
        self.references.append(provider)

    def on_unload(self, callback):
        self.unloaders.append(callback)


class HermesAdaptationTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.base = Path(self.temp.name).resolve()
        self.registry = {}
        self.ctx, self.native_results, self.text = Context(self.registry), {}, "LOCAL-ADAPTATION"
        self.profile_a = self.base / "profile-A"
        self.profile_a.mkdir()
        self.home = contextvars.ContextVar("mock-authoritative-hermes-home", default=self.profile_a)
        self.permission = False
        self.skill_view = mock.Mock(side_effect=lambda name, **kw: self.native_results[name])
        references = types.ModuleType("agent.context_references")
        references.ContextReferenceProvider, references.ContextCompletionItem = Reference, Completion
        references.get_context_reference_providers = lambda: dict(self.registry)
        constants = types.ModuleType("hermes_constants")
        constants.get_hermes_home = self.home.get
        skills = types.ModuleType("tools.skills_tool")
        skills.skill_view = self.skill_view
        config = types.ModuleType("hermes_cli.config")
        config.load_config_readonly = lambda: {"plugins": {"entries": {
            self.ctx.plugin_id: {"allow_gateway_injection": self.permission}}}}
        modules = {
            "agent": types.ModuleType("agent"), "agent.context_references": references,
            "tools": types.ModuleType("tools"), "tools.skills_tool": skills,
            "hermes_cli": types.ModuleType("hermes_cli"), "hermes_cli.config": config,
            "hermes_constants": constants,
        }
        patch = mock.patch.dict(sys.modules, modules)
        patch.start()
        self.addCleanup(patch.stop)
        self.reader = mock.Mock(side_effect=self.read_response)
        patch = mock.patch.object(adapter, "_reader", self.reader)
        patch.start()
        self.addCleanup(patch.stop)
        self.root = self.fixture("source")
        self.install = self.alias("installation", self.root)
        self.entry = self.descriptor(self.install, self.root)
        self.config = {"schemaVersion": 1, "skills": [self.entry],
                       "readerExecutable": str(self.base / "public-reader")}
        self.raw = self.result(self.install)
        self.native_results["sample"] = self.raw

    def fixture(self, name):
        root = self.base / name
        root.mkdir()
        (root / "SKILL.md").write_text("NATIVE-MAIN", encoding="utf-8")
        (root / "local-data").mkdir()
        # No ADAPTATION.md: a passing test must obtain its content from the fake Rust reader.
        return root

    def alias(self, name, root):
        link = self.base / name
        link.symlink_to(root, target_is_directory=True)
        return link

    def descriptor(self, installed, root, name="sample"):
        return {"installationSkillPath": str(installed / "SKILL.md"),
                "canonicalSourceRoot": str(root), "sourceIdentity": adapter._identity(root.lstat()),
                "nativeName": name}

    def result(self, installed, **values):
        return json.dumps({"success": True, "content": "NATIVE-MAIN", "name": "sample",
                           "skill_dir": str(installed), "custom": {"nested": [1, None, False]},
                           "futureField": "UNCHANGED", **values})

    def read_response(self, executable, request):
        return {**request, "text": self.text,
                "digest": hashlib.sha256(self.text.encode("utf-8")).hexdigest()}

    def register(self, config=None):
        core = adapter.register(self.ctx, self.config if config is None else config)
        self.addCleanup(core.close)
        return core

    def register_profile(self, home, config, module=adapter):
        ctx = Context(self.registry)
        token = self.home.set(home)
        try:
            core = module.register(ctx, config)
        finally:
            self.home.reset(token)
        self.addCleanup(core.close)
        return ctx, core

    def transform(self, core, raw=None, **kw):
        return core.transform(tool_name="skill_view", args={"name": "sample"},
                              result=self.raw if raw is None else raw, status="ok", **kw)

    def test_registration_and_native_provider_signature_without_injection_permission(self):
        self.register()
        self.assertNotIn("pleat-skill", self.ctx.commands)
        self.assertIn("transform_tool_result", self.ctx.hooks)
        provider = self.ctx.references[0]
        self.assertIsInstance(provider, Reference)
        self.assertEqual(provider.prefix, "pleat-skill")
        completions = asyncio.run(provider.autocomplete("sam", limit=1))
        self.assertEqual([c.text for c in completions], ["sample"])
        self.assertEqual([f"@{provider.prefix}:{c.text}" for c in completions], ["@pleat-skill:sample"])
        self.assertEqual(asyncio.run(provider.autocomplete("", limit=-1)), [])

    def test_transform_only_adds_independent_field(self):
        original = json.loads(self.raw)
        transformed = json.loads(self.transform(self.register(), session_id="SESSION-A"))
        extra = transformed.pop(adapter.FIELD)
        self.assertEqual(transformed, original)
        self.assertEqual(extra["text"], self.text)
        self.assertEqual(extra["sourceRoot"], str(self.root))
        self.assertEqual(self.reader.call_count, 1)
        request = self.reader.call_args.args[1]
        self.assertEqual(set(request), {"schemaVersion", "sourceRoot", "sourceIdentity", "localDataIdentity"})
        self.assertEqual(request["localDataIdentity"], adapter._identity((self.root / "local-data").lstat()))

    def test_python_never_opens_adaptation_body(self):
        core = self.register()
        with mock.patch("builtins.open", side_effect=AssertionError("Python file read forbidden")), \
             mock.patch.object(Path, "read_text", side_effect=AssertionError("Python text read forbidden")), \
             mock.patch.object(os, "open", side_effect=AssertionError("Python fd read forbidden")):
            self.assertIsInstance(self.transform(core), str)

    def test_only_successful_full_main_result_is_eligible(self):
        core = self.register()
        for changes in ({"success": False}, {"success": 1}, {"content": ""}, {"content": "  "},
                        {"dedup": True}, {"content_returned": False}, {"status": "unchanged"},
                        {"error": "synthetic"}, {"file": "references/a.md"},
                        {"skill_dir": None}, {adapter.FIELD: {"existing": True}}):
            with self.subTest(fields=list(changes)):
                self.assertIsNone(self.transform(core, self.result(self.install, **changes)))
        for raw in ("not json", "[]", '{"success":true,"success":true}', "{\"x\":NaN}"):
            self.assertIsNone(self.transform(core, raw))
        self.assertIsNone(core.transform(tool_name="other", args={}, result=self.raw, status="ok"))
        self.assertIsNone(core.transform(tool_name="skill_view", args={"file_path": "file"}, result=self.raw))
        self.assertIsNone(core.transform(tool_name="skill_view", args={}, result=self.raw, status="error"))
        self.reader.assert_not_called()

    def test_same_native_name_different_source_uses_actual_skill_dir(self):
        root = self.fixture("second-source")
        installed = self.alias("second-install", root)
        self.config["skills"].append(self.descriptor(installed, root))
        core = self.register()
        result = json.loads(self.transform(core, self.result(installed), session_id="S"))
        self.assertEqual(result[adapter.FIELD]["sourceIdentity"], adapter._identity(root.lstat()))
        self.assertEqual(self.reader.call_args.args[1]["sourceRoot"], str(root))

    def test_metadata_name_does_not_authorize_or_prevent_a_verified_source(self):
        self.entry["nativeName"] = "stale-alias"
        self.assertIsInstance(self.transform(self.register()), str)
        self.entry["sourceIdentity"] = "0:0"
        self.assertIsNone(self.transform(self.register()))

    def test_original_installation_chain_must_still_exist(self):
        core = self.register()
        winner = self.alias("native-winner", self.root)
        self.install.unlink()
        self.assertIsNone(self.transform(core, self.result(winner)))
        self.reader.assert_not_called()

    def test_exact_bad_descriptor_cannot_borrow_valid_alias(self):
        alias = self.alias("authorized-alias", self.root)
        self.config["skills"].append(self.descriptor(alias, self.root))
        self.entry["sourceIdentity"] = "0:0"
        self.assertIsNone(self.transform(self.register()))
        self.reader.assert_not_called()

    def test_native_winner_alias_does_not_replace_manifest_installation(self):
        winner = self.alias("native-winner", self.root)
        self.assertIsInstance(self.transform(self.register(), self.result(winner)), str)

    def test_compat_source_locator_preserves_source_identity(self):
        core = self.register()
        moved = self.base / "moved-source"
        self.root.rename(moved)
        self.root.symlink_to(moved, target_is_directory=True)
        self.assertIsInstance(self.transform(core), str)
        self.assertEqual(self.reader.call_args.args[1]["sourceRoot"], str(moved))
        self.root.unlink()
        replacement = self.fixture("replacement")
        self.root.symlink_to(replacement, target_is_directory=True)
        self.assertIsNone(self.transform(core))

    def test_replaced_installation_during_reader_is_rejected(self):
        core = self.register()
        foreign = self.fixture("foreign")
        def replace(executable, request):
            answer = self.read_response(executable, request)
            self.install.unlink()
            self.install.symlink_to(foreign, target_is_directory=True)
            return answer
        self.reader.side_effect = replace
        self.assertIsNone(self.transform(core, session_id="S"))
        self.assertEqual(len(core.seen), 0)

    def test_deleted_installation_during_reader_is_rejected(self):
        core = self.register()
        def remove(executable, request):
            answer = self.read_response(executable, request)
            self.install.unlink()
            return answer
        self.reader.side_effect = remove
        self.assertIsNone(self.transform(core))

    def test_replaced_local_directory_during_reader_is_rejected(self):
        def replace(executable, request):
            answer = self.read_response(executable, request)
            (self.root / "local-data").rename(self.root / "held-local")
            (self.root / "local-data").mkdir()
            return answer
        self.reader.side_effect = replace
        self.assertIsNone(self.transform(self.register()))

    def test_skill_file_mutation_during_reader_is_rejected(self):
        def mutate(executable, request):
            answer = self.read_response(executable, request)
            (self.root / "SKILL.md").write_text("CONCURRENT-NEW-BODY", encoding="utf-8")
            return answer
        self.reader.side_effect = mutate
        self.assertIsNone(self.transform(self.register()))

    def test_source_id_digest_dedup_is_scoped_and_alias_independent(self):
        alias = self.alias("other-alias", self.root)
        self.config["skills"].append(self.descriptor(alias, self.root, "other"))
        core = self.register()
        self.assertIsInstance(self.transform(core, session_id="S", task_id="T1"), str)
        self.assertIsNone(self.transform(core, self.result(alias), session_id="S", task_id="T2"))
        self.assertIsInstance(self.transform(core, session_id="OTHER"), str)
        self.text = "NEW-ADAPTATION-VERSION"
        self.assertIsInstance(self.transform(core, session_id="S"), str)
        self.assertEqual(self.reader.call_count, 4)  # Fresh read even on a dedup hit.

    def test_task_fallback_and_missing_identity_never_become_global_claims(self):
        core = self.register()
        self.assertIsInstance(self.transform(core, task_id="T1"), str)
        self.assertIsNone(self.transform(core, task_id="T1"))
        self.assertIsInstance(self.transform(core, task_id="T2"), str)
        length = len(core.seen)
        self.assertIsInstance(self.transform(core), str)
        self.assertIsInstance(self.transform(core), str)
        self.assertEqual(len(core.seen), length)

    def test_concurrent_same_session_same_digest_produces_one_claim(self):
        core = self.register()
        both = threading.Barrier(2)
        results = []
        def response(executable, request):
            both.wait(timeout=1)
            return self.read_response(executable, request)
        self.reader.side_effect = response
        workers = [threading.Thread(target=lambda: results.append(
            self.transform(core, session_id="S"))) for _ in range(2)]
        for worker in workers:
            worker.start()
        for worker in workers:
            worker.join(timeout=2)
            self.assertFalse(worker.is_alive())
        self.assertEqual(sum(isinstance(result, str) for result in results), 1)
        self.assertEqual(sum(result is None for result in results), 1)

    def test_bounded_cache_lifecycle_reset_and_unload(self):
        core = self.register()
        with mock.patch.object(adapter, "MAX_SEEN", 3):
            for n in range(8):
                self.assertIsInstance(self.transform(core, session_id=f"S{n}"), str)
            self.assertEqual(len(core.seen), 3)
            self.assertIsInstance(self.transform(core, session_id="S0"), str)
        self.ctx.hooks["on_session_delete"](session_id="S7", deleted_ids=["S7", "S6"])
        self.assertEqual(len(core.seen), 1)
        self.assertIsNone(self.transform(core, session_id="S0"))
        self.assertIsInstance(self.transform(core, session_id="S7"), str)
        self.ctx.unloaders[0]()
        self.assertIsNone(self.transform(core))
        self.assertIsNone(asyncio.run(self.ctx.references[0].expand("sample")))

    def test_native_end_of_message_and_prompt_rebuild_do_not_erase_session_claim(self):
        core=self.register();self.assertIsInstance(self.transform(core,session_id='continuous'),str)
        # Replay the pinned consumer's real per-message event, then its optional
        # prompt rebuild event. Either may occur while the transcript continues.
        for event in ['on_session_end','on_session_start']:
            callback=self.ctx.hooks.get(event)
            if callback:callback(session_id='continuous',completed=True,failed=False)
        self.assertIsNone(self.transform(core,session_id='continuous'))
        self.assertIsInstance(self.transform(core,session_id='other-session'),str)

    def test_native_committed_delete_is_scoped_and_accepts_actual_bulk_field(self):
        core=self.register()
        for name in ['A','B','C']:self.assertIsInstance(self.transform(core,session_id=name),str)
        callback=self.ctx.hooks['on_session_delete'];callback(reason='invalid-no-identity')
        self.assertEqual(len(core.seen),3)
        callback(session_id='A',deleted_ids=['A','B'],reason='session_reset')
        self.assertIsNone(self.transform(core,session_id='C'))
        self.assertIsInstance(self.transform(core,session_id='A'),str)
        self.assertIsInstance(self.transform(core,session_id='B'),str)
        callback(session_id='A',deleted_ids=['unknown']*(adapter.MAX_SEEN+1),reason='prune')
        self.assertEqual(len(core.seen),0)

    def test_reader_rejection_classes_preserve_native_result(self):
        core = self.register()
        for reason in ("missing", "invalid", "oversized", "fifo", "nofollow", "empty", "changed"):
            self.reader.side_effect = ValueError(reason)
            self.assertIsNone(self.transform(core, session_id="S"))
        self.assertEqual(len(core.seen), 0)
        # These are protocol rejection mocks, not real Rust filesystem tests.

    def test_response_validation_rejects_empty_invalid_oversized_and_wrong_binding(self):
        core = self.register()
        base = self.read_response("", {"schemaVersion": 1, "sourceRoot": str(self.root),
            "sourceIdentity": self.entry["sourceIdentity"],
            "localDataIdentity": adapter._identity((self.root / "local-data").lstat())})
        for change in ({"schemaVersion": True}, {"sourceRoot": "/foreign"}, {"sourceIdentity": "0:0"},
                       {"localDataIdentity": "0:0"}, {"text": ""}, {"text": "  "},
                       {"text": "\0"}, {"text": "\ud800"},
                       {"text": "x" * (adapter.MAX_ADAPTATION + 1)}, {"digest": "0" * 64}):
            self.reader.side_effect = None
            self.reader.return_value = {**base, **change}
            self.assertIsNone(self.transform(core))

    def test_command_native_call_and_real_route(self):
        self.permission = True
        core = self.register()
        self.assertIn("pleat-skill", self.ctx.commands)
        answer = self.ctx.commands["pleat-skill"]("sample do the task", session_id="S", session_key="real-route")
        self.assertEqual(answer, "Pleat skill input accepted for scheduling.")
        self.skill_view.assert_called_once_with("sample", task_id="S", preprocess=False)
        call = self.ctx.inject_message.call_args
        self.assertEqual(call.kwargs, {"role": "user", "session_key": "real-route"})
        for marker in ("NATIVE-MAIN", self.text, "do the task"):
            self.assertIn(marker, call.args[0])
        self.assertEqual(len(core.seen), 1)

    def test_command_permission_revocation_missing_route_and_failed_injection(self):
        self.permission = True
        core = self.register()
        self.assertEqual(core.command("sample", session_id="S"), adapter.UNAVAILABLE)
        self.skill_view.assert_not_called()
        self.permission = False
        self.assertEqual(core.command("sample", session_key="route"), adapter.UNAVAILABLE)
        self.skill_view.assert_not_called()
        self.permission = True
        self.ctx.inject_message.return_value = False
        self.assertEqual(core.command("sample", session_key="route"), adapter.UNAVAILABLE)
        self.assertEqual(len(core.seen), 0)
        self.ctx.inject_message.return_value = True
        core.command("sample", session_key="route")
        self.assertIn(self.text, self.ctx.inject_message.call_args.args[0])

    def test_manual_dedup_preserves_repeated_user_instruction_and_shares_session_scope(self):
        self.permission = True
        core = self.register()
        self.transform(core, session_id="S")
        core.command("sample repeat instruction", session_key="route", session_id="S")
        message = self.ctx.inject_message.call_args.args[0]
        self.assertIn("NATIVE-MAIN", message)
        self.assertIn("repeat instruction", message)
        self.assertNotIn(self.text, message)
        core.command("sample", session_key="other-route")
        self.assertIn(self.text, self.ctx.inject_message.call_args.args[0])

    def test_reference_complete_without_extra_permission_and_without_persistent_dedup(self):
        core = self.register()
        provider = self.ctx.references[0]
        for _ in range(2):
            message = asyncio.run(provider.expand("sample"))
            self.assertIn("NATIVE-MAIN", message)
            self.assertIn(self.text, message)
        self.skill_view.assert_called_with("sample", task_id=None, preprocess=False)
        self.ctx.inject_message.assert_not_called()
        self.assertEqual(len(core.seen), 0)
        self.assertIsNone(asyncio.run(provider.expand("invalid name")))

    def test_native_worker_preserves_contextvars_and_does_not_guess_a_task_id(self):
        key = contextvars.ContextVar("synthetic-profile", default="missing")
        token = key.set("synthetic-profile-A")
        self.addCleanup(key.reset, token)
        seen = []
        self.skill_view.side_effect = lambda name, **kw: (seen.append(key.get()), self.raw)[1]
        self.register()
        asyncio.run(self.ctx.references[0].expand("sample"))
        self.assertEqual(seen, ["synthetic-profile-A"])

    def test_native_timeout_bounds_inflight_work_and_never_injects_late(self):
        self.permission = True
        core = self.register()
        release = threading.Event()
        self.addCleanup(release.set)
        self.skill_view.side_effect = lambda *a, **kw: (release.wait(1), self.raw)[1]
        with mock.patch.object(adapter, "NATIVE_SECONDS", 0.01):
            for _ in range(adapter.MAX_NATIVE_INFLIGHT):
                self.assertEqual(core.command("sample", session_key="route"), adapter.UNAVAILABLE)
            self.assertEqual(core.command("sample", session_key="route"), adapter.UNAVAILABLE)
        self.assertEqual(self.skill_view.call_count, adapter.MAX_NATIVE_INFLIGHT)
        self.ctx.inject_message.assert_not_called()
        release.set()
        self.assertTrue(core.native_slot.acquire(timeout=1))
        core.native_slot.release()
        self.ctx.inject_message.assert_not_called()

    def test_native_consumer_gather_keeps_two_skills_and_two_session_contexts(self):
        second=self.fixture("concurrent-second")
        alias=self.alias("concurrent-second-install",second)
        self.native_results['second']=self.result(alias,name='second',content='SECOND-NATIVE-MAIN')
        self.register({**self.config,'skills':[self.entry,self.descriptor(alias,second,'second')]})
        provider=self.ctx.references[0]
        session=contextvars.ContextVar('owned-native-session',default='one-message')
        seen=[]
        def native(name,**kwargs):
            seen.append((name,session.get()));time.sleep(0.02);return self.native_results[name]
        self.skill_view.side_effect=native
        async def gather(sessions=False):
            async def one(name,label):
                token=session.set(label)
                try:return await provider.expand(name)
                finally:session.reset(token)
            if sessions:return await asyncio.gather(one('sample','A'),one('second','B'))
            return await asyncio.gather(provider.expand('sample'),provider.expand('second'))
        for separate_sessions in [False,True]:
            messages=asyncio.run(gather(separate_sessions))
            self.assertIn('NATIVE-MAIN',messages[0]);self.assertIn('SECOND-NATIVE-MAIN',messages[1])
            self.assertTrue(all(self.text in message for message in messages))
        self.assertCountEqual(seen,[('sample','one-message'),('second','one-message'),('sample','A'),('second','B')])
        self.ctx.inject_message.assert_not_called()

    def test_full_native_sixteen_reference_budget_is_supported(self):
        entries=[];expected={}
        for index in range(16):
            name='parallel-'+str(index);root=self.fixture(name);alias=self.alias(name+'-install',root)
            entries.append(self.descriptor(alias,root,name))
            expected[name]='MAIN-OWNED-'+str(index);self.native_results[name]=self.result(alias,name=name,content=expected[name])
        self.register({**self.config,'skills':entries});provider=self.ctx.references[0]
        def native(name,**kwargs):
            time.sleep(0.02);return self.native_results[name]
        self.skill_view.side_effect=native
        async def gather():return await asyncio.gather(*(provider.expand(name) for name in expected))
        messages=asyncio.run(gather())
        self.assertEqual(len(messages),16)
        for message,body in zip(messages,expected.values()):
            self.assertIsNotNone(message);self.assertIn(body,message);self.assertIn(self.text,message)
        self.assertEqual(self.skill_view.call_count,16);self.ctx.inject_message.assert_not_called()

    def test_callable_snapshot_fresh_per_hook_uninstall_does_not_disable_other_skill(self):
        second = self.fixture("second")
        second_alias = self.alias("second-install", second)
        entry2 = self.descriptor(second_alias, second, "second")
        config = {**self.config, "skills": [self.entry, entry2]}
        provider = mock.Mock(side_effect=lambda: config)
        core = self.register(provider)
        self.assertEqual(provider.call_count, 0)
        self.assertIsInstance(self.transform(core), str)
        self.assertEqual(provider.call_count, 1)
        config["skills"] = [entry2]
        self.assertIsNone(self.transform(core))
        self.assertEqual(provider.call_count, 2)
        self.assertIsInstance(self.transform(core, self.result(second_alias)), str)
        self.assertEqual(provider.call_count, 3)

    def test_callable_snapshot_once_per_manual_reference_and_autocomplete(self):
        self.permission = True
        provider = mock.Mock(side_effect=lambda: self.config)
        self.register(provider)
        self.ctx.commands["pleat-skill"]("sample", session_key="route")
        self.assertEqual(provider.call_count, 1)
        ref = self.ctx.references[0]
        asyncio.run(ref.expand("sample"))
        self.assertEqual(provider.call_count, 2)
        asyncio.run(ref.autocomplete(""))
        self.assertEqual(provider.call_count, 3)

    def test_config_provider_failure_is_closed_for_every_entrypoint(self):
        self.permission = True
        provider = mock.Mock(side_effect=ValueError("synthetic loader refusal"))
        core = self.register(provider)
        self.assertIsNone(self.transform(core))
        self.assertEqual(core.command("sample", session_key="route"), adapter.UNAVAILABLE)
        self.assertIsNone(asyncio.run(self.ctx.references[0].expand("sample")))
        self.assertEqual(asyncio.run(self.ctx.references[0].autocomplete("")), [])
        self.reader.assert_not_called()
        self.skill_view.assert_not_called()

    def test_static_snapshot_copies_input_and_dynamic_snapshot_is_not_reread_inflight(self):
        core = self.register()
        self.entry["sourceIdentity"] = "0:0"
        self.assertIsInstance(self.transform(core), str)
        config = copy.deepcopy(core.config)
        provider = mock.Mock(return_value=config)
        dynamic = self.register(provider)
        def remove_descriptor(executable, request):
            config["skills"].clear()
            return self.read_response(executable, request)
        self.reader.side_effect = remove_descriptor
        self.assertIsInstance(self.transform(dynamic), str)
        self.assertEqual(provider.call_count, 1)
        self.assertIsNone(self.transform(dynamic))

    def test_entry_reader_path_precedes_shared_reader_and_can_stand_alone(self):
        self.entry["readerExecutable"] = str(self.base / "historical-reader")
        self.assertIsInstance(self.transform(self.register()), str)
        self.assertEqual(self.reader.call_args.args[0], self.entry["readerExecutable"])
        del self.config["readerExecutable"]
        self.assertIsInstance(self.transform(self.register()), str)
        self.assertEqual(self.reader.call_args.args[0], self.entry["readerExecutable"])

    def test_config_bounds_conflicts_and_reader_validation(self):
        for config in ({}, {**self.config, "schemaVersion": True}, {**self.config, "unknown": 1},
                       {**self.config, "readerExecutable": "relative"},
                       {**self.config, "skills": [self.entry] * (adapter.MAX_ENTRIES + 1)},
                       {**self.config, "skills": [self.entry, {**self.entry, "nativeName": "conflict"}]},
                       {**self.config, "skills": [{**self.entry, "readerExecutable": None}]}):
            with self.subTest(keys=list(config)):
                with self.assertRaises(ValueError):
                    self.register(config)
        large = {**self.config, "skills": [{**self.entry,
            "installationSkillPath": "/" + "x" * 4000 + f"/{n}/SKILL.md"}
            for n in range(adapter.MAX_ENTRIES)]}
        self.assertGreater(len(json.dumps(large).encode("utf-8")), adapter.MAX_CONFIG)
        with self.assertRaises(ValueError):
            self.register(large)

    def test_real_4096_entry_limit_including_more_than_256_normal_entries(self):
        self.assertEqual(adapter.MAX_ENTRIES, 4096)
        self.assertEqual(adapter.MAX_CONFIG, 8 * 1024 * 1024)
        entries = [self.entry] + [{**self.entry,
            "installationSkillPath": str(self.base / f"not-this-winner-{n}" / "SKILL.md"),
            "sourceIdentity": f"0:{n}"} for n in range(adapter.MAX_ENTRIES - 1)]
        config = {**self.config, "skills": entries}
        self.assertLessEqual(len(json.dumps(config).encode("utf-8")), adapter.MAX_CONFIG)
        self.assertIsInstance(self.transform(self.register(config)), str)
        self.assertEqual(self.reader.call_count, 1)

    def test_links_cycle_steps_and_time_are_bounded(self):
        cycle = self.base / "cycle"
        cycle.symlink_to(cycle)
        with self.assertRaises(ValueError):
            adapter._chain(str(cycle), adapter.time.monotonic() + 1)
        with self.assertRaises(ValueError):
            adapter._chain(str(self.root), adapter.time.monotonic() - 1)

    def test_symlink_local_data_and_symlink_skill_are_refused(self):
        core = self.register()
        (self.root / "local-data").rmdir()
        (self.root / "local-data").symlink_to(self.base)
        self.assertIsNone(self.transform(core))
        self.reader.assert_not_called()
        (self.root / "local-data").unlink()
        (self.root / "local-data").mkdir()
        (self.root / "SKILL.md").rename(self.root / "held-skill")
        (self.root / "SKILL.md").symlink_to(self.root / "held-skill")
        self.assertIsNone(self.transform(core))

    def test_dispatcher_routes_a_b_a_by_official_profile_and_directory_identity(self):
        profile_b = self.base / "profile-B"
        profile_b.mkdir()
        root_b = self.fixture("source-B")
        install_b = self.alias("installation-B", root_b)
        config_a = {**self.config, "skills": [{**self.entry, "nativeName": "completion-A"}]}
        config_b = {**self.config, "skills": [self.descriptor(install_b, root_b, "completion-B")]}
        loader_a, loader_b = mock.Mock(return_value=config_a), mock.Mock(return_value=config_b)
        core_a = self.register(loader_a)
        ctx_b, core_b = self.register_profile(profile_b, loader_b)
        self.assertEqual((core_a.reference_status, core_b.reference_status), ("active", "active"))
        self.assertEqual(len(self.ctx.references), 1)
        self.assertEqual(ctx_b.references, [])  # No second public prefix registration attempt.
        provider = self.registry["pleat-skill"]
        self.skill_view.side_effect = lambda name, **kw: self.result(
            self.install if self.home.get() == self.profile_a else install_b,
            content="MAIN-A" if self.home.get() == self.profile_a else "MAIN-B")
        def read(executable, request):
            text = "ADAP-A" if request["sourceRoot"] == str(self.root) else "ADAP-B"
            return {**request, "text": text, "digest": hashlib.sha256(text.encode()).hexdigest()}
        self.reader.side_effect = read
        for home, expected, foreign in ((self.profile_a, "A", "B"), (profile_b, "B", "A"),
                                         (self.profile_a, "A", "B")):
            token = self.home.set(home)
            try:
                message = asyncio.run(provider.expand("same-native-name"))
                self.assertIn(f"MAIN-{expected}", message)
                self.assertIn(f"ADAP-{expected}", message)
                self.assertNotIn(f"ADAP-{foreign}", message)
            finally:
                self.home.reset(token)
        self.assertEqual((loader_a.call_count, loader_b.call_count), (2, 1))
        for home, expected in ((self.profile_a, "completion-A"), (profile_b, "completion-B")):
            token = self.home.set(home)
            try:
                items = asyncio.run(provider.autocomplete(""))
                self.assertEqual([item.text for item in items], [expected])
            finally:
                self.home.reset(token)

    def test_foreign_prefix_retained_even_with_matching_owner_attributes(self):
        class Foreign(Reference):
            prefix = "pleat-skill"
            plugin_id, protocol = Context.plugin_id, 1
            async def autocomplete(self, query, *, limit=10):
                return [Completion("foreign")]
            async def expand(self, target):
                return "FOREIGN-CONTENT"
        foreign = Foreign()
        self.registry["pleat-skill"] = foreign
        core = self.register()
        self.assertEqual(core.reference_status, "foreign_prefix")
        self.assertIs(self.registry["pleat-skill"], foreign)
        self.assertEqual(self.ctx.references, [])
        self.assertEqual(asyncio.run(foreign.expand("anything")), "FOREIGN-CONTENT")
        core.close()
        self.assertIs(self.registry["pleat-skill"], foreign)
        self.reader.assert_not_called()

    def test_reload_in_different_module_reuses_provider_and_old_unload_keeps_new_owner(self):
        old = self.register()
        provider = self.registry["pleat-skill"]
        spec = importlib.util.spec_from_file_location("synthetic_profile_B_reload_adapter", ASSET)
        reloaded = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(reloaded)  # Another copy of our asset, no target module imports.
        patch = mock.patch.object(reloaded, "_reader", self.reader)
        patch.start()
        self.addCleanup(patch.stop)
        next_config = {**self.config, "skills": [{**self.entry, "nativeName": "new-owner-name"}]}
        ctx_new, newer = self.register_profile(self.profile_a, next_config, reloaded)
        self.assertEqual(newer.reference_status, "active")
        self.assertEqual(ctx_new.references, [])
        self.assertIs(self.registry["pleat-skill"], provider)
        old.close()
        old.close()  # Repeated old-owner unload must also be harmless.
        self.assertFalse(newer.closed)
        self.assertEqual([i.text for i in asyncio.run(provider.autocomplete(""))], ["new-owner-name"])
        self.assertIn(self.text, asyncio.run(provider.expand("sample")))
        newer.close()
        self.assertIsNone(asyncio.run(provider.expand("sample")))
        self.assertIs(self.registry["pleat-skill"], provider)  # No native unregister or replacement.

    def test_all_owners_can_unload_and_same_profile_can_register_again(self):
        old = self.register()
        provider = self.registry["pleat-skill"]
        old.close()
        self.assertEqual(asyncio.run(provider.autocomplete("")), [])
        ctx, newer = self.register_profile(self.profile_a, self.config)
        self.assertEqual(newer.reference_status, "active")
        self.assertEqual(ctx.references, [])
        self.assertIn(self.text, asyncio.run(provider.expand("sample")))

    def test_unregistered_profile_never_falls_back_to_only_registered_owner(self):
        core = self.register()
        unknown = self.base / "unregistered-profile"
        unknown.mkdir()
        self.home.set(unknown)
        provider = self.registry["pleat-skill"]
        self.assertIsNone(asyncio.run(provider.expand("sample")))
        self.assertEqual(asyncio.run(provider.autocomplete("")), [])
        self.assertFalse(core.closed)
        self.skill_view.assert_not_called()
        self.reader.assert_not_called()

    def test_replaced_profile_directory_needs_new_owner_and_old_unload_cannot_remove_it(self):
        loader = mock.Mock(return_value=self.config)
        old = self.register(loader)
        provider = self.registry["pleat-skill"]
        self.profile_a.rename(self.base / "retained-old-profile")
        self.profile_a.mkdir()
        self.assertIsNone(asyncio.run(provider.expand("sample")))
        loader.assert_not_called()
        self.skill_view.assert_not_called()
        _, newer = self.register_profile(self.profile_a, self.config)
        old.close()
        self.assertFalse(newer.closed)
        self.assertIn(self.text, asyncio.run(provider.expand("sample")))
        bus = sys.modules[adapter._REFERENCE_BUS]
        self.assertEqual(len(bus.profiles), 1)  # The old directory identity was pruned.

    def test_official_profile_compat_alias_routes_to_same_directory_identity(self):
        self.register()
        alias = self.alias("profile-home-alias", self.profile_a)
        self.home.set(alias)
        self.assertIn(self.text, asyncio.run(self.registry["pleat-skill"].expand("sample")))

    def test_profile_change_inside_loader_rejects_before_native_lookup(self):
        other = self.base / "other-profile"
        other.mkdir()
        def loader():
            self.home.set(other)
            return self.config
        self.register(loader)
        self.assertIsNone(asyncio.run(self.registry["pleat-skill"].expand("sample")))
        self.skill_view.assert_not_called()
        self.assertEqual(self.home.get(), self.profile_a)  # Worker context did not alter caller context.

    def test_owner_replaced_during_reference_read_discards_stale_result(self):
        self.register()
        provider = self.registry["pleat-skill"]
        new_owners = []
        def replace_owner(executable, request):
            ctx = Context(self.registry)
            newer = adapter.register(ctx, self.config)
            new_owners.append(newer)
            return self.read_response(executable, request)
        self.reader.side_effect = replace_owner
        self.assertIsNone(asyncio.run(provider.expand("sample")))
        self.assertEqual(len(new_owners), 1)
        self.addCleanup(new_owners[0].close)
        self.reader.side_effect = self.read_response
        self.assertIn(self.text, asyncio.run(provider.expand("sample")))

    def test_profile_table_real_bound_does_not_evict_active_profiles(self):
        self.register()
        provider = self.registry["pleat-skill"]
        for number in range(1, adapter.MAX_PROFILES):
            home = self.base / f"bounded-profile-{number}"
            home.mkdir()
            _, core = self.register_profile(home, self.config)
            self.assertEqual(core.reference_status, "active")
        overflow = self.base / "profile-capacity-overflow"
        overflow.mkdir()
        ctx, refused = self.register_profile(overflow, self.config)
        self.assertEqual(refused.reference_status, "profile_capacity")
        self.assertEqual(ctx.references, [])
        self.assertEqual(len(sys.modules[adapter._REFERENCE_BUS].profiles), adapter.MAX_PROFILES)
        self.assertIn(self.text, asyncio.run(provider.expand("sample")))
        _, reloaded = self.register_profile(self.profile_a, self.config)
        self.assertEqual(reloaded.reference_status, "active")

    def test_public_registration_refusal_is_observed_without_patching_registry(self):
        self.ctx.register_context_reference = mock.Mock(return_value=None)
        core = self.register()
        self.assertEqual(core.reference_status, "registration_refused")
        self.assertEqual(self.registry, {})
        self.assertEqual(len(sys.modules[adapter._REFERENCE_BUS].profiles), 0)

    def test_unload_during_public_registration_cannot_publish_a_closed_owner(self):
        original = self.ctx.register_context_reference
        def register_and_unload(provider):
            original(provider)
            self.ctx.unloaders[-1]()
        self.ctx.register_context_reference = mock.Mock(side_effect=register_and_unload)
        core = self.register()
        self.assertTrue(core.closed)
        self.assertEqual(core.reference_status, "unavailable")
        self.assertEqual(len(sys.modules[adapter._REFERENCE_BUS].profiles), 0)
        self.assertIsNone(asyncio.run(self.registry["pleat-skill"].expand("sample")))

    def test_foreign_bus_namespace_is_not_overwritten(self):
        foreign = types.ModuleType(adapter._REFERENCE_BUS)
        sys.modules[adapter._REFERENCE_BUS] = foreign
        core = self.register()
        self.assertEqual(core.reference_status, "unavailable")
        self.assertIs(sys.modules[adapter._REFERENCE_BUS], foreign)
        self.assertEqual(self.registry, {})


class ReaderTransportTests(unittest.TestCase):
    """Synthetic pipes/selectors only: subprocess.Popen is never allowed to run."""
    def reader(self, chunks, returncode=0, timeout=False, live=False):
        process = mock.MagicMock()
        process.__enter__.return_value = process
        process.stdin.fileno.return_value, process.stdout.fileno.return_value = 10, 11
        process.wait.return_value, process.poll.return_value = returncode, None if live else 0
        selector = mock.MagicMock()
        selector.__enter__.return_value = selector
        write = types.SimpleNamespace(fileobj=process.stdin, fd=10)
        read = types.SimpleNamespace(fileobj=process.stdout, fd=11)
        selector.get_map.side_effect = [True] * (len(chunks) + 1) + [False]
        selector.select.side_effect = [[(write, 0)]] + [[(read, 0)] for _ in chunks]
        request = {"schemaVersion": 1, "sourceRoot": "/synthetic", "sourceIdentity": "1:2",
                   "localDataIdentity": "1:3"}
        with mock.patch.object(adapter.subprocess, "Popen", return_value=process) as popen, \
             mock.patch.object(adapter.selectors, "DefaultSelector", return_value=selector), \
             mock.patch.object(adapter.os, "set_blocking"), \
             mock.patch.object(adapter.os, "write", side_effect=lambda fd, data: len(data)), \
             mock.patch.object(adapter.os, "read", side_effect=chunks), \
             mock.patch.object(adapter, "READER_SECONDS", -1 if timeout else 3):
            try:
                value = adapter._reader("/synthetic-reader", request)
            finally:
                if popen.called:
                    args, kwargs = popen.call_args
                    self.assertEqual(args[0], ["/synthetic-reader", adapter.FLAG])
                    # Inspect names only; never expand or assert on inherited credential objects.
                    self.assertLessEqual(set(kwargs["env"]), {"PATH", "HOME", "LANG"})
                    self.assertFalse(kwargs["shell"])
                    self.assertEqual(kwargs["stderr"], subprocess.DEVNULL)
                    if live:
                        process.kill.assert_called_once_with()
            return value

    def test_bounded_reader_transport_and_environment(self):
        self.assertEqual(self.reader([b'{"schemaVersion":1}', b""]), {"schemaVersion": 1})

    def test_reader_nonzero_exit_invalid_utf8_json_oversize_and_timeout(self):
        for chunks, kwargs in (([b"{}", b""], {"returncode": 1}),
                               ([b"\xff", b""], {}), ([b"broken", b""], {}),
                               ([b"x" * 4096] * (adapter.MAX_RESPONSE // 4096 + 1), {"live": True}),
                               ([b"", b""], {"timeout": True, "live": True})):
            with self.subTest(options=list(kwargs)):
                with self.assertRaises((ValueError, UnicodeError)):
                    self.reader(chunks, **kwargs)

    def test_request_size_bound_prevents_subprocess(self):
        with mock.patch.object(adapter.subprocess, "Popen") as popen:
            with self.assertRaises(ValueError):
                adapter._reader("/synthetic-reader", {"oversized": "x" * (16 * 1024 + 1)})
            popen.assert_not_called()


if __name__ == "__main__":
    unittest.main(verbosity=2)
