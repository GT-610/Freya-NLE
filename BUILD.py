#!/usr/bin/env python3
"""Build and package the Freya NLE desktop application (Rust + Slint)."""

import argparse
import hashlib
import json
import locale
import multiprocessing
import os
import platform
import queue
import re
import shlex
import shutil
import signal
import stat
import subprocess
import sys
import tarfile
import tempfile
import threading
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Callable, ClassVar, List, Optional, Type


_SEMVER_NUMERIC_IDENTIFIER = r"(?:0|[1-9][0-9]*)"
_SEMVER_NON_NUMERIC_IDENTIFIER = r"(?:[0-9]*[A-Za-z-][0-9A-Za-z-]*)"
_SEMVER_PRERELEASE_IDENTIFIER = rf"(?:{_SEMVER_NUMERIC_IDENTIFIER}|{_SEMVER_NON_NUMERIC_IDENTIFIER})"
_SEMVER_PATTERN = re.compile(
    rf"^(?P<major>{_SEMVER_NUMERIC_IDENTIFIER})\."
    rf"(?P<minor>{_SEMVER_NUMERIC_IDENTIFIER})\."
    rf"(?P<patch>{_SEMVER_NUMERIC_IDENTIFIER})"
    rf"(?:-{_SEMVER_PRERELEASE_IDENTIFIER}(?:\.{_SEMVER_PRERELEASE_IDENTIFIER})*)?"
    r"(?:\+[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?$"
)

# Arch is a rolling release, so `archlinux:latest` silently changes the glibc
# and FFmpeg sonames an archive is linked against. Pin the digest instead: a
# rebuild of one commit then produces the same runtime floor it did before.
ARCH_LINUX_IMAGE = (
    "docker.io/library/archlinux@sha256:"
    "d4594bb2300cba11f41c2f27923eb56f63d9988909c402f9c95517d1e362c899"
)
# The AppImage is built on the oldest distribution the toolchain supports, so
# the binary asks for the lowest glibc it can rather than Arch's current one.
# Ubuntu 22.04 carries glibc 2.35, which is what the AppImage's ELF floor is.
APPIMAGE_BASE_IMAGE = (
    "docker.io/library/ubuntu@sha256:"
    "076e7bf82319619f28dd0aa9c02ab34b055ad37100395b4f49a2ffba04fb5e11"
)


def parse_semver(version_string: str) -> tuple[int, int, int]:
    match = _SEMVER_PATTERN.fullmatch(version_string)
    if not match:
        raise ValueError(f"Invalid semantic version: {version_string}")
    return tuple(int(match.group(name)) for name in ("major", "minor", "patch"))


def numeric_version(version_string: str) -> str:
    """Return the ``major.minor.patch`` core of a semantic version.

    macOS accepts only one to three period-separated integers in
    ``CFBundleShortVersionString`` and ``CFBundleVersion``, so the prerelease
    suffix of a candidate such as ``0.7.0-rc.1`` cannot be written into them
    verbatim. The suffix stays visible through the About dialog, which reads the
    crate version.
    """
    return ".".join(str(part) for part in parse_semver(version_string))


def normalize_macos_architecture(machine: str) -> str:
    normalized = machine.strip().lower()
    normalized = {"aarch64": "arm64", "amd64": "x86_64", "x64": "x86_64"}.get(normalized, normalized)
    if normalized not in {"arm64", "x86_64"}:
        raise RuntimeError(f"Unsupported macOS architecture: {machine}")
    return normalized


def read_project_version(source_dir: Path) -> str:
    """Read the canonical Rust workspace version."""
    cargo_toml = source_dir / "rust" / "Cargo.toml"
    try:
        text = cargo_toml.read_text(encoding="utf-8")
    except OSError as exc:
        raise RuntimeError(f"Could not read project version from {cargo_toml}: {exc}") from exc
    match = re.search(r'^version\s*=\s*"([^"]+)"\s*$', text, flags=re.MULTILINE)
    if not match:
        raise RuntimeError(f"Could not find workspace package version in {cargo_toml}")
    return match.group(1)


@dataclass
class BuildConfig:
    source_dir: Path
    temp_base: Path
    output_dir: Path
    target: str
    is_debug: bool
    use_container: bool
    is_offline: bool
    version_major: int = 0
    version_minor: int = 0
    version_patch: int = 0
    version_string: str = "0.0.0"
    # Fail instead of widening the declared macOS range when a dependency was
    # built for a newer system. CI sets this so a bottle that outruns the
    # deployment target breaks the build rather than the promise.
    strict_deployment_target: bool = False

    @property
    def build_type(self) -> str:
        return "Debug" if self.is_debug else "Release"

    @property
    def work_dir(self) -> Path:
        return self.temp_base / self.target / self.build_type

    @property
    def cargo_target_dir(self) -> Path:
        return self.work_dir / "cargo-target"

    @property
    def cargo_profile(self) -> str:
        return "debug" if self.is_debug else "release"

    @property
    def dist_dir(self) -> Path:
        return self.source_dir / "dist"


class Logger:
    def __init__(self, log_cb: Callable[[str], None], progress_cb: Callable[[int, str], None]):
        self._log = log_cb
        self._progress = progress_cb

    def log(self, message: str):
        self._log(message)

    def section(self, title: str):
        self._log(f">>> {title}")

    def progress(self, value: int, message: str):
        self._progress(value, message)


