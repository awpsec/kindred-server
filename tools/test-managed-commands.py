#!/usr/bin/env python3
"""Exercise the real detached worker via guest RPC, using only temporary files.

Usage: python3 tools/test-managed-commands.py /path/to/debug/kindred [--long]
--long also proves a command survives the old 60-second foreground cutoff.
"""
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time
import uuid

binary = str(Path(sys.argv[1]).resolve())
with tempfile.TemporaryDirectory(prefix="kindred-command-test-") as folder:
    root = Path(folder)
    env = dict(os.environ, HOME=folder, KINDRED_GUEST="1")
    def rpc(tool, args):
        p = subprocess.run([binary, "guest-rpc"], input=json.dumps(dict(tool=tool, args=args)), text=True, capture_output=True, env=env, timeout=8)
        assert p.returncode == 0, p.stderr
        v = json.loads(p.stdout)
        assert "error" not in v, v
        return v
    def start(command, seconds=30):
        key = str(uuid.uuid4())
        args = dict(process_id=key, command=command, path=folder, max_seconds=seconds)
        result = rpc("command_start", args)
        assert result["id"] == key
        return key, args
    def read(key, stop=False):
        return rpc("command_poll", dict(commands=[dict(id=key, stop=stop)]))["commands"][0]
    def finish(key, limit=15):
        end = time.monotonic() + limit
        while time.monotonic() < end:
            v = read(key)
            if v["status"] not in ("starting", "running"):
                return v
            time.sleep(.2)
        raise AssertionError(f"worker {key} did not finish")
    key, args = start("echo launch >> launches; echo phase-one; sleep 3; echo phase-two")
    time.sleep(1.4)
    assert "phase-one" in read(key)["text"]
    # New RPC process, same ID, including a different command: must never replay.
    args["command"] = "echo duplicate >> launches"
    rpc("command_start", args)
    result = finish(key)
    assert result["status"] == "completed" and result["exit_code"] == 0, result
    assert "phase-two" in result["text"]
    assert (root / "launches").read_text().splitlines() == ["launch"]
    print("PASS detached lifetime, incremental output, final receipt, duplicate start")
    key, _ = start("echo partial; sleep 30; echo BAD > should-not-exist")
    time.sleep(.4)
    read(key, True)
    assert finish(key)["status"] == "cancelled"
    assert not (root / "should-not-exist").exists()
    key, _ = start("sleep 30\n" + "# padding for a full stdin pipe\n" * 900)
    time.sleep(.4)
    read(key, True)
    assert finish(key)["status"] == "cancelled"
    key, _ = start("echo partial; sleep 30", 1)
    assert finish(key)["status"] == "timed_out"
    key, _ = start("echo deliberate-failure >&2; exit 7")
    result = finish(key)
    assert result["status"] == "failed" and result["exit_code"] == 7
    assert "deliberate-failure" in result["text"]
    print("PASS cancellation including full stdin pipe, deadline, failed exit and stderr")
    key, _ = start("head -c 100000 /dev/zero | tr '\\0' x; echo TAIL")
    result = finish(key)
    assert result["output_truncated"] and result["text"].endswith("TAIL\n")
    assert len(result["text"].encode()) <= 32768
    assert read(str(uuid.uuid4()))["status"] == "unknown"
    print("PASS bounded tail capture and unknown worker")
    if "--long" in sys.argv:
        key, _ = start("echo begin; sleep 65; echo after-foreground-cutoff", 90)
        result = finish(key, 75)
        assert result["status"] == "completed" and "after-foreground-cutoff" in result["text"]
        print("PASS real 65-second command survives original 60-second limit")
