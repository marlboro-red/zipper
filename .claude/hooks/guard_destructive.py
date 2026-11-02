#!/usr/bin/env python3
"""PreToolUse guard for Bash: refuse destructive deletions outside scratch space.

Blocks:
- `rm` with -r, -R, -f, --recursive or --force unless every target lies strictly
  inside /private/tmp/claude-501/ (also spelled /tmp/claude-501/) or the
  repository's target/ directory;
- such an `rm` whose target uses a variable without a `${NAME:?}` guard, or a
  variable whose value is not assigned literally earlier in the same command;
- `git worktree remove --force` and `git clean -f`.
"""
import json
import os
import re
import shlex
import sys

REPO = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
ALLOWED_ROOTS = ["/private/tmp/claude-501", "/tmp/claude-501", os.path.join(REPO, "target")]
SEPARATORS = {";", "&&", "||", "|", "&", "(", ")", "\n"}
SHELLS = {"sh", "bash", "zsh"}
WRAPPERS = {"sudo", "command", "env", "nice", "time", "xargs", "nohup"}
GUARDED = re.compile(r"\$\{([A-Za-z_][A-Za-z0-9_]*):\?[^}]*\}")
ANY_VARIABLE = re.compile(r"\$(\{[^}]*\}|[A-Za-z_][A-Za-z0-9_]*|\(|[0-9@*#?$!-])")


def deny(reason):
    print(json.dumps({"hookSpecificOutput": {
        "hookEventName": "PreToolUse",
        "permissionDecision": "deny",
        "permissionDecisionReason": "Blocked by .claude/hooks/guard_destructive.py: " + reason,
    }}))
    sys.exit(0)


def tokens(command):
    lexer = shlex.shlex(command, posix=True, punctuation_chars=";&|()")
    lexer.whitespace_split = True
    lexer.commenters = ""
    return list(lexer)


def segments(words):
    current = []
    for word in words:
        if word in SEPARATORS or set(word) <= set(";&|()"):
            if current:
                yield current
            current = []
        else:
            current.append(word)
    if current:
        yield current


def inside_allowed(path):
    normal = os.path.normpath(path)
    return any(normal.startswith(root + os.sep) for root in ALLOWED_ROOTS)


def resolve(target, assignments, cwd):
    def substitute(match):
        name = match.group(1)
        if name not in assignments:
            raise ValueError(f"`{target}` uses ${{{name}:?}}, but {name} is not assigned a literal path earlier in this command")
        return assignments[name]
    expanded = GUARDED.sub(substitute, target)
    if ANY_VARIABLE.search(expanded) or "`" in expanded:
        raise ValueError(f"`{target}` expands a variable without a ${{NAME:?}} guard, so an empty value could widen the deletion")
    expanded = os.path.expanduser(expanded)
    return expanded if os.path.isabs(expanded) else os.path.join(cwd, expanded)


def check_rm(arguments, assignments, cwd):
    flags = [a for a in arguments if a.startswith("-") and a != "--"]
    destructive = any(a in ("--recursive", "--force") or
                      (not a.startswith("--") and set(a[1:]) & set("rRf")) for a in flags)
    if not destructive:
        return
    targets, after_dashes = [], False
    for argument in arguments:
        if argument == "--" and not after_dashes:
            after_dashes = True
        elif after_dashes or not argument.startswith("-"):
            targets.append(argument)
    if not targets:
        deny("`rm` with -r or -f and no target")
    for target in targets:
        try:
            path = resolve(target, assignments, cwd)
        except ValueError as error:
            deny(str(error))
        if not inside_allowed(path):
            deny(f"`rm -r/-f` target `{target}` resolves to `{os.path.normpath(path)}`, which is outside "
                 f"the scratch directory /private/tmp/claude-501/ and {REPO}/target/. "
                 "Use a fresh `mktemp -d` folder under the scratch directory, or `git checkout`/`git clean -n` for repo files.")


def check(command, cwd, depth=0):
    try:
        words = tokens(command)
    except ValueError:
        if re.search(r"\brm\b|git\s+(clean|worktree)", command):
            deny("the command could not be parsed, and it mentions rm, git clean or git worktree")
        return
    assignments = {}
    for segment in segments(words):
        while segment and re.fullmatch(r"[A-Za-z_][A-Za-z0-9_]*=.*", segment[0]):
            name, value = segment[0].split("=", 1)
            if not ANY_VARIABLE.search(value) and "`" not in value:
                assignments[name] = value
            segment = segment[1:]
        while segment and os.path.basename(segment[0]) in WRAPPERS:
            segment = segment[1:]
            while segment and (segment[0].startswith("-") or "=" in segment[0]):
                segment = segment[1:]
        if not segment:
            continue
        program = os.path.basename(segment[0])
        arguments = segment[1:]
        if program == "cd" and arguments:
            try:
                cwd = resolve(arguments[0], assignments, cwd)
            except ValueError:
                pass
        elif program == "rm":
            check_rm(arguments, assignments, cwd)
        elif program == "git" and "clean" in arguments and any(
                a.startswith("-") and not a.startswith("--") and "f" in a or a == "--force" for a in arguments):
            deny("`git clean -f` deletes untracked files. Run `git clean -n` to list them and remove each one deliberately.")
        elif program == "git" and "worktree" in arguments and "remove" in arguments and (
                "--force" in arguments or "-f" in arguments):
            deny("`git worktree remove --force` discards uncommitted work in the worktree. Remove it without --force.")
        elif program in SHELLS and "-c" in arguments and depth < 3:
            index = arguments.index("-c")
            if index + 1 < len(arguments):
                check(arguments[index + 1], cwd, depth + 1)
        elif program == "find" and ("-delete" in arguments or "rm" in arguments):
            roots = [a for a in arguments if not a.startswith("-") and not a.startswith("(")][:1] or ["."]
            for root in roots:
                try:
                    path = resolve(root, assignments, cwd)
                except ValueError as error:
                    deny(str(error))
                if not inside_allowed(path):
                    deny(f"`find {root} -delete` deletes outside the scratch directory and target/")


def main():
    payload = json.load(sys.stdin)
    command = payload.get("tool_input", {}).get("command", "")
    try:
        check(command, payload.get("cwd") or os.getcwd())
    except Exception as error:
        if re.search(r"\brm\b|git\s+(clean|worktree)|-delete", command):
            deny(f"the guard failed ({error!r}) on a command that may delete files")


if __name__ == "__main__":
    main()
