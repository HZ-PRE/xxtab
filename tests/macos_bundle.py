"""Verify the shipped app after relocation, with no Homebrew in PATH."""
import importlib.util
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile

REPO = Path(__file__).resolve().parent.parent
spec = importlib.util.spec_from_file_location("deps", REPO / "tools/build-macos-deps.py")
deps = importlib.util.module_from_spec(spec)
spec.loader.exec_module(deps)


def main(app, arch, test_binary):
    assert sys.platform == "darwin"
    clean = {"PATH": "/usr/bin:/bin:/usr/sbin:/sbin", "HOME": os.environ["HOME"], "LC_ALL": "C"}
    with tempfile.TemporaryDirectory(prefix="xxtab-bundle-test-") as temp:
        moved = Path(temp) / "Moved App With Spaces.app"
        shutil.copytree(app, moved)
        root = moved / "Contents/Helpers"
        scripts = moved / "Contents/Resources/wireguard"
        assert {p.name for p in root.iterdir()} == {"bash", "wg", "wireguard-go"}
        assert {p.name for p in scripts.iterdir()} == {"wg-quick", "wg-quick.bash"}
        cli = moved / "Contents/MacOS/xxtab"
        for binary in [root / name for name in ("bash", "wg", "wireguard-go")] + [cli, moved / "Contents/MacOS/xxtab-macos"]:
            if binary.name == "xxtab-macos":
                # Swift may reference Apple's system Swift runtime via @rpath.
                assert subprocess.check_output(["lipo", "-archs", str(binary)], text=True).strip() == arch
            else:
                deps.verify_binary(binary, arch)
            loads = subprocess.check_output(["otool", "-l", str(binary)], text=True)
            # Specifically inspect the minimum-OS load command, not dylib versions.
            match = re.search(r"cmd LC_BUILD_VERSION\n(?:(?!\nLoad command).)*?minos (\d+)\.(\d+)", loads, re.S)
            if match is None:
                match = re.search(r"cmd LC_VERSION_MIN_MACOSX\n\s*cmdsize \d+\n\s*version (\d+)\.(\d+)", loads)
            assert match and tuple(map(int, match.groups())) <= (13, 0), f"macOS 13 compatibility: {binary}"
            subprocess.run(["codesign", "--verify", "--strict", str(binary)], check=True)
        subprocess.run(["codesign", "--verify", "--deep", "--strict", str(moved)], check=True)
        for name in ("bash", "wg", "wireguard-go"):
            subprocess.run([str(root / name), "--version"], env=clean, check=True)
        # Tests the wrapper, bundled Bash 4+ requirement and upstream script without root/network changes.
        subprocess.run([str(scripts / "wg-quick"), "--help"], env=clean, check=True, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
        request = json.dumps({"action": "dependencies", "root": str(Path(temp) / "profiles")})
        def ready():
            result = subprocess.run([str(cli), "desktop"], input=request, env=clean, capture_output=True, text=True, check=True)
            return json.loads(result.stdout)["data"]["ready"]
        assert ready(), "complete app cannot find its bundled tools"
        subprocess.run([str(moved / "Contents/MacOS/xxtab-macos"), "--smoke-test"], env=clean, check=True)
        if os.environ.get("XXTAB_MACOS_LIFECYCLE_TEST") == "1":
            assert test_binary, "missing system lifecycle test binary"
            tests = Path(temp) / "System Tests.app/Contents"
            (tests / "MacOS").mkdir(parents=True)
            (tests / "Helpers").symlink_to(moved / "Contents/Helpers", target_is_directory=True)
            (tests / "Resources").symlink_to(moved / "Contents/Resources", target_is_directory=True)
            runner = tests / "MacOS/system-tests"
            shutil.copy2(test_binary, runner)
            subprocess.run(["/usr/bin/sudo", "-n", "/usr/bin/env", "PATH=" + clean["PATH"], str(runner), "system::tests::", "--ignored", "--test-threads=1"], env=clean, check=True)
            subprocess.run([sys.executable, str(REPO / "tests/macos_session.py"), str(cli)], env=clean, check=True)
        # Even on runners with Brew installed, a damaged app must not silently use it.
        bash = root / "bash"
        hidden = root / "bash.unavailable"
        bash.rename(hidden)
        try:
            assert not ready(), "app silently fell back to Homebrew"
        finally:
            hidden.rename(bash)
    print("PASS: self-contained, signed, relocated app; system-only PATH; missing dependency rejection")


if __name__ == "__main__":
    main(Path(sys.argv[1]).resolve(strict=True), sys.argv[2], sys.argv[3] if len(sys.argv) > 3 else "")
