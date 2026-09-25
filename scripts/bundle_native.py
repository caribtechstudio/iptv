#!/usr/bin/env python3
"""Copies FFmpeg and libmpv, with every non-system library they load, into
src-tauri/resources/native so that Fluxo.app runs without Homebrew.

Layout (inside Fluxo.app/Contents/Resources/native):
    ffmpeg             the FFmpeg command, loading its libraries from lib/
    lib/*.dylib        libmpv.2.dylib and all dependencies, loading each other from lib/

Install names are rewritten to @executable_path/@loader_path and every file is re-signed
ad hoc, as Apple Silicon requires after `install_name_tool`.
"""

from __future__ import annotations

import os
import shutil
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
OUT = ROOT / "src-tauri" / "resources" / "native"
SYSTEM_PREFIXES = ("/usr/lib/", "/System/")


def run(*args: str) -> str:
    return subprocess.run(args, check=True, capture_output=True, text=True).stdout


def brew_prefix() -> Path:
    try:
        return Path(run("brew", "--prefix").strip())
    except (OSError, subprocess.CalledProcessError):
        return Path("/opt/homebrew")


def dependencies(path: Path) -> list[str]:
    """Libraries referenced by a Mach-O file, without its own install name."""
    lines = run("otool", "-L", str(path)).splitlines()[1:]
    names = [line.strip().split(" (")[0] for line in lines if line.strip()]
    own_id = run("otool", "-D", str(path)).splitlines()[1:]
    own = own_id[0].strip() if own_id else None
    return [name for name in names if name != own]


def rpaths(path: Path) -> list[str]:
    result, lines = [], run("otool", "-l", str(path)).splitlines()
    for index, line in enumerate(lines):
        if line.strip() == "cmd LC_RPATH":
            for follow in lines[index + 1 : index + 4]:
                follow = follow.strip()
                if follow.startswith("path "):
                    result.append(follow.split(" ", 1)[1].split(" (offset")[0])
    return result


def resolve(reference: str, origin: Path) -> Path | None:
    """Real file behind an install name, following @rpath/@loader_path and symlinks."""
    if reference.startswith("@loader_path/") or reference.startswith("@executable_path/"):
        candidate = origin.parent / reference.split("/", 1)[1]
        return candidate.resolve() if candidate.exists() else None
    if reference.startswith("@rpath/"):
        rest = reference.split("/", 1)[1]
        for entry in rpaths(origin):
            base = entry.replace("@loader_path", str(origin.parent)).replace(
                "@executable_path", str(origin.parent)
            )
            candidate = Path(base) / rest
            if candidate.exists():
                return candidate.resolve()
        return None
    candidate = Path(reference)
    return candidate.resolve() if candidate.exists() else None


def is_system(reference: str) -> bool:
    return reference.startswith(SYSTEM_PREFIXES)


def main() -> int:
    prefix = brew_prefix()
    ffmpeg = prefix / "bin" / "ffmpeg"
    libmpv = prefix / "lib" / "libmpv.2.dylib"
    missing = [str(path) for path in (ffmpeg, libmpv) if not path.exists()]
    if missing:
        print(f"Introuvable : {', '.join(missing)}. Installez-les avec « brew install mpv ».")
        return 1

    shutil.rmtree(OUT, ignore_errors=True)
    (OUT / "lib").mkdir(parents=True)

    # source file -> bundled file; libraries are keyed by their real path.
    bundled: dict[Path, Path] = {}
    queue: list[Path] = []

    def add(source: Path, target: Path) -> None:
        if source in bundled:
            return
        shutil.copy2(source, target)
        target.chmod(0o755)
        bundled[source] = target
        queue.append(source)

    add(ffmpeg.resolve(), OUT / "ffmpeg")
    add(libmpv.resolve(), OUT / "lib" / "libmpv.2.dylib")

    names: dict[Path, str] = {libmpv.resolve(): "libmpv.2.dylib"}
    while queue:
        source = queue.pop()
        target = bundled[source]
        is_executable = target.parent == OUT
        loader = "@executable_path/lib" if is_executable else "@loader_path"
        changes: list[str] = []
        for reference in dependencies(source):
            if is_system(reference):
                continue
            real = resolve(reference, source)
            if real is None:
                print(f"Dépendance introuvable : {reference} (de {source})")
                return 1
            name = names.setdefault(real, Path(reference).name)
            add(real, OUT / "lib" / name)
            changes += ["-change", reference, f"{loader}/{name}"]
        args = ["install_name_tool"]
        if not is_executable:
            args += ["-id", f"@loader_path/{target.name}"]
        args += changes
        if len(args) > 1:
            subprocess.run([*args, str(target)], check=True, capture_output=True)
        for entry in rpaths(target):
            subprocess.run(
                ["install_name_tool", "-delete_rpath", entry, str(target)],
                check=False,
                capture_output=True,
            )

    for target in bundled.values():
        subprocess.run(["codesign", "--force", "--sign", "-", str(target)], check=True, capture_output=True)

    # Every reference must now stay inside the bundle.
    for target in bundled.values():
        for reference in dependencies(target):
            if not is_system(reference) and not reference.startswith(("@loader_path/", "@executable_path/")):
                print(f"Référence externe restante dans {target.name} : {reference}")
                return 1

    check = subprocess.run([str(OUT / "ffmpeg"), "-hide_banner", "-version"], capture_output=True, text=True)
    if check.returncode != 0:
        print(f"Le FFmpeg copié ne démarre pas : {check.stderr.strip()}")
        return 1
    size = sum(path.stat().st_size for path in bundled.values())
    print(f"{len(bundled)} fichiers copiés dans {OUT.relative_to(ROOT)} ({size / 1_048_576:.0f} Mo).")
    return 0


if __name__ == "__main__":
    sys.exit(main())
