#!/usr/bin/env python3
"""Build relocatable runtime dependencies, pinned by SHA256, for macOS 13+.

Only executables, wg-quick and licenses enter the app. Corresponding upstream
sources and build instructions are published as a separate Release attachment.
"""
import argparse
from concurrent.futures import ThreadPoolExecutor
import hashlib
import json
import os
from pathlib import Path
import platform
import shutil
import subprocess
import sys
import tarfile
import tempfile
import time
import urllib.request

REPO = Path(__file__).resolve().parent.parent
MANIFEST = REPO / "packaging/macos/dependencies.json"


def run(args, **kwargs):
    return subprocess.run([str(arg) for arg in args], check=True, **kwargs)


def fetch(item, destination):
    path = destination / item["name"]
    local = REPO / "archives" / item["name"]
    if local.is_file():
        if hashlib.sha256(local.read_bytes()).hexdigest() != item["sha256"]:
            raise ValueError(f"Local source SHA256 mismatch: {item['name']}")
        shutil.copy2(local, path)
    if path.is_file() and hashlib.sha256(path.read_bytes()).hexdigest() == item["sha256"]:
        return path
    for attempt in range(3):
        try:
            request = urllib.request.Request(item["url"], headers={"User-Agent": "xxtab-build"})
            with urllib.request.urlopen(request, timeout=60) as response, path.open("wb") as output:
                shutil.copyfileobj(response, output)
            if hashlib.sha256(path.read_bytes()).hexdigest() != item["sha256"]:
                raise ValueError(f"SHA256 mismatch: {item['name']}")
            return path
        except OSError:
            if attempt == 2:
                raise
            time.sleep(1)


def prepare_sources(work):
    manifest = json.loads(MANIFEST.read_text())
    archives = work / "archives"
    archives.mkdir()
    items = [manifest[name] for name in ("bash", "wireguard-tools", "wireguard-go")]
    items += manifest["bash"]["patches"]
    with ThreadPoolExecutor(max_workers=4) as pool:
        list(pool.map(lambda item: fetch(item, archives), items))
    sources = {}
    for name in ("bash", "wireguard-tools", "wireguard-go"):
        destination = work / name
        destination.mkdir()
        with tarfile.open(archives / manifest[name]["name"]) as archive:
            archive.extractall(destination, filter="data")
        children = list(destination.iterdir())
        if len(children) != 1 or not children[0].is_dir():
            raise ValueError(f"Unexpected source layout: {name}")
        sources[name] = children[0]
    return manifest, sources, archives


def linked_libraries(output):
    # otool prints one install name per line, followed by compatibility info.
    return [line.strip().split(" (compatibility version", 1)[0]
            for line in output.splitlines()[1:] if line.strip()]


def verify_binary(binary, arch):
    actual = run(["lipo", "-archs", binary], capture_output=True, text=True).stdout.strip()
    if actual != arch:
        raise ValueError(f"Wrong architecture for {binary.name}: {actual}")
    output = run(["otool", "-L", binary], capture_output=True, text=True).stdout
    for library in linked_libraries(output):
        if not library.startswith(("/usr/lib/", "/System/Library/")):
            raise ValueError(f"Non-system dylib in {binary.name}: {library}")