class PlatformBuilder:
    RESOURCE_DIRECTORIES: ClassVar[tuple[tuple[str, ...], ...]] = (
        ("catalog", "effects"),
        ("catalog", "objects"),
        ("plugins",),
        ("effect-packages",),
        ("repos",),
    )

    def __init__(self, config: BuildConfig, logger: Logger):
        self.config = config
        self.logger = logger
        self.env = os.environ.copy()
        self.env.update({
            "GIT_TERMINAL_PROMPT": "0",
            "HOMEBREW_NO_AUTO_UPDATE": "1",
            "DEBIAN_FRONTEND": "noninteractive",
            "CARGO_TARGET_DIR": str(config.cargo_target_dir),
        })
        self.container_name = ""
        self.use_container = False
        self.cancelled = False
        self.current_proc: Optional[subprocess.Popen] = None

    def build(self):
        self.logger.progress(10, f"{self.config.build_type} build started")
        self.logger.section("Checking dependencies")
        self.install_dependencies()
        self.check_cancelled()
        self.logger.section("Compiling Rust + Slint frontend")
        self.compile()
        self.logger.progress(70, "Compilation complete")
        self.check_cancelled()
        self.logger.section("Packaging")
        self.package()
        self.logger.progress(90, "Package assembled")
        self.check_cancelled()
        self.logger.section("Creating archive")
        self.archive()
        self.logger.progress(100, "Complete")

    def check_cancelled(self):
        if self.cancelled:
            raise RuntimeError("Build cancelled")

    def cancel(self):
        self.cancelled = True
        process = self.current_proc
        if process and process.poll() is None:
            self.logger.log("Stopping running command...")
            try:
                if os.name != "nt":
                    os.killpg(process.pid, signal.SIGTERM)
                else:
                    process.terminate()
            except ProcessLookupError:
                pass

    def install_dependencies(self):
        pass

    def get_cargo_build_cmd(self) -> List[str]:
        command = [
            "cargo", "build",
            "--manifest-path", str(self.config.source_dir / "rust" / "Cargo.toml"),
            "--package", "freya-nle", "--locked",
        ]
        if not self.config.is_debug:
            command.append("--release")
        if self.config.is_offline:
            command.append("--offline")
        return command

    def cargo_environment(self) -> dict:
        """Environment the compiler needs, applied before any cargo invocation."""
        return {}

    def compile(self):
        jobs = multiprocessing.cpu_count()
        self.env["CARGO_BUILD_JOBS"] = str(jobs)
        self.env.update({name: value for name, value in self.cargo_environment().items()})
        self.logger.log(f"Parallel jobs: {jobs}")
        self.run_cmd(self.get_cargo_build_cmd())

    def cargo_binary(self, windows: bool = False) -> Path:
        suffix = ".exe" if windows else ""
        return self.config.cargo_target_dir / self.config.cargo_profile / f"freya-nle{suffix}"

    def packaged_executable_name(self, windows: bool = False) -> str:
        """Name the staged executable takes inside the archive.

        The macOS bundle puts `freya-nle` under `Contents/MacOS`, so the other
        platforms ship the same name instead of the pre-rename `AviQtl`.
        """
        return f"freya-nle{'.exe' if windows else ''}"

    def package(self):
        raise NotImplementedError

    def prepare_output_dir(self):
        self.config.output_dir.mkdir(parents=True, exist_ok=True)
        # Materialize the directory entries before removing anything.  Keeping
        # an os.scandir iterator alive while rmtree runs can leave the output
        # directory handle open on Windows.
        for entry in list(self.config.output_dir.iterdir()):
            if entry.is_dir() and not entry.is_symlink():
                self.remove_tree(entry)
                continue
            try:
                entry.unlink()
            except PermissionError:
                os.chmod(entry, os.stat(entry).st_mode | stat.S_IWUSR)
                entry.unlink()

    def copy_resources(self, destination: Path):
        for parts in self.RESOURCE_DIRECTORIES:
            source = self.config.source_dir.joinpath(*parts)
            if source.exists():
                shutil.copytree(source, destination / parts[-1], dirs_exist_ok=True)
                self.logger.log(f"Bundled resources: {source.relative_to(self.config.source_dir)}")

    def find_carla_discovery_tool(self, windows: bool = False) -> Path | None:
        name = "carla-discovery-native.exe" if windows else "carla-discovery-native"
        found = shutil.which(name, path=self.env.get("PATH"))
        roots = [self.config.source_dir / "vendor" / "carla"]
        if found:
            roots.append(Path(found).parent)
        if platform.system() == "Darwin" and shutil.which("brew"):
            try:
                prefix = subprocess.check_output(
                    ["brew", "--prefix", "carla"], text=True, stderr=subprocess.DEVNULL
                ).strip()
                roots.append(Path(prefix))
            except subprocess.CalledProcessError:
                pass
        for root in roots:
            candidates = [root / name, root / "runtime" / name, *root.glob(f"**/{name}")]
            for candidate in candidates:
                if candidate.is_file():
                    return candidate
        return None

    def archive(self):
        self.config.dist_dir.mkdir(parents=True, exist_ok=True)
        archive_name = self.get_archive_name()
        archive_path = self.create_archive(archive_name)
        self.logger.log(f"Archive: {archive_path}")

    # -- Artifact naming -------------------------------------------------
    #
    # One scheme for every platform:
    #
    #     Freya-NLE-<version>-<os>-<arch>[-<variant>].<ext>
    #
    # The toolchain that produced a build belongs in `build-info.json`, not in
    # the file name: a user downloads "the Linux build for x86_64", and the
    # Arch-compatible archive is a variant of that, not a separate platform.

    PLATFORM_OS: ClassVar[str] = "unknown"
    PLATFORM_ARCH: ClassVar[str] = "unknown"
    # A variant marks a build that is compatible only with a narrower host than
    # its OS and architecture imply, such as the Arch-current sysroot.
    artifact_variant: ClassVar[str] = ""
    archive_extension: ClassVar[str] = ".zip"

    def get_archive_name(self) -> str:
        parts = [
            "Freya-NLE",
            self.config.version_string,
            self.PLATFORM_OS,
            self.PLATFORM_ARCH,
        ]
        if self.artifact_variant:
            parts.append(self.artifact_variant)
        return "-".join(parts)

    def create_archive(self, archive_name: str) -> Path:
        archive_path = self.config.dist_dir / f"{archive_name}{self.archive_extension}"
        archive_path.unlink(missing_ok=True)
        if self.archive_extension == ".zip":
            self.create_zip(archive_name)
        elif self.archive_extension == ".tar.zst":
            self.create_tar_zst(archive_name)
        elif self.archive_extension == ".AppImage":
            self.create_appimage(archive_name)
        else:
            raise RuntimeError(f"Unsupported archive extension: {self.archive_extension}")
        return self.config.dist_dir / f"{archive_name}{self.archive_extension}"

    def create_zip(self, archive_name: str):
        # `shutil.make_archive` does not preserve the executable bit, so the
        # macOS bundle is zipped with `ditto` and Linux ships `.tar.zst`.
        shutil.make_archive(str(self.config.dist_dir / archive_name), "zip", root_dir=self.config.output_dir)

    def create_tar_zst(self, archive_name: str):
        """Write a `.tar.zst` that keeps mode bits and stores no host metadata.

        `tarfile` records the exact permission of every entry, which a zip made
        by `shutil.make_archive` does not, so `freya-nle` stays executable after
        extraction. Ownership and device numbers are normalised and the mtime is
        pinned to the source's commit time so two builds of one commit differ
        only by their contents.
        """
        if not shutil.which("zstd", path=self.env.get("PATH")):
            raise RuntimeError("zstd was not found; install it to build the Linux archive")
        tar_path = self.config.dist_dir / f"{archive_name}.tar"
        tar_path.unlink(missing_ok=True)
        with tarfile.open(tar_path, "w", format=tarfile.PAX_FORMAT) as tar:
            for entry in sorted(self.config.output_dir.rglob("*")):
                relative = entry.relative_to(self.config.output_dir)
                info = tar.gettarinfo(str(entry), arcname=str(relative))
                info.uid = info.gid = 0
                info.uname = info.gname = ""
                if info.isreg():
                    with entry.open("rb") as handle:
                        tar.addfile(info, handle)
                else:
                    tar.addfile(info)
        archive_path = self.config.dist_dir / f"{archive_name}.tar.zst"
        try:
            self.run_cmd([
                "zstd", "--quiet", "--force", "-19", "--long", "-T0",
                str(tar_path), "-o", str(archive_path),
            ], force_host=True)
        finally:
            tar_path.unlink(missing_ok=True)

    # -- Archive contents ------------------------------------------------

    def write_build_info(self, destination: Path):
        """Record how this build was produced, next to the binaries.

        The file name deliberately carries no toolchain, so this is where the
        answer to "what is this build tied to" lives: the commit, the pinned
        toolchain, the host libraries, and the minimum of each OS the binaries
        were actually produced against.
        """
        info = {
            "name": "Freya NLE",
            "version": self.config.version_string,
            "platform": self.PLATFORM_OS,
            "architecture": self.PLATFORM_ARCH,
            "variant": self.artifact_variant or None,
            "profile": self.config.build_type,
            "commit": self.git_commit(),
            "target": self.build_target(),
            "rustc": self.command_output(["rustc", "--version"]),
            "ffmpeg": self.ffmpeg_provenance(),
            "requirements": self.runtime_requirements(),
        }
        destination.mkdir(parents=True, exist_ok=True)
        (destination / "build-info.json").write_text(
            json.dumps(info, indent=2, sort_keys=True) + "\n", encoding="utf-8"
        )

    def build_target(self) -> str:
        for line in self.command_output(["rustc", "-vV"]).splitlines():
            if line.startswith("host: "):
                return line[len("host: "):].strip()
        return ""

    def git_commit(self) -> str:
        output = self.command_output(["git", "rev-parse", "HEAD"])
        return output.splitlines()[0].strip() if output else ""

    def ffmpeg_provenance(self) -> dict:
        """Report where FFmpeg came from, because it decides the runtime floor.

        A build linked against a distribution's FFmpeg is tied to that
        distribution's soname. `FFMPEG_DIR` marks a locally built one, and
        anything else is the host's own package manager.
        """
        ffmpeg_dir = self.env.get("FFMPEG_DIR")
        source = "ffmpeg-dir" if ffmpeg_dir else "system"
        version = ""
        for probe in (["ffmpeg", "-version"],):
            output = self.command_output(probe)
            if output:
                version = output.splitlines()[0].strip()
                break
        return {"source": source, "path": ffmpeg_dir or None, "version": version}

    def runtime_requirements(self) -> dict:
        return {}

    def copy_licenses(self, destination: Path):
        """Ship the licence texts the archive owes its recipients.

        The application is AGPLv3 and bundles FFmpeg (GPL) and x264, so an
        archive with no licence text at all is a compliance gap rather than a
        cosmetic omission. Missing optional files are skipped so a fixture tree
        can still be packaged.
        """
        legal = destination / "licenses"
        copied = False
        for source in (
            self.config.source_dir / "LICENSE.md",
            self.config.source_dir / "third-party-licenses.md",
        ):
            if source.is_file():
                legal.mkdir(parents=True, exist_ok=True)
                shutil.copy2(source, legal / source.name)
                copied = True
        if copied:
            self.logger.log("Bundled licence texts")

    def command_output(self, command: List[str]) -> str:
        try:
            result = subprocess.run(
                command, capture_output=True, text=True, encoding="utf-8",
                errors="replace", env=self.env, cwd=self.config.source_dir,
            )
        except OSError:
            return ""
        return result.stdout if result.returncode == 0 else ""

    def run_cmd(self, command: List[str], shell: bool = False, force_host: bool = False):
        self.check_cancelled()
        in_container = self.use_container and not force_host
        display_command = shlex.join(command) if isinstance(command, list) else command
        self.logger.log(f"{'[Container] ' if in_container else ''}{display_command}")
        actual_command = command
        if in_container:
            inner = f"cd {shlex.quote(str(self.config.source_dir))} && {display_command}"
            actual_command = self.container_invocation(inner)
            shell = True
        popen_options = {"start_new_session": True} if os.name != "nt" else {}
        process = subprocess.Popen(
            actual_command, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
            text=True, encoding="utf-8", errors="replace", shell=shell,
            env=self.env, cwd=self.config.source_dir, **popen_options,
        )
        self.current_proc = process
        try:
            assert process.stdout is not None
            for line in process.stdout:
                self.logger.log(line.rstrip())
            process.wait()
        finally:
            self.current_proc = None
        self.check_cancelled()
        if process.returncode != 0:
            raise subprocess.CalledProcessError(process.returncode, actual_command)

    def container_invocation(self, inner: str) -> str:
        """Command that runs `inner` inside the build container.

        `distrobox` is the preferred host because it creates the container as a
        persistent home-oriented environment. It is not installed everywhere --
        on macOS, in particular, only `podman` is -- so a plain `podman run`
        against the same image is used instead of failing the build outright.
        """
        if self.container_engine == "podman":
            return " ".join([
                "podman", "run", "--rm",
                "--userns=keep-id",
                "--security-opt", "label=disable",
                "--platform", "linux/amd64",
                "-v", f"{shlex.quote(str(self.config.source_dir))}:/src:Z",
                "-w", "/src",
                "-v", f"{shlex.quote(str(self.config.temp_base))}:/src/.build_tmp:Z",
                self.image_name,
                "bash", "-lc", shlex.quote(inner),
            ])
        return f"distrobox enter {shlex.quote(self.container_name)} -- bash -lc {shlex.quote(inner)}"

    def remove_tree(self, path: Path):
        def make_writable_and_retry(function, target, _exception):
            os.chmod(target, os.stat(target).st_mode | stat.S_IWUSR)
            function(target)
        try:
            shutil.rmtree(path, onexc=make_writable_and_retry)
        except TypeError:
            shutil.rmtree(path, onerror=make_writable_and_retry)


