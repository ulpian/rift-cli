"""Interactive installer, project setup, and Canopy previews in disposable folders."""

import argparse
import os
from pathlib import Path
import subprocess
import tempfile

from preview_install import MANIFEST, ROOT, prepare
from preview_support import environment, initialize, project


def preview(binary, flow):
    if os.name != "posix":
        raise SystemExit("These previews require macOS or Linux.")
    with tempfile.TemporaryDirectory(prefix=f"rift-{flow}-preview-") as temporary:
        root = Path(temporary)
        env = environment(root)
        if flow == "install":
            prepare(root, binary)
            env.update(RIFT_BIN_DIR=str(root / "bin"), RIFT_DOWNLOAD_MANIFEST=MANIFEST,
                       RIFT_PREVIEW_ASSETS=str(root / "assets"),
                       PATH=str(root / "commands") + os.pathsep + env["PATH"])
            command, cwd = ["sh", str(ROOT / "install.sh")], root
            print("Real installer; local download fixture. Choose defaults, then Confirm defaults.", flush=True)
        else:
            cwd = project(root, env, worktree=flow == "dashboard")
            if flow == "dashboard":
                initialize(binary, cwd, env)
                print("Dashboard: Shift-Tab opens Settings; Left opens projects. Ctrl-C exits.", flush=True)
            else:
                print("Fresh project setup: review choices and confirm, or Ctrl-C to cancel.", flush=True)
            command = [str(binary)]
        print("All preview config, caches, projects and downloaded binaries are removed on exit.", flush=True)
        subprocess.run(command, env=env, cwd=cwd, check=True)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("flow", choices=["install", "setup", "dashboard"])
    args = parser.parse_args()
    try:
        preview(args.binary.expanduser().resolve(strict=True), args.flow)
    except KeyboardInterrupt:
        pass
