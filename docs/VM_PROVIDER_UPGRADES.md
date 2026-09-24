# Existing VM provider bridge upgrades

A 0.58.0 standalone upgrade on linux-client left the existing guest's provider-cli.py
unchanged. The new server sent max_steps=0 (unlimited), while the old bridge
asserted max_steps>0 before launching Claude. Account authentication still worked,
and the generic provider error incorrectly appeared to be a connection failure.

The affected guest was repaired using the provider-cli.py already bundled with
its installed 0.58.0 server, after backing up the previous file. The installed
hash matched the bundle. Scratch's existing fifth attempt completed with a tool
result and assistant response, without a VM restart or credential changes.

VM ensure now synchronizes this Kindred-owned bridge from the installed server
bundle before reporting the guest ready for work. It validates Python syntax and
SHA-256, keeps a content-addressed backup, replaces atomically, and does nothing
when hashes already match. It does not reinstall provider packages, reset the VM,
change authentication, or replay failed tasks. A failed synchronization prevents
startup with an actionable error. Read-only connection-status remains read-only.

Regression coverage: deploy/test_vm_manager.py checks synchronization on existing
ready/runtime-failed guests, idempotence, backup preservation, unchanged unrelated
files, and rejection of malformed or corrupt source before replacement.

This source change requires the next server package. The one-time repair above
restored Scratch; it is not a release deployment.


## Local-access catalog overflow

Atlas still failed after the bridge upgrade: local access adds tools to the
standard catalog, exceeding the bridge's historical 64 registered-tool limit.
A sanitized traceback confirmed the assertion fired before Claude started.
Scratch had local access disabled and did not encounter this second defect.

Remove the catalog count cap in both the Claude/Kimi bridge and Pi. Nonempty,
unique tool catalogs, transport frame limits, exact tool exposure and execution
permissions remain enforced. This is separate from the already-unlimited
default tool-call budget. Regression tests exercise 80 registered tools with
max_steps=0, including a declined call through both subscription transports.

The installed linux-client guest received the same narrow catalog-validation repair
with a clean pre-change backup. Temporary traceback instrumentation was removed.
Atlas's previous run had exhausted its retries before that repair landed. After
the owner selected Continue task, run 4578ab1d-4fd7-476a-b285-ff27397f72f1
discovered connectors, selected its model, emitted an assistant response and
completed without an error. Local-access permissions remained unchanged.