class LinuxBuilderBase(PlatformBuilder):
    def __init__(self, config: BuildConfig, logger: Logger):
        super().__init__(config, logger)
        self.use_container = config.use_container
        self.image_name = ""
        self.container_engine = "distrobox"

    def create_container(self):
        """Prepare the build container, or decide not to need one.

        With `distrobox` available the container is created once and reused.
        Without it, `podman run --rm` creates a container per command, so there
        is nothing to set up; the image is pulled on first use instead.
        """
        if shutil.which("distrobox") and shutil.which("podman"):
            self.container_engine = "distrobox"
            try:
                self.run_cmd([
                    "distrobox", "create", "--name", self.container_name,
                    "--image", self.image_name, "--yes",
                ], force_host=True)
            except subprocess.CalledProcessError:
                self.logger.log("Container already exists. Using as-is.")
            self.run_cmd(["true"])
            return
        if shutil.which("podman"):
            self.container_engine = "podman"
            self.run_cmd(["true"])
            return
        raise RuntimeError(
            "Building Linux packages needs distrobox or podman; install one of them"
        )

    def install_dependencies(self):
        if not self.use_container:
            self.logger.log("Host mode: skipping system package installation")
        elif self.config.is_offline:
            self.logger.log("Offline mode: using the existing container and dependencies")
            self.run_cmd(["true"])
        else:
            self.create_container()

    def package(self):
        self.prepare_output_dir()
        source = self.cargo_binary()
        if not source.is_file():
            raise FileNotFoundError(f"Executable not found: {source}")
        destination = self.config.output_dir / self.packaged_executable_name()
        shutil.copy2(source, destination)
        self.copy_resources(self.config.output_dir)
        if discovery := self.find_carla_discovery_tool():
            shutil.copy2(discovery, self.config.output_dir / discovery.name)
        self.copy_licenses(self.config.output_dir)
        self.write_build_info(self.config.output_dir)
        self.logger.log(f"Executable: {destination}")


class ArchBuilder(LinuxBuilderBase):
    # The archive is built against Arch current, which is a rolling release, so
    # its glibc and FFmpeg sonames are whatever that snapshot carried. The
    # variant says so in the file name; the AppImage is the portable track.
    PLATFORM_OS = "linux"
    PLATFORM_ARCH = "x86_64"
    artifact_variant = "archlinux"
    archive_extension = ".tar.zst"

    def __init__(self, config: BuildConfig, logger: Logger):
        super().__init__(config, logger)
        self.container_name = "archlinux-aviqtl"
        # Pinned so a rebuild months later links against the same glibc and
        # FFmpeg soname instead of silently tracking Arch's rolling release.
        self.image_name = ARCH_LINUX_IMAGE

    def install_dependencies(self):
        super().install_dependencies()
        if not self.use_container or self.config.is_offline:
            return
        dependencies = [
            "base-devel", "git", "rust", "clang", "pkgconf", "zip", "zstd", "ffmpeg",
            "mesa", "vulkan-devel", "libx11", "libxcursor", "libxi", "libxrandr",
            "libxkbcommon", "wayland", "wayland-protocols",
        ]
        self.run_cmd(["sudo", "pacman", "-Syu", "--needed", "--noconfirm", *dependencies])

    def runtime_requirements(self) -> dict:
        """Report the glibc and FFmpeg the archive was linked against.

        This is the answer to "will this run on my distribution": the archive
        needs at least the glibc symbol version listed here and the same FFmpeg
        sonames, which is why it is a variant rather than the standard build.
        """
        requirements = super().runtime_requirements()
        requirements["glibc"] = self.maximum_glibc_version()
        requirements["ffmpeg_sonames"] = self.ffmpeg_sonames()
        return requirements

    def maximum_glibc_version(self) -> str:
        output = self.command_output(["objdump", "-T", str(self.cargo_binary())])
        versions = re.findall(r"GLIBC_([0-9]+\.[0-9]+)", output)
        if not versions:
            return ""
        return max(versions, key=lambda value: tuple(int(part) for part in value.split(".")))

    def ffmpeg_sonames(self) -> list[str]:
        libraries = []
        for path in self.config.output_dir.rglob("*"):
            if not path.is_file():
                continue
            for needed in re.findall(r"Shared library: \[(lib(?:av|sw)[a-z]+\.so\.[0-9]+)\]",
                                     self.command_output(["readelf", "-d", str(path)])):
                if needed not in libraries:
                    libraries.append(needed)
        return sorted(libraries)


