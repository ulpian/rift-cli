"""Disposable native preview fixtures. Preserve HOME and terminal capabilities."""

import os
from pathlib import Path
import subprocess


def environment(root):
    env = {key: value for key, value in os.environ.items()
           if not key.startswith(("RIFT_", "GIT_"))}
    templates = root / "templates"
    templates.mkdir(exist_ok=True)
    env.update(RIFT_CONFIG_DIR=str(root / "config"), RIFT_CACHE_DIR=str(root / "cache"),
               RIFT_NO_KEYCHAIN="1", GIT_CONFIG_GLOBAL=os.devnull,
               GIT_CONFIG_SYSTEM=os.devnull, GIT_CONFIG_NOSYSTEM="1",
               GIT_TEMPLATE_DIR=str(templates))
    return env


def run(command, env, cwd):
    return subprocess.run([str(part) for part in command], env=env, cwd=cwd,
                          check=True, timeout=120, capture_output=True, text=True)


def project(root, env, name="project", worktree=False):
    repo = root / name
    repo.mkdir()
    run(["git", "init", "--quiet", "--initial-branch=main", repo], env, root)
    (repo / "README.md").write_text("# Disposable Rift preview\n")
    run(["git", "add", "README.md"], env, repo)
    run(["git", "-c", "user.name=Rift Preview", "-c", "user.email=preview@localhost",
         "-c", "commit.gpgsign=false", "commit", "--quiet", "-m", "Preview fixture"], env, repo)
    if worktree:
        run(["git", "worktree", "add", "--quiet", "-b", "feature-demo", root / "feature-demo"], env, repo)
    return repo


def initialize(binary, repo, env):
    policy = repo / ".rift"
    policy.mkdir()
    (policy / "config.toml").write_text(
        'schema_version=1\n[project]\nname="Rift preview"\n'
        '[features]\ntotal_recall=false\n[capture]\nraw_transcripts=false\n'
    )
    run([binary, "init", "--no-recall"], env, repo)
