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

    def test_blocks_a_piped_check_that_continues_on_success_of_the_filter(self):
        for command in [
            "cargo xtask gate fast --remote 2>&1 | tail -18 && cargo xtask ship",
            "cargo xtask lint-docs 2>&1 | tail -3; git add -A && git commit -m x",
            "cargo xtask lint-docs 2>&1 | tail -3 && git commit -m x",
            "cd /x && cargo xtask verify 2>&1 | grep stale && echo fine",
        ]:
            with self.subTest(command=command):
                self.assertEqual(denied(command), "&&" in command.split("|", 1)[1].split(";")[0], command)

    def test_blocks_a_check_whose_failure_a_semicolon_ignores(self):
        for command in [
            'cargo run -q -p xtask -- lint-docs; echo "docs $?"; git add benchmarks && git commit -q -m x',
            "cargo xtask gate fast > log 2>&1; cargo xtask ship",
            "cargo xtask lint-commits; git push origin HEAD",
        ]:
            with self.subTest(command=command):
                self.assertTrue(denied(command))

    def test_allows_a_check_that_guards_what_follows(self):
        for command in [
            "if cargo run -q -p xtask -- lint-docs; then git add docs && git commit -m x; fi",
            "cargo xtask lint-docs && git commit -m x",
            "cargo xtask gate fast > log 2>&1; tail -3 log",
            'cargo run -q -p xtask -- lint-docs; echo "docs $?"',
        ]:
            with self.subTest(command=command):
                self.assertFalse(denied(command))

    def test_blocks_a_loop_that_sleeps_until_work_finishes(self):
        for command in [
            "until grep -q '^rc=' /tmp/x/sync.out; do sleep 20; done; cat /tmp/x/sync.out",
            'while ! grep -q "^exit" /tmp/x/verify.log; do sleep 10; done',
            'while pgrep -f "plants.sh" >/dev/null; do sleep 15; done; tail -5 log',
            "zsh -c 'until test -f done.txt; do\n  sleep 5\ndone'",
        ]:
            with self.subTest(command=command):
                self.assertTrue(denied(command))

    def test_allows_loops_that_do_not_sleep(self):
        for command in [
            "for try in 1 2 3; do cargo xtask ship > log 2>&1 && break; done",
            "while read -r p; do kill $p; done < pids.txt",
            "for f in *.json; do python3 -c 'import json' $f; done",
            "zsh scripts/wait_for_quiet.sh; sleep 1",
        ]:
            with self.subTest(command=command):
                self.assertFalse(denied(command))

    def test_allows_checks_whose_status_survives(self):
        for command in [
            "cargo xtask gate fast > log 2>&1; tail -3 log",
            "if cargo xtask gate fast > log 2>&1; then cargo xtask ship; fi",
            "set -o pipefail; cargo xtask gate fast | tail -3 && cargo xtask ship",
            "cargo xtask lint-docs 2>&1 | tail -3",
            "cargo build -q 2>&1 | tail -2 && cp a b",
        ]:
            with self.subTest(command=command):
                self.assertFalse(denied(command))

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