class AppImageBuilder(LinuxBuilderBase):
    """The portable Linux build.

    The Arch archive is linked against a rolling release: its glibc and FFmpeg
    sonames are whatever the snapshot carried, which is why it is a variant. The
    AppImage is the one a user on any distribution is expected to download, so
    it is built on the oldest distribution the toolchain supports, links one
    pinned FFmpeg, and ships the libraries that are not part of a desktop base
    system. Only the graphics driver, ALSA, and the X11/Wayland client libraries
    come from the host.
    """

    PLATFORM_OS = "linux"
    PLATFORM_ARCH = "x86_64"
    archive_extension = ".AppImage"

    # The desktop entry and the AppRun wrapper the image carries.
    DESKTOP_ID = "io.github.gt610.freya-nle"

    def __init__(self, config: BuildConfig, logger: Logger):
        super().__init__(config, logger)
        self.container_name = "ubuntu-appimage"
        self.image_name = APPIMAGE_BASE_IMAGE
        # `podman run` is the reliable path for a one-shot toolchain container;
        # distrobox creates a persistent home that this build does not want.
        self.container_engine = "podman"

    def install_dependencies(self):
        if not self.use_container:
            self.logger.log("Host mode: skipping system package installation")
            return
        # The pinned image is Ubuntu 22.04, so the packages come from apt. Rust
        # comes from rustup, not from the distribution: 22.04's rustc is far too
        # old for this workspace and `rust-toolchain.toml` pins 1.97.1.
        self.run_cmd([
            "bash", "-c",
            "export DEBIAN_FRONTEND=noninteractive && "
            "apt-get update -qq && "
            "apt-get install -y -qq --no-install-recommends "
            "build-essential curl ca-certificates git pkg-config zstd xz-utils "
            "libasound2-dev libxkbcommon-dev libwayland-dev libx11-dev "
            "libxcursor-dev libxi-dev libxrandr-dev file",
        ])
        self.install_rust()

    def install_rust(self):
        """Install the pinned Rust toolchain inside the build container."""
        channel = self.rust_channel()
        self.run_cmd([
            "bash", "-c",
            "if ! command -v cargo >/dev/null || "
            f"[ \"$(cargo --version | cut -d' ' -f2)\" != \"{channel}\" ]; then "
            "curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | "
            f"sh -s -- -y --profile minimal --default-toolchain {channel}; "
            "fi; "
            # The container's shell is non-interactive, so the rustup environment
            # has to be sourced explicitly for every later command.
            'export PATH="$HOME/.cargo/bin:$PATH"; '
            "rustc --version",
        ])

    def rust_channel(self) -> str:
        toolchain = self.config.source_dir / "rust-toolchain.toml"
        try:
            text = toolchain.read_text(encoding="utf-8")
        except OSError:
            return "stable"
        match = re.search(r'^channel\s*=\s*"([^"]+)"', text, flags=re.MULTILINE)
        return match.group(1) if match else "stable"

    def build_ffmpeg(self):
        """Build the pinned FFmpeg inside the same container as the application."""
        prefix = "/src/build/ffmpeg/x86_64-unknown-linux-gnu"
        self.run_cmd([
            "bash", "/src/scripts/build-ffmpeg.sh",
            "--target", "x86_64-unknown-linux-gnu", "--prefix", prefix,
        ])
        self.env["FFMPEG_DIR"] = str(self.config.source_dir / "build" / "ffmpeg"
                                     / "x86_64-unknown-linux-gnu")

    def cargo_environment(self) -> dict:
        environment = {}
        if ffmpeg_dir := self.env.get("FFMPEG_DIR"):
            # The build runs inside the container, where the source tree is /src.
            environment["FFMPEG_DIR"] = "/src/build/ffmpeg/x86_64-unknown-linux-gnu"
        return environment

    def package(self):
        super().package()
        self.stage_appdir()

    def appdir_path(self) -> Path:
        return self.config.output_dir / "FreyaNLE.AppDir"

    def stage_appdir(self):
        """Build the AppDir AppImage packaging needs, next to the plain files."""
        appdir = self.appdir_path()
        if appdir.exists():
            self.remove_tree(appdir)
        (appdir / "usr" / "bin").mkdir(parents=True)
        (appdir / "usr" / "lib").mkdir(parents=True)
        (appdir / "usr" / "share" / "applications").mkdir(parents=True)
        (appdir / "usr" / "share" / "icons" / "hicolor" / "512x512" / "apps").mkdir(parents=True)
        for entry in self.config.output_dir.iterdir():
            if entry.name == appdir.name:
                continue
            target = appdir / "usr" / "bin" / entry.name
            if entry.is_dir():
                shutil.copytree(entry, target)
            else:
                shutil.copy2(entry, target)
        self.write_apprun(appdir)
        self.write_desktop_entry(appdir)
        self.write_appimage_icon(appdir)
        self.logger.log(f"AppDir: {appdir}")

    def write_apprun(self, appdir: Path):
        """The entry point the AppImage runtime executes."""
        apprun = appdir / "AppRun"
        apprun.write_text(
            "#!/bin/sh\n"
            "# Resolve the mount point so the bundled resources are found next to\n"
            "# the executable, then hand every argument through unchanged.\n"
            'HERE="$(dirname "$(readlink -f "$0")")"\n'
            'export LD_LIBRARY_PATH="$HERE/usr/lib:$HERE/usr/lib/../lib:${LD_LIBRARY_PATH:-}"\n'
            'export PATH="$HERE/usr/bin:$PATH"\n'
            f'exec "$HERE/usr/bin/freya-nle" "$@"\n',
            encoding="utf-8",
        )
        apprun.chmod(0o755)

    def write_desktop_entry(self, appdir: Path):
        entry = appdir / "usr" / "share" / "applications" / f"{self.DESKTOP_ID}.desktop"
        entry.write_text(
            "[Desktop Entry]\n"
            "Type=Application\n"
            "Name=Freya NLE\n"
            "Comment=Non-linear video editor with AviUtl's editing model\n"
            "Exec=freya-nle %f\n"
            f"Icon={self.DESKTOP_ID}\n"
            "Terminal=false\n"
            "Categories=AudioVideo;Video;\n"
            "MimeType=application/x-freya-nle-project;\n",
            encoding="utf-8",
        )
        # appimagetool requires the desktop file at the AppDir root as well.
        shutil.copy2(entry, appdir / f"{self.DESKTOP_ID}.desktop")

    def write_appimage_icon(self, appdir: Path):
        """Write a 512x512 PNG icon, converting the committed SVG if needed."""
        target = appdir / "usr" / "share" / "icons" / "hicolor" / "512x512" / "apps" / f"{self.DESKTOP_ID}.png"
        source_svg = self.config.source_dir / "assets" / "icon.svg"
        ico = self.config.source_dir / "assets" / "icon.ico"
        self.run_cmd([
            "bash", "-c",
            # `rsvg-convert` or ImageMagick may be absent; the ICO already holds
            # a 256x256 frame, which is an acceptable AppImage icon.
            f"if command -v rsvg-convert >/dev/null && [ -f {shlex.quote(str(source_svg))} ]; then "
            f"rsvg-convert -w 512 -h 512 {shlex.quote(str(source_svg))} -o {shlex.quote(str(target))}; "
            f"elif command -v convert >/dev/null && [ -f {shlex.quote(str(ico))} ]; then "
            f"convert {shlex.quote(str(ico))} -resize 512x512 {shlex.quote(str(target))}; "
            "else exit 1; fi",
        ])
        shutil.copy2(target, appdir / f"{self.DESKTOP_ID}.png")

    IMAGE_TOOL = "appimagetool-x86_64.AppImage"
    IMAGE_TOOL_URL = "https://github.com/AppImage/appimagetool/releases/download/continuous/appimagetool-x86_64.AppImage"
    IMAGE_TOOL_SHA256 = "e8c1169c4af66c3184d7ee110713d4be04befe671fa358e2c1591ee840553b03"

    def appimagetool_path(self) -> Path:
        tool = self.config.temp_base / self.IMAGE_TOOL
        if tool.is_file() and self.sha256_of(tool) == self.IMAGE_TOOL_SHA256:
            return tool
        tool.parent.mkdir(parents=True, exist_ok=True)
        tool.unlink(missing_ok=True)
        self.download(self.IMAGE_TOOL_URL, tool)
        actual = self.sha256_of(tool)
        if actual != self.IMAGE_TOOL_SHA256:
            tool.unlink(missing_ok=True)
            raise RuntimeError(
                f"appimagetool checksum mismatch: expected {self.IMAGE_TOOL_SHA256}, got {actual}"
            )
        tool.chmod(0o755)
        return tool

    @staticmethod
    def sha256_of(path: Path) -> str:
        digest = hashlib.sha256()
        with path.open("rb") as handle:
            for chunk in iter(lambda: handle.read(1024 * 1024), b""):
                digest.update(chunk)
        return digest.hexdigest()

    def download(self, url: str, destination: Path):
        self.logger.log(f"Downloading {url}")
        # `curl` honours the proxy environment the CI already sets; urllib would
        # need the same variables passed through by hand.
        self.run_cmd(["curl", "-fsSL", "--retry", "3", "-o", str(destination), url],
                     force_host=True)

    def create_appimage(self, archive_name: str):
        """Pack the staged AppDir into a self-contained executable image."""
        appdir = self.appdir_path()
        if not appdir.is_dir():
            raise FileNotFoundError(f"AppDir was not staged: {appdir}")
        tool = self.appimagetool_path()
        destination = self.config.dist_dir / f"{archive_name}.AppImage"
        environment = self.env.copy()
        # appimagetool refuses to run without an explicit architecture when it
        # cannot infer one from the host.
        environment["ARCH"] = "x86_64"
        # The tool is itself an AppImage; extracting it avoids needing FUSE in
        # the build container, which never has it.
        self.run_cmd(
            [str(tool), "--appimage-extract-and-run", "--no-appstream",
             str(appdir), str(destination)],
            force_host=True,
        )

    def runtime_requirements(self) -> dict:
        requirements = super().runtime_requirements()
        requirements["glibc"] = self.maximum_glibc_version()
        requirements["ffmpeg_sonames"] = self.ffmpeg_sonames()
        requirements["host_libraries"] = ["libasound.so.2"]
        return requirements

    def maximum_glibc_version(self) -> str:
        output = self.command_output(["objdump", "-T", str(self.cargo_binary())])
        versions = re.findall(r"GLIBC_([0-9]+\.[0-9]+)", output)
        if not versions:
            return ""
        return max(versions, key=lambda value: tuple(int(part) for part in value.split(".")))

    def ffmpeg_sonames(self) -> list[str]:
        return ArchBuilder.ffmpeg_sonames(self)


class WindowsDependencyMixin:
    WINDOWS_SYSTEM_DLLS = {
        "advapi32.dll", "authz.dll", "avrt.dll", "bcrypt.dll", "bcryptprimitives.dll",
        "cfgmgr32.dll", "combase.dll", "comctl32.dll",
        "comdlg32.dll", "crypt32.dll", "d3d9.dll", "d3d11.dll", "d3d12.dll",
        "d3dcompiler_47.dll", "dcomp.dll", "dnsapi.dll", "dwmapi.dll", "dwrite.dll",
        "dxgi.dll", "dxva2.dll", "gdi32.dll", "gdiplus.dll", "imm32.dll",
        "iphlpapi.dll", "kernel32.dll", "mpr.dll", "msimg32.dll", "msvcp140.dll",
        "msvcp_win.dll", "msvcrt.dll",
        "mf.dll", "mfplat.dll", "mfreadwrite.dll", "ncrypt.dll", "ntdll.dll",
        "netapi32.dll", "ole32.dll", "oleaut32.dll", "opengl32.dll", "powrprof.dll",
        "propsys.dll", "rpcrt4.dll", "secur32.dll", "setupapi.dll", "shcore.dll",
        "shell32.dll", "shlwapi.dll", "ucrtbase.dll", "user32.dll", "userenv.dll",
        "uiautomationcore.dll", "uxtheme.dll", "version.dll", "vcruntime140.dll", "vcruntime140_1.dll",
        "winhttp.dll", "wininet.dll", "winmm.dll", "winspool.drv", "ws2_32.dll",
        "wsock32.dll", "wtsapi32.dll",
    }

    def is_windows_system_dll(self, name: str) -> bool:
        lowered = name.lower()
        return lowered.startswith(("api-ms-win-", "ext-ms-win-")) or lowered in self.WINDOWS_SYSTEM_DLLS

    def packaged_binaries(self) -> list[Path]:
        return [path for path in self.config.output_dir.rglob("*")
                if path.is_file() and path.suffix.lower() in {".exe", ".dll"}]

    def copy_windows_dependencies(self, dependency_reader, dependency_finder):
        scanned: set[str] = set()
        unresolved: set[str] = set()
        while True:
            copied = False
            for binary in self.packaged_binaries():
                key = str(binary.resolve()).lower()
                if key in scanned:
                    continue
                scanned.add(key)
                for name in dependency_reader(binary):
                    if self.is_windows_system_dll(name) or (self.config.output_dir / name).exists():
                        continue
                    source = dependency_finder(name)
                    if source is None:
                        unresolved.add(name)
                        continue
                    shutil.copy2(source, self.config.output_dir / name)
                    unresolved.discard(name)
                    copied = True
                    self.logger.log(f"Bundled runtime DLL: {name}")
            if not copied:
                break
        if unresolved:
            raise FileNotFoundError("Runtime DLLs were not found: " + ", ".join(sorted(unresolved)))

    # The MSVC C runtime is not part of Windows, and every FFmpeg DLL vcpkg
    # builds as well as the application's own binary import VCRUNTIME140. The
    # archive calls itself portable, so the runtime has to travel with it
    # instead of asking the user to install a redistributable first.
    WINDOWS_RUNTIME_DLLS = ("vcruntime140.dll", "vcruntime140_1.dll", "msvcp140.dll")

    def windows_runtime_directories(self) -> list[Path]:
        directories: list[Path] = []
        redist = self.env.get("VCToolsRedistDir")
        if redist:
            root = Path(redist)
            # <VS>\VC\Redist\MSVC\<version>\x64\Microsoft.VC143.CRT
            directories.extend(sorted(root.glob("*/x64/Microsoft.VC*.CRT"), reverse=True))
            directories.extend(sorted(root.glob("x64/Microsoft.VC*.CRT"), reverse=True))
        system_root = Path(self.env.get("SystemRoot", os.environ.get("SystemRoot", r"C:\Windows")))
        directories.append(system_root / "System32")
        return directories

    def copy_windows_runtime(self):
        directories = self.windows_runtime_directories()
        for name in self.WINDOWS_RUNTIME_DLLS:
            destination = self.config.output_dir / name
            if destination.exists():
                continue
            source = next((directory / name for directory in directories
                           if (directory / name).is_file()), None)
            if source is None:
                # `msvcp140.dll` is only pulled in when something was compiled
                # against the C++ standard library, so its absence is legal.
                if name == "msvcp140.dll":
                    self.logger.log("msvcp140.dll was not found; nothing imports it")
                    continue
                raise FileNotFoundError(
                    f"{name} was not found; a portable archive cannot start without it"
                )
            shutil.copy2(source, destination)
            self.logger.log(f"Bundled MSVC runtime: {name}")


