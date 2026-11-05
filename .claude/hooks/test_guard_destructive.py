#!/usr/bin/env python3
"""Cases for guard_destructive.py; `cargo xtask gate` runs this file."""
import json
import os
import subprocess
import sys
import unittest

HOOK = os.path.join(os.path.dirname(os.path.abspath(__file__)), "guard_destructive.py")
SCRATCH = "/private/tmp/claude-501/case"
REPO_TARGET = os.path.join(os.path.dirname(os.path.dirname(os.path.dirname(HOOK))), "target")


def denied(command, cwd=SCRATCH):
    payload = json.dumps({"tool_input": {"command": command}, "cwd": cwd})
    result = subprocess.run([sys.executable, HOOK], input=payload, capture_output=True, text=True, check=True)
    return '"deny"' in result.stdout


class Guard(unittest.TestCase):
    def test_blocks(self):
        for command in [
            "rm -rf /",
            "rm -rf ~/build",
            "rm -f README.md",
            'rm -rf "$DIR/x"',
            "rm -rf ${DIR:?}/x",
            "rm -rf build",
            "bash -c 'rm -rf /etc/x'",
            "find . -delete",
            "git clean -fd",
            "git worktree remove --force wt",
            "pkill -f cbmc",
            "pkill kani-driver",
            "killall cargo",
            "sh -c 'pkill -f cargo-kani'",
        ]:
            with self.subTest(command=command):
                self.assertTrue(denied(command, cwd=os.path.dirname(REPO_TARGET)))

    def test_blocks_relative_targets_after_an_unknown_cd(self):
        for command in ['cd "$P" && rm -rf build', "cd ${P:?} && rm -rf build", "cd && rm -rf build",
                        "cd - && rm -rf build"]:
            with self.subTest(command=command):
                self.assertTrue(denied(command))

    def test_allows(self):
        for command in [
            "rm -rf build",
            "rm -rf /private/tmp/claude-501/x",
            f"rm -rf {REPO_TARGET}/bench-work",
            "D=/private/tmp/claude-501/y; rm -rf ${D:?}/z",
            "D=/private/tmp/claude-501/y; cd \"${D:?}\" && rm -rf z",
            'cd "$P" && rm -rf /private/tmp/claude-501/q',
            'cd "$P" && ls',
            "rm notes.txt",
            "git clean -n",
            "kill 4242",
            "pkill -f /private/tmp/claude-501/m22-kani/target/kani",
            "pkill -f /Users/aleph/Desktop/Projects/marlboro-red/zipper/.claude/worktrees/agent-x/target/kani",
        ]:
            with self.subTest(command=command):
                self.assertFalse(denied(command))


if __name__ == "__main__":
    unittest.main()