def build(app, archive_path):
    if sys.platform != "darwin":
        raise SystemExit("Build dependencies on macOS with Xcode Command Line Tools and Go.")
    arch = platform.machine()
    if arch not in ("arm64", "x86_64"):
        raise SystemExit("Unsupported architecture")
    app = app.resolve(strict=True)
    binary_dir = app / "Contents/Helpers"
    script_dir = app / "Contents/Resources/wireguard"
    licenses = app / "Contents/Resources/licenses"
    binary_dir.mkdir(parents=True)
    script_dir.mkdir(parents=True)
    licenses.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="macos-deps-", dir=REPO / ".tools") as temp:
        work = Path(temp)
        manifest, sources, archives = prepare_sources(work)
        env = os.environ.copy()
        env.update(MACOSX_DEPLOYMENT_TARGET="13.0", GOTOOLCHAIN="local", CGO_ENABLED="0", GOOS="darwin",
                   GOARCH="arm64" if arch == "arm64" else "amd64", GOFLAGS="", GOWORK="off")
        go_version = run(["go", "version"], env=env, capture_output=True, text=True).stdout.split()[2]
        if go_version != "go" + manifest["go_version"]:
            raise ValueError(f"Use Go {manifest['go_version']} for reproducible dependencies (found {go_version})")
        env["CC"] = run(["xcrun", "--find", "clang"], capture_output=True, text=True).stdout.strip()
        env["SDKROOT"] = run(["xcrun", "--show-sdk-path"], capture_output=True, text=True).stdout.strip()
        env["CFLAGS"] = f"-Os -arch {arch} -mmacosx-version-min=13.0"
        env["LDFLAGS"] = f"-arch {arch} -mmacosx-version-min=13.0"
        # Do not accidentally detect Homebrew libraries via the build shell.
        for key in ("CPPFLAGS", "LIBRARY_PATH", "CPATH", "C_INCLUDE_PATH", "CPLUS_INCLUDE_PATH", "DYLD_LIBRARY_PATH"):
            env.pop(key, None)
        env.update(ac_cv_func_strchrnul="no", bash_cv_func_strchrnul_works="no")
        jobs = str(min(os.cpu_count() or 2, 8))
        bash = sources["bash"]
        for patch in manifest["bash"]["patches"]:
            run(["/usr/bin/patch", "-f", "-p0", "-i", archives / patch["name"]], cwd=bash)
        run([bash / "configure", "--prefix=/usr", "--disable-nls", "--disable-readline", "--without-bash-malloc"], cwd=bash, env=env)
        run(["make", "-j", jobs, "bash"], cwd=bash, env=env)
        shutil.copy2(bash / "bash", binary_dir / "bash")
        tools = sources["wireguard-tools"]
        run(["make", "-C", tools / "src", "-j", jobs, "wg", "PLATFORM=darwin", "WITH_WGQUICK=no",
             "WITH_SYSTEMDUNITS=no", "WITH_BASHCOMPLETION=no", "WIREGUARD_TOOLS_VERSION=" + manifest["wireguard-tools"]["version"]], env=env)
        shutil.copy2(tools / "src/wg", binary_dir / "wg")
        shutil.copy2(tools / "src/wg-quick/darwin.bash", script_dir / "wg-quick.bash")
        shutil.copy2(REPO / "packaging/macos/wg-quick", script_dir / "wg-quick")
        go = sources["wireguard-go"]
        run(["go", "mod", "download"], cwd=go, env=env)
        run(["go", "mod", "verify"], cwd=go, env=env)
        run(["go", "build", "-mod=readonly", "-trimpath", "-buildvcs=false", "-ldflags=-s -w", "-o", binary_dir / "wireguard-go", "."], cwd=go, env=env)
        for name in ("bash", "wg", "wireguard-go"):
            (binary_dir / name).chmod(0o755)
        for name in ("wg-quick", "wg-quick.bash"):
            (script_dir / name).chmod(0o755)
        for name in ("bash", "wg", "wireguard-go"):
            verify_binary(binary_dir / name, arch)
        shutil.copy2(bash / "COPYING", licenses / "Bash-COPYING.txt")
        shutil.copy2(tools / "COPYING", licenses / "WireGuard-tools-COPYING.txt")
        shutil.copy2(go / "LICENSE", licenses / "wireguard-go-LICENSE.txt")
        goroot = Path(run(["go", "env", "GOROOT"], env=env, capture_output=True, text=True).stdout.strip())
        shutil.copy2(goroot / "LICENSE", licenses / "Go-LICENSE.txt")
        # Include licenses of modules actually linked into this Darwin executable.
        modules = run(["go", "list", "-mod=readonly", "-deps", "-f", "{{if .Module}}{{if not .Module.Main}}{{.Module.Dir}}{{end}}{{end}}", "."], cwd=go, env=env, capture_output=True, text=True).stdout
        for index, directory in enumerate(sorted(set(filter(None, modules.splitlines())))):
            shutil.copy2(Path(directory) / "LICENSE", licenses / f"Go-module-{index}-{Path(directory).name}-LICENSE.txt")
        source_name = archive_path.name
        versions = {name: manifest[name]["version"] for name in ("bash", "wireguard-tools", "wireguard-go")}
        notice = ("Bundled runtime components: " + json.dumps(versions) + "\n"
                  "Bash: GPL-3.0-or-later; wireguard-tools and wg-quick: GPL-2.0-only; wireguard-go: MIT.\n"
                  "Go and golang.org/x modules: BSD-style licenses included alongside this notice.\n"
                  "Corresponding sources, Bash patches, and build scripts are provided alongside this app at\n"
                  f"https://github.com/HZ-PRE/xxtab/releases under {source_name}\n")
        (licenses / "macos-components.txt").write_text(notice)
        # GPL corresponding source is a separate downloadable artifact, never app bloat.
        archive_path.parent.mkdir(parents=True, exist_ok=True)
        with tarfile.open(archive_path, "w:gz") as output:
            output.add(archives, arcname="xxtab-dependency-sources/archives")
            for relative in ("tools/build-macos-deps.py", "packaging/macos/dependencies.json", "packaging/macos/wg-quick"):
                output.add(REPO / relative, arcname="xxtab-dependency-sources/" + relative)
            readme = work / "BUILD.txt"
            readme.write_text("Build on macOS with Python 3.12+, Xcode Command Line Tools and Go " + manifest["go_version"] + ".\n"
                              "Create .tools and an empty xxtab.app/Contents directory. Run:\n"
                              "python3 tools/build-macos-deps.py xxtab.app rebuilt-sources.tar.gz\n"
                              "The archives directory contains the exact unmodified sources and Bash patches.\n"
                              "The included script uses these local archives when present, or downloads and verifies the same files.\n")
            output.add(readme, arcname="xxtab-dependency-sources/BUILD.txt")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("app", type=Path)
    parser.add_argument("sources", type=Path)
    args = parser.parse_args()
    (REPO / ".tools").mkdir(exist_ok=True)
    build(args.app, args.sources.resolve())