class Msys2Builder(WindowsDependencyMixin, PlatformBuilder):
    # MSYS2 is an explicit alternative, not the Windows build a release ships,
    # so it keeps a variant suffix rather than claiming the plain name.
    PLATFORM_OS = "windows"
    PLATFORM_ARCH = "x86_64"
    artifact_variant = "msys2"

    def install_dependencies(self):
        if self.config.is_offline:
            self.logger.log("Skipping dependency installation (--offline)")
            return
        if os.environ.get("MSYSTEM") != "UCRT64":
            raise RuntimeError("The --msys2 build must run from an MSYS2 UCRT64 shell")
        dependencies = [
            "mingw-w64-ucrt-x86_64-toolchain", "mingw-w64-ucrt-x86_64-rust",
            "mingw-w64-ucrt-x86_64-ffmpeg", "mingw-w64-ucrt-x86_64-pkgconf",
            "mingw-w64-ucrt-x86_64-binutils", "zip",
        ]
        self.run_cmd(["pacman", "-Syu", "--needed", "--noconfirm", *dependencies])

    def msys2_bin_directories(self) -> list[Path]:
        directories = []
        for name in ("MINGW_PREFIX", "MSYSTEM_PREFIX"):
            if value := self.env.get(name):
                directories.append(Path(value) / "bin")
        directories.extend(Path(part) for part in self.env.get("PATH", "").split(os.pathsep) if part)
        return list(dict.fromkeys(directories))

    def find_runtime_dll(self, name: str) -> Path | None:
        return next((directory / name for directory in self.msys2_bin_directories()
                     if (directory / name).is_file()), None)

    def imported_dlls(self, binary: Path) -> list[str]:
        objdump = shutil.which("objdump", path=self.env.get("PATH"))
        if not objdump:
            raise FileNotFoundError("objdump was not found in the MSYS2 UCRT64 environment")
        result = subprocess.run([objdump, "-p", str(binary)], capture_output=True, text=True,
                                errors="replace", env=self.env)
        return re.findall(r"DLL Name:\s*(\S+)", result.stdout) if result.returncode == 0 else []

    def package(self):
        self.prepare_output_dir()
        source = self.cargo_binary(windows=True)
        if not source.is_file():
            raise FileNotFoundError(f"Executable not found: {source}")
        destination = self.config.output_dir / self.packaged_executable_name(windows=True)
        shutil.copy2(source, destination)
        self.copy_resources(self.config.output_dir)
        if discovery := self.find_carla_discovery_tool(windows=True):
            shutil.copy2(discovery, self.config.output_dir / discovery.name)
        self.copy_windows_dependencies(self.imported_dlls, self.find_runtime_dll)
        self.copy_licenses(self.config.output_dir)
        self.write_build_info(self.config.output_dir)
        self.logger.log(f"Executable: {destination}")


class MsvcBuilder(WindowsDependencyMixin, PlatformBuilder):
    PLATFORM_OS = "windows"
    PLATFORM_ARCH = "x86_64"

    def __init__(self, config: BuildConfig, logger: Logger):
        super().__init__(config, logger)
        if os.name != "nt":
            raise RuntimeError("MSVC build can only run on Windows")
        self.vcpkg_root: Path | None = None
        self.vcpkg_triplet = "x64-windows"

    @staticmethod
    def parse_cmd_environment(output: str) -> dict[str, str]:
        parsed: dict[str, str] = {}
        uppercase_keys: set[str] = set()
        for line in output.splitlines():
            if "=" not in line:
                continue
            key, _, value = line.partition("=")
            normalized = key.upper()
            is_uppercase = key == normalized
            if normalized not in parsed or is_uppercase or normalized not in uppercase_keys:
                parsed[normalized] = value
            if is_uppercase:
                uppercase_keys.add(normalized)
        return parsed

    def find_vcvarsall(self) -> Path | None:
        candidates = [Path(value) for name in ("VCVARSALL", "VCVARSALL_BAT")
                      if (value := os.environ.get(name))]
        vswhere = Path(os.environ.get("ProgramFiles(x86)", r"C:\Program Files (x86)")) / "Microsoft Visual Studio/Installer/vswhere.exe"
        if vswhere.is_file():
            result = subprocess.run(
                [str(vswhere), "-latest", "-products", "*",
                 "-requires", "Microsoft.VisualStudio.Component.VC.Tools.x86.x64",
                 "-version", "[17.0,)", "-property", "installationPath"],
                capture_output=True, text=True, encoding=locale.getpreferredencoding(False), errors="replace",
            )
            if result.returncode == 0 and result.stdout.strip():
                candidates.append(Path(result.stdout.strip()) / "VC/Auxiliary/Build/vcvarsall.bat")
        candidates.extend(Path(rf"C:\Program Files\Microsoft Visual Studio\2022\{edition}\VC\Auxiliary\Build\vcvarsall.bat")
                          for edition in ("BuildTools", "Community", "Professional", "Enterprise"))
        return next((candidate for candidate in candidates if candidate.is_file()), None)

    def setup_msvc_environment(self):
        vcvarsall = self.find_vcvarsall()
        if not vcvarsall:
            raise RuntimeError("vcvarsall.bat not found; install Visual Studio C++ Build Tools")
        wrapper_path = None
        try:
            with tempfile.NamedTemporaryFile("w", suffix=".bat", delete=False, encoding="utf-8") as wrapper:
                wrapper.write(f'@call "{vcvarsall}" x64 > nul\n@set\n')
                wrapper_path = wrapper.name
            result = subprocess.run(
                ["cmd.exe", "/d", "/c", wrapper_path], capture_output=True, text=True,
                encoding=locale.getpreferredencoding(False), errors="replace",
            )
            if result.returncode != 0:
                raise RuntimeError(f"vcvarsall.bat failed: {result.stderr}")
            self.env.update(self.parse_cmd_environment(result.stdout))
            self.env.pop("Path", None)
            visual_studio_version = self.env.get("VISUALSTUDIOVERSION", "")
            try:
                visual_studio_major = int(visual_studio_version.split(".", 1)[0])
            except (ValueError, IndexError):
                visual_studio_major = 0
            if visual_studio_major < 17:
                raise RuntimeError(
                    "Visual Studio 2022 or newer is required for MSVC builds; "
                    f"detected VisualStudioVersion={visual_studio_version or 'unknown'}"
                )
        finally:
            if wrapper_path:
                Path(wrapper_path).unlink(missing_ok=True)

    def find_vcpkg_root(self) -> Path | None:
        candidates = [Path(value) for value in
                      (self.env.get("VCPKG_ROOT"), os.environ.get("VCPKG_INSTALLATION_ROOT")) if value]
        candidates.extend((Path(r"C:\vcpkg"), self.config.source_dir / "vcpkg"))
        return next((root for root in candidates if (root / "vcpkg.exe").is_file()), None)

    def find_libclang(self) -> Path | None:
        candidates: list[Path] = []
        if value := self.env.get("LIBCLANG_PATH"):
            candidates.append(Path(value))
        for variable in ("LLVMInstallDir", "VCINSTALLDIR", "VSINSTALLDIR"):
            if value := self.env.get(variable):
                base = Path(value)
                candidates.extend((base / "bin", base / "Tools/Llvm/x64/bin"))
        download_roots = [
            Path(value) / "tools/clang"
            for value in (self.env.get("VCPKG_DOWNLOADS"), os.environ.get("VCPKG_DOWNLOADS"))
            if value
        ]
        download_roots.extend((
            Path(self.env.get("LOCALAPPDATA", os.environ.get("LOCALAPPDATA", "")))
            / "vcpkg/downloads/tools/clang",
            self.vcpkg_root / "downloads/tools/clang" if self.vcpkg_root else Path(),
        ))
        for root in download_roots:
            if root.is_dir():
                candidates.extend(path.parent for path in root.glob("*/bin/libclang.dll"))
        if clang := shutil.which("clang.exe", path=self.env.get("PATH")):
            candidates.append(Path(clang).parent)
        for directory in dict.fromkeys(candidates):
            if (directory / "libclang.dll").is_file():
                return directory
        return None

    def export_github_actions_environment(self):
        github_env = os.environ.get("GITHUB_ENV")
        github_path = os.environ.get("GITHUB_PATH")
        if not github_env or not github_path:
            return

        with Path(github_env).open("a", encoding="utf-8") as environment_file:
            for name in ("FFMPEG_DIR", "LIBCLANG_PATH"):
                environment_file.write(f"{name}={self.env[name]}\n")
        with Path(github_path).open("a", encoding="utf-8") as path_file:
            path_file.write(f"{self.vcpkg_bin_directory()}\n")
        self.logger.log("Exported MSVC dependency paths for later GitHub Actions steps")

    def ffmpeg_release_log_paths(self, installed_root: Path) -> list[Path]:
        """Return the possible vcpkg log files for the FFmpeg Release build.

        vcpkg normally keeps buildtrees under its own checkout even when an
        alternate --x-install-root is used. Some versions/layouts instead put
        them below the install root, so support both locations.
        """
        roots = [
            installed_root / "vcpkg" / "buildtrees" / "ffmpeg",
            installed_root / "vcpkg" / "blds" / "ffmpeg",
            self.vcpkg_root / "buildtrees" / "ffmpeg" if self.vcpkg_root else None,
            self.vcpkg_root / "blds" / "ffmpeg" if self.vcpkg_root else None,
        ]
        paths: list[Path] = []
        for root in roots:
            if root is None:
                continue
            for suffix in ("out.log", "err.log"):
                path = root / f"build-{self.vcpkg_triplet}-rel-{suffix}"
                if path not in paths:
                    paths.append(path)
        return paths

    @staticmethod
    def read_log_tail(path: Path, max_bytes: int = 8192, max_lines: int = 1) -> str:
        try:
            with path.open("rb") as log_file:
                log_file.seek(0, os.SEEK_END)
                size = log_file.tell()
                log_file.seek(max(0, size - max_bytes))
                contents = log_file.read().decode(
                    locale.getpreferredencoding(False), errors="replace"
                )
        except OSError:
            return ""
        lines = [line.strip() for line in contents.splitlines() if line.strip()]
        return "\n".join(lines[-max_lines:]) if lines else ""

    def monitor_ffmpeg_release(self, installed_root: Path, stop_event: threading.Event):
        """Report vcpkg's otherwise captured FFmpeg Release build progress."""
        paths = self.ffmpeg_release_log_paths(installed_root)
        last_state = None
        last_report = 0.0
        while not stop_event.wait(10):
            existing = []
            for path in paths:
                try:
                    size = path.stat().st_size
                except OSError:
                    continue
                existing.append((path, size, self.read_log_tail(path)))

            if not existing:
                state = None
            else:
                state = tuple((str(path), size, tail) for path, size, tail in existing)

            now = time.monotonic()
            if state is not None and (state != last_state or now - last_report >= 30):
                details = "; ".join(
                    f"{path.name}: {size / 1024 / 1024:.1f} MiB"
                    + (f", latest: {tail}" if tail else "")
                    for path, size, tail in existing
                )
                self.logger.log(f"FFmpeg Release is still building ({details})")
                last_state = state
                last_report = now
            elif state is None and now - last_report >= 30:
                self.logger.log("FFmpeg Release is still building; waiting for vcpkg log files")
                last_state = None
                last_report = now

    def log_ffmpeg_failure(self, installed_root: Path):
        error_logs = [path for path in self.ffmpeg_release_log_paths(installed_root) if path.name.endswith("-err.log")]
        for path in error_logs:
            tail = self.read_log_tail(path, max_bytes=32768, max_lines=40)
            if tail:
                self.logger.log(f"FFmpeg Release error log: {path}")
                self.logger.log(tail)
                return

    def install_dependencies(self):
        self.setup_msvc_environment()
        self.vcpkg_root = self.find_vcpkg_root()
        if not self.vcpkg_root:
            raise RuntimeError("vcpkg.exe was not found; set VCPKG_ROOT")
        installed_root = self.config.work_dir / "vcpkg_installed"
        target_root = installed_root / self.vcpkg_triplet
        self.env.update({
            "VCPKG_ROOT": str(self.vcpkg_root),
            "VCPKGRS_TRIPLET": self.vcpkg_triplet,
            "FFMPEG_DIR": str(target_root),
        })
        if libclang_path := self.find_libclang():
            self.env["LIBCLANG_PATH"] = str(libclang_path)
            self.logger.log(f"Using libclang at {libclang_path}")
        else:
            raise RuntimeError(
                "libclang.dll was not found; install the LLVM/Clang component or "
                "let vcpkg acquire its clang tool"
            )
        self.export_github_actions_environment()
        if self.config.is_offline:
            if not (target_root / "include" / "libavcodec" / "avcodec.h").is_file():
                raise RuntimeError(f"Offline MSVC dependencies are incomplete: {target_root}")
            return
        stop_event = threading.Event()
        monitor = threading.Thread(
            target=self.monitor_ffmpeg_release,
            args=(installed_root, stop_event),
            name="ffmpeg-release-log-monitor",
            daemon=True,
        )
        self.logger.log("FFmpeg Release build output is captured by vcpkg; progress summaries will appear every 10 seconds")
        monitor.start()
        try:
            self.run_cmd([
                str(self.vcpkg_root / "vcpkg.exe"), "install", "--triplet", self.vcpkg_triplet,
                "--x-manifest-root", str(self.config.source_dir),
                "--x-install-root", str(installed_root),
            ])
        except subprocess.CalledProcessError:
            self.log_ffmpeg_failure(installed_root)
            raise
        finally:
            stop_event.set()
            monitor.join(timeout=2)

    def vcpkg_bin_directory(self) -> Path:
        return self.config.work_dir / "vcpkg_installed" / self.vcpkg_triplet / "bin"

    def find_runtime_dll(self, name: str) -> Path | None:
        candidate = self.vcpkg_bin_directory() / name
        return candidate if candidate.is_file() else None

    def imported_dlls(self, binary: Path) -> list[str]:
        dumpbin = shutil.which("dumpbin.exe", path=self.env.get("PATH"))
        if not dumpbin:
            raise FileNotFoundError("dumpbin.exe was not found in the MSVC environment")
        result = subprocess.run(
            [dumpbin, "/dependents", str(binary)], capture_output=True, text=True,
            encoding=locale.getpreferredencoding(False), errors="replace", env=self.env,
        )
        return re.findall(r"^\s+([^\s]+\.dll)\s*$", result.stdout, flags=re.IGNORECASE | re.MULTILINE)

    def package(self):
        self.prepare_output_dir()
        source = self.cargo_binary(windows=True)
        if not source.is_file():
            raise FileNotFoundError(f"Executable not found: {source}")
        destination = self.config.output_dir / self.packaged_executable_name(windows=True)
        shutil.copy2(source, destination)
        self.copy_resources(self.config.output_dir)
        if discovery := self.find_carla_discovery_tool(windows=True):
            shutil.copy2(discovery, self.config.output_dir / discovery.name)
        self.copy_windows_dependencies(self.imported_dlls, self.find_runtime_dll)
        self.copy_windows_runtime()
        self.copy_licenses(self.config.output_dir)
        self.write_build_info(self.config.output_dir)
        self.logger.log(f"Executable: {destination}")


class XcodeBuilder(PlatformBuilder):
    PLATFORM_OS = "macos"
    PLATFORM_ARCH = normalize_macos_architecture(platform.machine())

    # The oldest macOS the application requires. The value in Info.plist is not
    # taken on trust: packaging measures every Mach-O it is about to ship and
    # refuses a bundle whose real floor is higher than this, so the plist can
    # never promise an older system than the contents support.
    DEPLOYMENT_TARGET = "12.0"
    # App icon in the bundle. `tools/gen_app_icon.py` writes it from the same
    # SVG that produces the Windows ICO.
    APP_ICON_NAME = "icon.icns"

    def install_dependencies(self):
        if self.config.is_offline:
            self.logger.log("Skipping dependency installation (--offline)")
            return
        if not shutil.which("brew"):
            raise RuntimeError("Homebrew not found")
        # Rust comes from rustup and the pinned `rust-toolchain.toml`; letting
        # Homebrew provide a second `rust` would shadow that pin. `nasm` is only
        # needed to assemble the x86_64 FFmpeg when cross-compiling for Intel.
        self.run_cmd(["brew", "install", "pkgconf", "nasm"])
        self.build_ffmpeg()

    def ffmpeg_target(self) -> str:
        return self.build_target() or f"{platform.machine()}-apple-darwin"

    def build_ffmpeg(self):
        """Build the FFmpeg this bundle links against, at one pinned version.

        Homebrew's FFmpeg is built for whatever macOS the bottle targets, so its
        dylibs raise the bundle's floor regardless of what this project asks for,
        and the 9.x bottles no longer cover Intel at all. Building the pinned
        source keeps every architecture on one version, one deployment target,
        and one feature set.
        """
        script = self.config.source_dir / "scripts" / "build-ffmpeg.sh"
        if not script.is_file():
            raise FileNotFoundError(f"FFmpeg build script not found: {script}")
        prefix = self.config.source_dir / "build" / "ffmpeg" / self.ffmpeg_target()
        self.run_cmd([
            "bash", str(script), "--target", self.ffmpeg_target(), "--prefix", str(prefix),
        ], force_host=True)
        environment = {"FFMPEG_DIR": str(prefix)}
        self.env.update(environment)
        # Later steps in the pipeline read these back.
        self.export_environment(environment)

    def export_environment(self, variables: dict):
        """Hand variables to later GitHub Actions steps when running in CI."""
        github_env = os.environ.get("GITHUB_ENV")
        if not github_env:
            return
        with Path(github_env).open("a", encoding="utf-8") as handle:
            for name, value in variables.items():
                handle.write(f"{name}={value}\n")

    def cargo_environment(self) -> dict:
        # Without this the floor is whatever SDK the build host happened to have,
        # so the same commit produces different requirements on different runner
        # images. Every dependency compiled from source honours it as well.
        environment = {"MACOSX_DEPLOYMENT_TARGET": self.DEPLOYMENT_TARGET}
        if ffmpeg_dir := self.env.get("FFMPEG_DIR"):
            environment["FFMPEG_DIR"] = ffmpeg_dir
        return environment

    def write_info_plist(self, contents: Path, minimum_system: str):
        bundle_version = numeric_version(self.config.version_string)
        plist = f'''<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleDevelopmentRegion</key><string>en</string>
<key>CFBundleExecutable</key><string>freya-nle</string>
<key>CFBundleIconFile</key><string>{self.APP_ICON_NAME}</string>
<key>CFBundleIdentifier</key><string>io.github.gt610.freya-nle</string>
<key>CFBundleInfoDictionaryVersion</key><string>6.0</string>
<key>CFBundleName</key><string>Freya NLE</string>
<key>CFBundleDisplayName</key><string>Freya NLE</string>
<key>CFBundlePackageType</key><string>APPL</string>
<key>CFBundleShortVersionString</key><string>{bundle_version}</string>
<key>CFBundleVersion</key><string>{bundle_version}</string>
<key>CFBundleDocumentTypes</key><array>
<dict>
<key>CFBundleTypeName</key><string>Freya NLE Project</string>
<key>CFBundleTypeRole</key><string>Editor</string>
<key>LSHandlerRank</key><string>Owner</string>
<key>LSItemContentTypes</key><array><string>io.github.gt610.freya-nle.project</string></array>
<key>CFBundleTypeExtensions</key><array><string>aviqtl</string></array>
</dict>
<dict>
<key>CFBundleTypeName</key><string>Freya NLE Fragment</string>
<key>CFBundleTypeRole</key><string>Editor</string>
<key>LSHandlerRank</key><string>Owner</string>
<key>LSItemContentTypes</key><array><string>io.github.gt610.freya-nle.fragment</string></array>
<key>CFBundleTypeExtensions</key><array><string>freyafrag</string><string>aviqtlfrag</string></array>
</dict>
</array>
<key>UTExportedTypeDeclarations</key><array>
<dict>
<key>UTTypeIdentifier</key><string>io.github.gt610.freya-nle.project</string>
<key>UTTypeDescription</key><string>Freya NLE Project</string>
<key>UTTypeConformsTo</key><array><string>public.json</string></array>
<key>UTTypeTagSpecification</key><dict>
<key>public.filename-extension</key><array><string>aviqtl</string></array>
</dict>
</dict>
<dict>
<key>UTTypeIdentifier</key><string>io.github.gt610.freya-nle.fragment</string>
<key>UTTypeDescription</key><string>Freya NLE Fragment</string>
<key>UTTypeConformsTo</key><array><string>public.json</string></array>
<key>UTTypeTagSpecification</key><dict>
<key>public.filename-extension</key><array><string>freyafrag</string><string>aviqtlfrag</string></array>
</dict>
</dict>
</array>
<key>LSApplicationCategoryType</key><string>public.app-category.video</string>
<key>LSMinimumSystemVersion</key><string>{minimum_system}</string>
<key>NSHighResolutionCapable</key><true/>
</dict></plist>
'''
        (contents / "Info.plist").write_text(plist, encoding="utf-8")

    @staticmethod
    def macos_dependencies(binary: Path) -> list[str]:
        result = subprocess.run(["otool", "-L", str(binary)], capture_output=True,
                                text=True, errors="replace", check=True)
        return [line.strip().split(" (compatibility version", 1)[0]
                for line in result.stdout.splitlines()[1:] if line.strip()]

    @staticmethod
    def is_macos_system_dependency(path: str) -> bool:
        return path.startswith(("/System/", "/usr/lib/", "@executable_path/", "@loader_path/"))

    def bundle_macos_dependencies(self, executable: Path, frameworks: Path):
        frameworks.mkdir(parents=True, exist_ok=True)
        pending = [executable]
        scanned: set[Path] = set()
        while pending:
            binary = pending.pop()
            if binary in scanned:
                continue
            scanned.add(binary)
            for dependency in self.macos_dependencies(binary):
                if self.is_macos_system_dependency(dependency):
                    continue
                if dependency.startswith("@rpath/"):
                    if (frameworks / Path(dependency).name).exists():
                        continue
                    raise FileNotFoundError(f"Could not resolve macOS @rpath dependency: {dependency}")
                source = Path(dependency)
                if not source.exists():
                    raise FileNotFoundError(f"macOS dependency not found: {dependency}")
                destination = frameworks / source.name
                if not destination.exists():
                    shutil.copy2(source.resolve(), destination)
                    os.chmod(destination, destination.stat().st_mode | stat.S_IWUSR)
                    pending.append(destination)
                relative = f"@executable_path/../Frameworks/{destination.name}"
                self.run_cmd(["install_name_tool", "-change", dependency, relative, str(binary)])
        for dylib in frameworks.iterdir():
            self.run_cmd(["install_name_tool", "-id", f"@rpath/{dylib.name}", str(dylib)])

    def macos_app_bundle(self) -> Path:
        """The one bundle path packaging creates and archiving publishes."""
        return self.config.output_dir / "Freya NLE.app"

    @staticmethod
    def macho_minimum_system(path: Path) -> str:
        """Read the minimum macOS version a Mach-O was actually built for."""
        result = subprocess.run(["vtool", "-show-build", str(path)], capture_output=True,
                                text=True, errors="replace")
        versions = re.findall(r"^\s*minos ([0-9.]+)$", result.stdout, flags=re.MULTILINE)
        if not versions:
            return ""
        return max(versions, key=lambda value: tuple(int(part) for part in value.split(".")))

    def effective_minimum_system(self, app: Path) -> str:
        """The oldest macOS this bundle can actually start on.

        The application binary and every framework it loads each carry their
        own floor, and the bundle runs only on the strictest of them. Homebrew
        builds its bottles for the OS it was installed on, so a bundle can claim
        macOS 12 in `Info.plist` while shipping a library that needs macOS 15.
        """
        candidates = [self.macho_minimum_system(app / "Contents" / "MacOS" / "freya-nle")]
        frameworks = app / "Contents" / "Frameworks"
        if frameworks.is_dir():
            candidates.extend(self.macho_minimum_system(path) for path in sorted(frameworks.iterdir()))
        measured = [value for value in candidates if value]
        if not measured:
            return self.DEPLOYMENT_TARGET
        return max(measured, key=lambda value: tuple(int(part) for part in value.split(".")))

    def verify_minimum_system(self, app: Path, declared: str):
        """Fail the build when the bundle needs a newer macOS than it claims."""
        measured = self.effective_minimum_system(app)
        if tuple(int(part) for part in measured.split(".")) > tuple(
            int(part) for part in declared.split(".")
        ):
            raise RuntimeError(
                f"The bundle needs macOS {measured} but Info.plist declares {declared}. "
                "Rebuild the dependency against the deployment target or raise the "
                "declared minimum; do not ship a bundle that claims more than it supports."
            )
        self.logger.log(f"Minimum macOS: {declared} (measured {measured})")

    def package(self):
        self.prepare_output_dir()
        source = self.cargo_binary()
        if not source.is_file():
            raise FileNotFoundError(f"Executable not found: {source}")
        app = self.macos_app_bundle()
        contents = app / "Contents"
        macos = contents / "MacOS"
        resources = contents / "Resources"
        frameworks = contents / "Frameworks"
        macos.mkdir(parents=True)
        resources.mkdir(parents=True)
        executable = macos / "freya-nle"
        shutil.copy2(source, executable)
        self.copy_resources(resources)
        self.copy_app_icon(resources)
        self.copy_licenses(resources)
        self.write_build_info(resources)
        discovery = self.find_carla_discovery_tool()
        if discovery:
            shutil.copy2(discovery, macos / discovery.name)
        self.bundle_macos_dependencies(executable, frameworks)
        if discovery:
            self.bundle_macos_dependencies(macos / discovery.name, frameworks)
        # `Info.plist` is written after the frameworks are in place, because the
        # floor the bundle really has is only knowable once they are measurable.
        self.write_info_plist(contents, self.declared_minimum_system(app))
        self.sign_bundle(app)
        self.logger.log(f"App bundle: {app}")

    def declared_minimum_system(self, app: Path) -> str:
        """The version the bundle may honestly claim.

        A dependency built for a newer macOS raises the floor no matter what the
        project asked for, so the claim follows the measurement. When the
        deployment target is meant to hold (CI builds FFmpeg against it), an
        overshoot is an error instead of a quietly narrowed support range.
        """
        measured = self.effective_minimum_system(app)
        target = self.DEPLOYMENT_TARGET
        if tuple(int(part) for part in measured.split(".")) <= tuple(
            int(part) for part in target.split(".")
        ):
            self.logger.log(f"Minimum macOS: {target} (measured {measured})")
            return target
        message = (
            f"A bundled dependency needs macOS {measured}, above the {target} this "
            "build targets; the bundle now declares the higher version"
        )
        if self.config.strict_deployment_target:
            raise RuntimeError(message + " (--strict-deployment-target)")
        self.logger.log("Warning: " + message)
        return measured

    def sign_bundle(self, app: Path):
        """Ad-hoc sign, then verify what was written.

        The signing identity is confined here so a Developer ID and notarization
        can be added without touching packaging.
        """
        identity = self.env.get("MACOS_SIGN_IDENTITY", "-")
        self.run_cmd(["codesign", "--deep", "--force", "--sign", identity, str(app)])
        self.run_cmd(["codesign", "--verify", "--deep", "--strict", str(app)])

    def copy_app_icon(self, resources: Path):
        """Install the bundle icon, generating it from the SVG when absent."""
        icon = self.config.source_dir / "assets" / self.APP_ICON_NAME
        if not icon.is_file():
            generator = self.config.source_dir / "tools" / "gen_app_icon.py"
            if generator.is_file():
                self.logger.log("Generating the macOS icon from assets/icon.svg")
                self.run_cmd([sys.executable, str(generator)], force_host=True)
        if not icon.is_file():
            self.logger.log("No macOS icon available; the bundle will use the default")
            return
        shutil.copy2(icon, resources / self.APP_ICON_NAME)
        self.logger.log(f"Bundled app icon: {self.APP_ICON_NAME}")

    def create_zip(self, archive_name: str):
        archive_path = self.config.dist_dir / f"{archive_name}.zip"
        archive_path.unlink(missing_ok=True)
        self.run_cmd([
            "ditto", "-c", "-k", "--sequesterRsrc", "--keepParent",
            str(self.macos_app_bundle()), str(archive_path),
        ], force_host=True)


    def runtime_requirements(self) -> dict:
        requirements = super().runtime_requirements()
        requirements["minimum_macos"] = self.declared_minimum_system(self.macos_app_bundle())
        return requirements


BUILDERS: dict[str, Type[PlatformBuilder]] = {
    "appimage": AppImageBuilder,
    "arch": ArchBuilder,
    "msys2": Msys2Builder,
    "msvc": MsvcBuilder,
    "xcode": XcodeBuilder,
}


class BuildWorker(threading.Thread):
    def __init__(self, config: BuildConfig):
        super().__init__(daemon=True)
        self.config = config
        self.builder: Optional[PlatformBuilder] = None
        self.cancel_requested = False
        self.log_queue: queue.Queue = queue.Queue()
        self.finished_event = threading.Event()
        self.success = False
        self.error_msg = ""

    def run(self):
        try:
            self.builder = BUILDERS[self.config.target](
                self.config, Logger(self._enqueue_log, self._enqueue_progress)
            )
            if self.cancel_requested:
                self.builder.cancel()
            self.builder.build()
            self.success = True
            self.error_msg = "Build succeeded"
        except Exception as exc:
            self.error_msg = str(exc)
        finally:
            self.log_queue.put(None)
            self.finished_event.set()

    def _enqueue_log(self, message: str):
        self.log_queue.put(("log", message))

    def _enqueue_progress(self, value: int, message: str):
        self.log_queue.put(("progress", value, message))

    def cancel(self):
        self.cancel_requested = True
        if self.builder:
            self.builder.cancel()


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        prog="BUILD.py", description="Build and package the Freya NLE desktop application",
        formatter_class=argparse.RawTextHelpFormatter,
        epilog=("Examples:\n  python BUILD.py --arch\n  python BUILD.py --msvc --debug\n"
                "  python BUILD.py --msys2\n  python BUILD.py --xcode --offline\n"),
    )
    targets = parser.add_mutually_exclusive_group()
    targets.add_argument("--appimage", action="store_true",
                         help="Build the portable Linux AppImage")
    targets.add_argument("--arch", action="store_true", help="Build for Arch Linux")
    targets.add_argument("--msys2", action="store_true", help="Build for Windows MSYS2 UCRT64")
    targets.add_argument("--msvc", action="store_true", help="Build for Windows MSVC x64")
    targets.add_argument("--xcode", action="store_true", help="Build for macOS")
    parser.add_argument("--offline", action="store_true", help="Do not download dependencies")
    parser.add_argument("--debug", action="store_true", help="Build the debug profile")
    parser.add_argument("--no-container", action="store_true", help="Build Linux directly on the host")
    parser.add_argument("--version", type=str,
                        help="Validate the release version against rust/Cargo.toml")
    parser.add_argument("--strict-deployment-target", action="store_true",
                        help="Fail when a macOS dependency needs a newer system than targeted")
    return parser.parse_args()


def determine_target(args: argparse.Namespace, system_name: str | None = None) -> str | None:
    for name in ("appimage", "arch", "msys2", "msvc", "xcode"):
        if getattr(args, name):
            return name
    # An unqualified Linux build stays on the Arch-compatible path, which is
    # what the container tooling and the existing documentation assume; the
    # portable AppImage is asked for explicitly.
    return {"linux": "arch", "windows": "msvc", "darwin": "xcode"}.get(
        (system_name or platform.system()).lower()
    )


def main():
    for stream in (sys.stdout, sys.stderr):
        if hasattr(stream, "reconfigure"):
            stream.reconfigure(encoding="utf-8", errors="replace")
    args = parse_args()
    target = determine_target(args)
    if not target:
        print("Error: specify one of --appimage, --arch, --msys2, --msvc, --xcode")
        sys.exit(1)
    source_dir = Path.cwd()
    canonical_version = read_project_version(source_dir)
    version_string = args.version or canonical_version
    try:
        version_parts = parse_semver(version_string)
    except ValueError:
        print(f"Error: invalid semantic version '{version_string}'")
        sys.exit(1)
    if version_string != canonical_version:
        print(f"Error: --version {version_string} does not match Rust workspace version "
              f"{canonical_version}; update rust/Cargo.toml first")
        sys.exit(1)
    config = BuildConfig(
        source_dir=source_dir, temp_base=source_dir / ".build_tmp",
        output_dir=source_dir / "build", target=target, is_debug=args.debug,
        # Both Linux targets build inside a container: the Arch archive needs the
        # rolling-release sysroot, the AppImage needs a pinned old one.
        use_container=target in {"arch", "appimage"} and not args.no_container,
        is_offline=args.offline,
        version_major=version_parts[0], version_minor=version_parts[1],
        version_patch=version_parts[2], version_string=version_string,
        strict_deployment_target=args.strict_deployment_target,
    )
    worker = BuildWorker(config)
    cancelled = False

    def cancel_build():
        nonlocal cancelled
        if cancelled:
            os._exit(130)
        cancelled = True
        print("\nCancelling build...")
        worker.cancel()

    signal.signal(signal.SIGINT, lambda _signum, _frame: cancel_build())
    mode = "Container" if config.use_container else "Host"
    print(f"Build started | frontend=Slint | target={target} | {config.build_type} | "
          f"{mode} | offline={config.is_offline}")
    worker.start()
    while not worker.finished_event.is_set():
        try:
            message = worker.log_queue.get(timeout=0.2)
        except queue.Empty:
            continue
        if message is None:
            break
        print(message[1] if message[0] == "log" else f"[{message[1]}%] {message[2]}")
    while not worker.log_queue.empty():
        message = worker.log_queue.get_nowait()
        if message is not None:
            print(message[1] if message[0] == "log" else f"[{message[1]}%] {message[2]}")
    worker.join(timeout=3)
    if cancelled:
        print("\nBuild cancelled.")
        sys.exit(130)
    if not worker.success:
        print(f"\nBuild failed: {worker.error_msg}")
        sys.exit(1)


if __name__ == "__main__":
    main()
