import argparse
import io
import json
import os
import re
import shutil
import subprocess
import sys
import tarfile
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

from BUILD import (
    APPIMAGE_BASE_IMAGE,
    ARCH_LINUX_IMAGE,
    AppImageBuilder,
    ArchBuilder,
    BuildConfig,
    Logger,
    MsvcBuilder,
    Msys2Builder,
    PlatformBuilder,
    XcodeBuilder,
    determine_target,
    normalize_linux_architecture,
    normalize_macos_architecture,
    numeric_version,
    parse_semver,
    read_project_version,
)

SOURCE_DIR = Path(__file__).resolve().parent.parent


def config_for(source_dir: Path, *, debug=False, offline=False, target="test") -> BuildConfig:
    return BuildConfig(
        source_dir=source_dir,
        temp_base=source_dir / ".build_tmp",
        output_dir=source_dir / "build",
        target=target,
        is_debug=debug,
        use_container=False,
        is_offline=offline,
        version_string="0.7.0-rc.1",
    )


class TestBuildScript(unittest.TestCase):
    def test_accepts_valid_semver(self):
        cases = {
            "0.0.0": (0, 0, 0),
            "0.5.8": (0, 5, 8),
            "0.5.8-rc.1+build.2": (0, 5, 8),
            "1.0.0-0.3.7": (1, 0, 0),
        }
        for version, expected in cases.items():
            with self.subTest(version=version):
                self.assertEqual(parse_semver(version), expected)

    def test_rejects_invalid_semver(self):
        for version in ("01.2.3", "1.02.3", "1.2.03", "0.5.8-", "v1.2.3", "1.2"):
            with self.subTest(version=version), self.assertRaises(ValueError):
                parse_semver(version)

    def test_reads_rust_workspace_version(self):
        with tempfile.TemporaryDirectory() as temporary:
            source = Path(temporary)
            (source / "rust").mkdir()
            (source / "rust" / "Cargo.toml").write_text(
                '[workspace]\n[workspace.package]\nversion = "1.2.3-rc.1"\n', encoding="utf-8"
            )
            self.assertEqual(read_project_version(source), "1.2.3-rc.1")

    def test_prerelease_keeps_a_numeric_bundle_version(self):
        cases = {
            "0.6.2": "0.6.2",
            "0.7.0-rc.1": "0.7.0",
            "1.2.3+build.2": "1.2.3",
        }
        for version, expected in cases.items():
            with self.subTest(version=version):
                self.assertEqual(numeric_version(version), expected)

    def test_macos_plist_writes_a_numeric_version_for_a_candidate(self):
        with tempfile.TemporaryDirectory() as temporary:
            source = Path(temporary)
            contents = source / "Freya NLE.app" / "Contents"
            contents.mkdir(parents=True)
            config = config_for(source, target="xcode")
            config.version_string = "0.7.0-rc.1"
            builder = XcodeBuilder(config, Logger(lambda _message: None, lambda *_args: None))
            builder.write_info_plist(contents, "12.0")
            plist = (contents / "Info.plist").read_text(encoding="utf-8")
            # macOS accepts only period-separated integers in both version keys, so
            # a release candidate must not leak its suffix into the bundle metadata.
            self.assertIn("<key>CFBundleShortVersionString</key><string>0.7.0</string>", plist)
            self.assertIn("<key>CFBundleVersion</key><string>0.7.0</string>", plist)
            self.assertNotIn("rc.1", plist)

    def test_normalizes_macos_archive_architectures(self):
        for machine, expected in {
            "arm64": "arm64", "aarch64": "arm64", "x86_64": "x86_64", "AMD64": "x86_64"
        }.items():
            self.assertEqual(normalize_macos_architecture(machine), expected)
        with self.assertRaises(RuntimeError):
            normalize_macos_architecture("powerpc")

    def test_legacy_target_switches_remain_supported(self):
        defaults = {name: False for name in ("appimage", "arch", "msys2", "msvc", "xcode")}
        for target in defaults:
            values = {**defaults, target: True}
            self.assertEqual(determine_target(argparse.Namespace(**values), "unknown"), target)
        self.assertEqual(determine_target(argparse.Namespace(**defaults), "Darwin"), "xcode")
        self.assertEqual(determine_target(argparse.Namespace(**defaults), "Windows"), "msvc")
        self.assertEqual(determine_target(argparse.Namespace(**defaults), "linux"), "arch")
        # An unqualified Linux build stays Arch-compatible; the portable
        # AppImage is explicit because it builds in a different container.
        values = {**defaults, "appimage": True}
        self.assertEqual(determine_target(argparse.Namespace(**values), "linux"), "appimage")

    def test_cargo_command_builds_only_slint_frontend(self):
        with tempfile.TemporaryDirectory() as temporary:
            source = Path(temporary)
            builder = PlatformBuilder(
                config_for(source, offline=True), Logger(lambda _message: None, lambda *_args: None)
            )
            command = builder.get_cargo_build_cmd()
            self.assertEqual(command[:2], ["cargo", "build"])
            self.assertIn(str(source / "rust" / "Cargo.toml"), command)
            self.assertIn("freya-nle", command)
            self.assertIn("--release", command)
            self.assertIn("--offline", command)
            self.assertNotIn("cmake", " ".join(command).lower())
            self.assertNotIn("windeployqt", " ".join(command).lower())

    def test_debug_command_and_binary_path(self):
        with tempfile.TemporaryDirectory() as temporary:
            source = Path(temporary)
            builder = PlatformBuilder(
                config_for(source, debug=True), Logger(lambda _message: None, lambda *_args: None)
            )
            self.assertNotIn("--release", builder.get_cargo_build_cmd())
            self.assertEqual(
                builder.cargo_binary(),
                source / ".build_tmp" / "test" / "Debug" / "cargo-target" / "debug" / "freya-nle",
            )

    def test_resource_layout_matches_runtime_lookup(self):
        with tempfile.TemporaryDirectory() as temporary:
            source = Path(temporary)
            fixtures = {
                ("catalog", "effects", "blur.json"): "effect",
                ("catalog", "objects", "scene.json"): "object",
                ("plugins", "example", "main.lua"): "plugin",
                ("effect-packages", "weather", "manifest.json"): "package",
                ("repos", "catalog.json"): "repo",
            }
            for parts, contents in fixtures.items():
                path = source.joinpath(*parts)
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text(contents, encoding="utf-8")
            destination = source / "bundle"
            builder = PlatformBuilder(
                config_for(source), Logger(lambda _message: None, lambda *_args: None)
            )
            builder.copy_resources(destination)
            for parts, contents in fixtures.items():
                # `catalog/effects` and `catalog/objects` are staged as the
                # bundle-level `effects/` and `objects/` directories the
                # runtime scans, while the other roots keep their own name.
                packaged = (
                    destination.joinpath(parts[-2], parts[-1])
                    if parts[0] == "catalog"
                    else destination.joinpath(*parts)
                )
                self.assertEqual(packaged.read_text(encoding="utf-8"), contents)

    def test_packaged_executable_matches_the_macos_bundle(self):
        # The rename stopped at the macOS bundle: Linux and Windows packaging kept
        # writing "AviQtl"/"AviQtl.exe" into a Freya-NLE-* archive, so the shipped
        # executable and the archive name disagreed on every non-macOS build.
        with tempfile.TemporaryDirectory() as temporary:
            source = Path(temporary)
            builder = PlatformBuilder(
                config_for(source), Logger(lambda _message: None, lambda *_args: None)
            )
            self.assertEqual(builder.packaged_executable_name(), "freya-nle")
            self.assertEqual(builder.packaged_executable_name(windows=True), "freya-nle.exe")

            bundle = XcodeBuilder(
                config_for(source, target="xcode"),
                Logger(lambda _message: None, lambda *_args: None),
            )
            # The bundle executable and the loose binaries share one name, so an
            # archive can never disagree with the packaging that produced it.
            self.assertEqual(builder.packaged_executable_name(), "freya-nle")
            self.assertEqual(bundle.macos_app_bundle().name, "Freya NLE.app")

    def test_packagers_stage_the_executable_under_its_package_name(self):
        with tempfile.TemporaryDirectory() as temporary:
            source = Path(temporary)
            for name in ("LICENSE", "third-party-licenses.md", "licenses/GPL-2.0.txt",
                         "licenses/GPL-3.0.txt", "licenses/LGPL-2.1.txt"):
                path = source / name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text("fixture", encoding="utf-8")
            builder = ArchBuilder(
                config_for(source, target="arch"), Logger(lambda _message: None, lambda *_args: None)
            )
            staged = builder.cargo_binary()
            staged.parent.mkdir(parents=True, exist_ok=True)
            staged.write_text("binary", encoding="utf-8")
            builder.find_carla_discovery_tool = lambda *_args, **_kwargs: None
            builder.package()
            self.assertEqual(
                (source / "build" / "freya-nle").read_text(encoding="utf-8"), "binary"
            )

    def test_ci_verifies_the_executable_the_build_actually_writes(self):
        # msvc.yml asserted on `build\AviQtl.exe` while packaging wrote
        # `freya-nle.exe`, so the MSVC job failed after the rename landed. Keep
        # the workflow's expectations tied to the name the packager uses.
        workflow = (SOURCE_DIR / ".github" / "workflows" / "msvc.yml").read_text(encoding="utf-8")
        with tempfile.TemporaryDirectory() as temporary:
            source = Path(temporary)
            builder = PlatformBuilder(
                config_for(source), Logger(lambda _message: None, lambda *_args: None)
            )
            expected = builder.packaged_executable_name(windows=True)
        # Assert the guard itself, not the whole file: a matching name in a
        # comment or an error string would satisfy a substring search while
        # `Test-Path` still checked a file packaging no longer writes.
        self.assertRegex(
            workflow,
            rf'Test-Path -LiteralPath "build\\{re.escape(expected)}"',
        )
        self.assertNotIn("AviQtl.exe", workflow)

    def test_macos_archive_publishes_the_bundle_packaging_creates(self):
        # The rename left packaging writing "Freya NLE.app" while archiving still
        # looked for "AviQtl.app", so a macOS release failed at the archive step.
        with tempfile.TemporaryDirectory() as temporary:
            source = Path(temporary)
            builder = XcodeBuilder(
                config_for(source, target="xcode"),
                Logger(lambda _message: None, lambda *_args: None),
            )
            self.assertEqual(builder.macos_app_bundle().name, "Freya NLE.app")
            commands = []
            builder.run_cmd = lambda command, **_kwargs: commands.append(command)
            builder.create_zip("Freya-NLE-macOS-Xcode-test")
            self.assertEqual(len(commands), 1)
            archived, _destination = commands[0][-2:]
            self.assertEqual(Path(archived), builder.macos_app_bundle())

    def test_output_cleanup_preserves_the_directory(self):
        with tempfile.TemporaryDirectory() as temporary:
            source = Path(temporary)
            output = source / "build"
            nested = output / "nested"
            nested.mkdir(parents=True)
            (output / "old-file.txt").write_text("old", encoding="utf-8")
            (nested / "old-nested-file.txt").write_text("old", encoding="utf-8")
            original_directory = output.stat()
            builder = PlatformBuilder(
                config_for(source), Logger(lambda _message: None, lambda *_args: None)
            )

            builder.prepare_output_dir()

            self.assertTrue(output.is_dir())
            self.assertEqual(list(output.iterdir()), [])
            self.assertEqual(output.stat().st_ino, original_directory.st_ino)

    @unittest.skipUnless(os.name == "nt", "MSVC builder is only available on Windows")
    def test_msvc_exports_dependencies_for_later_github_actions_steps(self):
        with tempfile.TemporaryDirectory() as temporary:
            source = Path(temporary)
            github_env = source / "github-env"
            github_path = source / "github-path"
            builder = WindowsTestBuilder(
                config_for(source, target="msvc"),
                Logger(lambda _message: None, lambda *_args: None),
            )
            ffmpeg_dir = source / "vcpkg_installed" / "x64-windows"
            libclang_path = source / "clang" / "bin"
            builder.env.update({
                "FFMPEG_DIR": str(ffmpeg_dir),
                "LIBCLANG_PATH": str(libclang_path),
            })

            with patch.dict(
                os.environ,
                {
                    "GITHUB_ENV": str(github_env),
                    "GITHUB_PATH": str(github_path),
                },
            ):
                builder.export_github_actions_environment()

            self.assertEqual(
                github_env.read_text(encoding="utf-8").splitlines(),
                [f"FFMPEG_DIR={ffmpeg_dir}", f"LIBCLANG_PATH={libclang_path}"],
            )
            self.assertEqual(
                github_path.read_text(encoding="utf-8").splitlines(),
                [str(builder.vcpkg_bin_directory())],
            )

    def test_macos_plist_keeps_app_name_and_binary_name(self):
        with tempfile.TemporaryDirectory() as temporary:
            source = Path(temporary)
            contents = source / "Freya NLE.app" / "Contents"
            contents.mkdir(parents=True)
            builder = XcodeBuilder(
                config_for(source, target="xcode"), Logger(lambda _message: None, lambda *_args: None)
            )
            builder.write_info_plist(contents, "12.0")
            plist = (contents / "Info.plist").read_text(encoding="utf-8")
            self.assertIn("<string>Freya NLE</string>", plist)
            self.assertIn("<string>freya-nle</string>", plist)
            self.assertIn("<string>io.github.gt610.freya-nle</string>", plist)


class WindowsTestBuilder(MsvcBuilder):
    """An MSVC builder usable off Windows.

    The real constructor refuses to run anywhere but Windows, which is the right
    guard for a build tool. Naming, provenance, and runtime bundling are pure
    logic though, so they are exercised here without that guard rather than
    leaving the Windows path untested on the hosts that do most of the work.
    """

    def __init__(self, config, logger):
        PlatformBuilder.__init__(self, config, logger)
        self.vcpkg_root = None
        self.vcpkg_triplet = "x64-windows"


class TestArtifactNaming(unittest.TestCase):
    """One file-name scheme for every platform.

    A user picks "the Windows build for x86_64"; the toolchain that produced it
    is metadata, not part of the name. The Arch archive keeps a variant because
    it is compatible with a narrower host than "linux-x86_64" implies.
    """

    def builder(self, source, target):
        return {
            "arch": ArchBuilder,
            "msvc": WindowsTestBuilder,
            "msys2": Msys2Builder,
            "xcode": XcodeBuilder,
        }[target](
            config_for(source, target=target),
            Logger(lambda _message: None, lambda *_args: None),
        )

    def test_every_platform_follows_the_scheme(self):
        with tempfile.TemporaryDirectory() as temporary:
            source = Path(temporary)
            names = {
                target: self.builder(source, target).get_archive_name()
                for target in ("arch", "msvc", "msys2", "xcode")
            }
            self.assertEqual(names["msvc"], "Freya-NLE-0.7.0-rc.1-windows-x86_64")
            self.assertEqual(names["msys2"], "Freya-NLE-0.7.0-rc.1-windows-x86_64-msys2")
            self.assertEqual(names["arch"], "Freya-NLE-0.7.0-rc.1-linux-x86_64-archlinux")
            self.assertRegex(
                names["xcode"], r"^Freya-NLE-0\.7\.0-rc\.1-macos-(arm64|x86_64)$"
            )
            for name in names.values():
                # The toolchain that produced a build is metadata, not identity.
                self.assertNotIn("Xcode", name)
                self.assertNotIn("MSVC", name)
                self.assertNotIn("Universal", name)

    def test_arch_variant_goes_to_a_tar_zst(self):
        # `shutil.make_archive` drops the executable bit, so the Linux archive is
        # written with `tarfile`, which keeps it.
        with tempfile.TemporaryDirectory() as temporary:
            builder = self.builder(Path(temporary), "arch")
            self.assertEqual(builder.archive_extension, ".tar.zst")

    def test_archive_path_uses_the_declared_extension(self):
        with tempfile.TemporaryDirectory() as temporary:
            source = Path(temporary)
            builder = self.builder(source, "msvc")
            created = []
            builder.create_zip = lambda name: created.append(name)
            path = builder.create_archive("Freya-NLE-1.2.3-windows-x86_64")
            self.assertEqual(created, ["Freya-NLE-1.2.3-windows-x86_64"])
            self.assertEqual(path.suffix, ".zip")
            self.assertEqual(path.parent, source / "dist")


class TestArchiveContents(unittest.TestCase):
    def test_build_info_records_provenance(self):
        with tempfile.TemporaryDirectory() as temporary:
            source = Path(temporary)
            destination = source / "out"
            builder = WindowsTestBuilder(
                config_for(source, target="msvc"),
                Logger(lambda _message: None, lambda *_args: None),
            )
            builder.write_build_info(destination)
            info = json.loads((destination / "build-info.json").read_text(encoding="utf-8"))
            self.assertEqual(info["name"], "Freya NLE")
            self.assertEqual(info["version"], "0.7.0-rc.1")
            self.assertEqual(info["platform"], "windows")
            self.assertEqual(info["architecture"], "x86_64")
            self.assertIn("commit", info)
            self.assertIn("ffmpeg", info)

    def test_a_cross_build_records_the_target_cargo_was_given(self):
        # One arm64 runner builds the Intel bundle; `rustc -vV` would describe
        # the runner, not the archive.
        config = config_for(SOURCE_DIR, target="xcode")
        config.target_triple = "x86_64-apple-darwin"
        builder = PlatformBuilder(
            config, Logger(lambda _message: None, lambda *_args: None)
        )
        builder.command_output = (
            lambda command: "host: aarch64-apple-darwin\n" if command[0] == "rustc" else ""
        )
        self.assertEqual(builder.build_target(), "x86_64-apple-darwin")

    def test_the_recorded_ffmpeg_comes_from_the_linked_prefix(self):
        # `ffmpeg` on PATH is a different binary; describing it would misreport
        # what the archive actually contains.
        with tempfile.TemporaryDirectory() as temporary:
            source = Path(temporary)
            prefix = source / "ffmpeg"
            header = prefix / "include" / "libavutil" / "ffversion.h"
            header.parent.mkdir(parents=True)
            header.write_text('#define FFMPEG_VERSION "946fcce"\n', encoding="utf-8")
            (source / "scripts").mkdir()
            (source / "scripts" / "build-ffmpeg.sh").write_text(
                'FFMPEG_COMMIT="946fcce07b6dcd0331c8cc609192aeff5e1924f8"\n', encoding="utf-8")
            builder = PlatformBuilder(
                config_for(source), Logger(lambda _message: None, lambda *_args: None)
            )
            builder.env["FFMPEG_DIR"] = str(prefix)
            # A PATH ffmpeg would answer differently; make it answer at all and
            # the assertion below proves it was not consulted.
            builder.command_output = lambda command: (
                "ffmpeg version 4.4-ubuntu" if command[0] == "ffmpeg" else ""
            )
            provenance = builder.ffmpeg_provenance()
            self.assertEqual(provenance["source"], "ffmpeg-dir")
            self.assertEqual(provenance["version"], "946fcce")
            self.assertEqual(provenance["revision"], "946fcce07b6dcd0331c8cc609192aeff5e1924f8")

    def test_custom_ffmpeg_prefix_does_not_claim_the_recipe_revision(self):
        with tempfile.TemporaryDirectory() as temporary:
            source = Path(temporary)
            header = source / "ffmpeg" / "include" / "libavutil" / "ffversion.h"
            header.parent.mkdir(parents=True)
            header.write_text('#define FFMPEG_VERSION "9.0.1"\n', encoding="utf-8")
            (source / "scripts").mkdir()
            (source / "scripts" / "build-ffmpeg.sh").write_text(
                'FFMPEG_COMMIT="946fcce07b6dcd0331c8cc609192aeff5e1924f8"\n', encoding="utf-8")
            builder = PlatformBuilder(
                config_for(source), Logger(lambda _message: None, lambda *_args: None)
            )
            builder.env["FFMPEG_DIR"] = str(source / "ffmpeg")
            provenance = builder.ffmpeg_provenance()
            self.assertEqual(provenance["version"], "9.0.1")
            self.assertNotIn("revision", provenance)

    def test_a_system_ffmpeg_is_recorded_when_no_prefix_was_used(self):
        with tempfile.TemporaryDirectory() as temporary:
            builder = PlatformBuilder(
                config_for(Path(temporary)), Logger(lambda _message: None, lambda *_args: None)
            )
            builder.command_output = lambda command: "ffmpeg version 7.1.5 Copyright (c)"
            provenance = builder.ffmpeg_provenance()
            self.assertEqual(provenance["source"], "system")
            self.assertEqual(provenance["version"], "ffmpeg version 7.1.5 Copyright (c)")

    def test_repository_licences_are_staged(self):
        with tempfile.TemporaryDirectory() as temporary:
            destination = Path(temporary) / "out"
            builder = PlatformBuilder(
                config_for(SOURCE_DIR), Logger(lambda _message: None, lambda *_args: None)
            )
            builder.copy_licenses(destination)
            for name in ("LICENSE", "third-party-licenses.md", "GPL-2.0.txt",
                         "GPL-3.0.txt", "LGPL-2.1.txt"):
                self.assertTrue((destination / "licenses" / name).is_file(), name)
            self.assertEqual(
                (destination / "licenses" / "LICENSE").read_bytes(),
                (SOURCE_DIR / "LICENSE").read_bytes(),
            )

    def test_missing_required_licences_fail_before_staging(self):
        with tempfile.TemporaryDirectory() as temporary:
            source = Path(temporary)
            builder = PlatformBuilder(
                config_for(source), Logger(lambda _message: None, lambda *_args: None)
            )
            with self.assertRaisesRegex(FileNotFoundError, "Required licence files missing"):
                builder.copy_licenses(source / "out")
            self.assertFalse((source / "out").exists())


class TestAppImage(unittest.TestCase):
    """The portable Linux build.

    The Arch archive is linked against a rolling release, so the AppImage is the
    one a user on any distribution is expected to download. Its correctness
    rests on three things that can be checked without the container: the file
    name, the AppRun wrapper, and the pinned tool.
    """

    def builder(self, source):
        return AppImageBuilder(
            config_for(source, target="appimage"),
            Logger(lambda _message: None, lambda *_args: None),
        )

    def test_names_the_portable_build_without_a_variant(self):
        with tempfile.TemporaryDirectory() as temporary:
            builder = self.builder(Path(temporary))
            # No variant suffix: this is *the* Linux build, unlike the Archive.
            self.assertEqual(
                builder.get_archive_name(), "Freya-NLE-0.7.0-rc.1-linux-x86_64"
            )
            self.assertEqual(builder.archive_extension, ".AppImage")

    def test_an_unqualified_build_stays_x86_64_on_every_host(self):
        # The image is produced in a container pinned to linux/amd64, so the
        # machine running the build decides nothing. A default that followed the
        # host would rename the artifact on an Apple Silicon machine while still
        # producing an x86_64 payload.
        with tempfile.TemporaryDirectory() as temporary:
            builder = self.builder(Path(temporary))
            self.assertEqual(builder.platform_architecture(), "x86_64")
            self.assertEqual(builder.appimage_architecture(), "x86_64")
            self.assertEqual(builder.container_platform(), "linux/amd64")

    def test_the_arm64_build_is_named_for_its_architecture(self):
        with tempfile.TemporaryDirectory() as temporary:
            builder = self.builder(Path(temporary))
            builder.config.target_triple = "aarch64-unknown-linux-gnu"
            self.assertEqual(
                builder.get_archive_name(), "Freya-NLE-0.7.0-rc.1-linux-arm64"
            )
            self.assertEqual(builder.platform_architecture(), "arm64")
            # Artifact names say `arm64`, the two tools that consume the value
            # say `aarch64` and `arm64` respectively. Neither accepts the other's
            # spelling, which is why the mapping is not folded into one string.
            self.assertEqual(builder.appimage_architecture(), "aarch64")
            self.assertEqual(builder.container_platform(), "linux/arm64")
            self.assertEqual(builder.ffmpeg_target(), "aarch64-unknown-linux-gnu")

    def test_the_arm64_ffmpeg_prefix_does_not_collide_with_the_x86_64_one(self):
        # One checkout builds both images, and packaging reads `FFMPEG_DIR`. A
        # shared prefix would bundle x86_64 libraries into the arm64 image, which
        # links and then fails to load on the machine it was built for.
        with tempfile.TemporaryDirectory() as temporary:
            builder = self.builder(Path(temporary))
            prefixes = {}
            for triple in ("x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu"):
                builder.config.target_triple = triple
                prefixes[triple] = builder.ffmpeg_target()
            self.assertEqual(set(prefixes.values()),
                             {"x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu"})

    def test_an_unknown_linux_architecture_is_rejected(self):
        # Silently building the host architecture would label the artifact with an
        # architecture it does not have.
        with self.assertRaises(RuntimeError):
            normalize_linux_architecture("riscv64")

    def test_appdir_staging_puts_resources_next_to_the_executable(self):
        with tempfile.TemporaryDirectory() as temporary:
            source = Path(temporary)
            output = source / "build"
            output.mkdir()
            (output / "freya-nle").write_text("binary", encoding="utf-8")
            (output / "effects").mkdir()
            (output / "effects" / "blur.json").write_text("{}", encoding="utf-8")
            # Staging copies the pinned FFmpeg into the image, so give it one.
            library_dir = source / "ffmpeg" / "lib"
            library_dir.mkdir(parents=True)
            (library_dir / "libavutil.so.61").write_text("elf", encoding="utf-8")
            builder = self.builder(source)
            builder.env["FFMPEG_DIR"] = str(source / "ffmpeg")
            # The icon conversion shells out to an image tool; stub it out so the
            # staging logic is what is under test.
            builder.write_appimage_icon = lambda appdir: None
            builder.stage_appdir()
            appdir = builder.appdir_path()
            self.assertEqual(
                (appdir / "usr" / "bin" / "freya-nle").read_text(encoding="utf-8"), "binary"
            )
            # Resources must stay beside the executable: packaging copies them
            # from the build output, and the runtime looks next to the binary.
            self.assertTrue((appdir / "usr" / "bin" / "effects" / "blur.json").is_file())

    def test_apprun_sets_the_library_path_and_forwards_arguments(self):
        with tempfile.TemporaryDirectory() as temporary:
            builder = self.builder(Path(temporary))
            appdir = Path(temporary) / "FreyaNLE.AppDir"
            appdir.mkdir()
            builder.write_apprun(appdir)
            script = (appdir / "AppRun").read_text(encoding="utf-8")
            self.assertIn("LD_LIBRARY_PATH", script)
            self.assertIn('exec "$HERE/usr/bin/freya-nle" "$@"', script)
            # The mount point is not known until the image runs.
            self.assertIn('readlink -f "$0"', script)
            self.assertTrue(os.access(appdir / "AppRun", os.X_OK))

    def test_desktop_entry_names_the_icon_and_opens_projects(self):
        with tempfile.TemporaryDirectory() as temporary:
            builder = self.builder(Path(temporary))
            appdir = Path(temporary) / "FreyaNLE.AppDir"
            (appdir / "usr" / "share" / "applications").mkdir(parents=True)
            builder.write_desktop_entry(appdir)
            root = appdir / f"{builder.DESKTOP_ID}.desktop"
            self.assertTrue(root.is_file())
            text = root.read_text(encoding="utf-8")
            self.assertIn(f"Icon={builder.DESKTOP_ID}", text)
            self.assertIn("Categories=AudioVideo;Video;", text)
            self.assertIn("Exec=freya-nle %f", text)

    def test_the_image_carries_the_ffmpeg_it_was_linked_against(self):
        # The pinned FFmpeg's sonames exist on no distribution, so an image that
        # does not carry them cannot start anywhere.
        with tempfile.TemporaryDirectory() as temporary:
            source = Path(temporary)
            library_dir = source / "ffmpeg" / "lib"
            library_dir.mkdir(parents=True)
            real = library_dir / "libavcodec.so.63.1.102"
            real.write_text("elf", encoding="utf-8")
            (library_dir / "libavcodec.so.63").symlink_to(real.name)
            builder = self.builder(source)
            builder.env["FFMPEG_DIR"] = str(source / "ffmpeg")
            destination = source / "appdir" / "usr" / "lib"
            builder.bundle_linux_libraries(destination)
            self.assertEqual((destination / "libavcodec.so.63.1.102").read_text(encoding="utf-8"), "elf")
            link = destination / "libavcodec.so.63"
            self.assertTrue(link.is_symlink())
            # The link has to name the file beside it, not the build tree.
            self.assertEqual(os.readlink(link), "libavcodec.so.63.1.102")

    def test_the_container_builds_the_ffmpeg_it_links(self):
        # Ubuntu's FFmpeg is far too old for the crate, so the container path has
        # to build the pinned one or `ffmpeg-sys` has nothing to link.
        with tempfile.TemporaryDirectory() as temporary:
            config = config_for(Path(temporary), target="appimage")
            config.use_container = True
            builder = AppImageBuilder(config, Logger(lambda _m: None, lambda *_a: None))
            builder.create_podman_container = lambda: None
            builder.run_cmd = lambda *args, **kwargs: 0
            builder.install_rust = lambda: None
            built = []
            builder.build_ffmpeg = lambda: built.append(True)
            builder.install_dependencies()
            self.assertEqual(built, [True])

    def test_the_container_ffmpeg_prefix_is_apart_from_a_host_build(self):
        # The container shares the checkout through the mount, so a prefix named
        # only after the target would let it reuse an FFmpeg the host compiled
        # against its own glibc: the recipe's reuse check only asks whether the
        # prefix holds a pkg-config file. The container gets its own prefix and
        # `WORK_DIR`, so x264 and the source tree move with it while repeated
        # container builds still reuse what the last one left there.
        with tempfile.TemporaryDirectory() as temporary:
            config = config_for(Path(temporary), target="appimage")
            config.use_container = True
            builder = AppImageBuilder(config, Logger(lambda _m: None, lambda *_a: None))
            commands = []
            builder.run_cmd = lambda command, **kwargs: commands.append(command) or 0
            builder.build_ffmpeg()
            command = commands[-1][-1]
            self.assertIn("WORK_DIR=/src/.build_tmp/ffmpeg/container", command)
            self.assertIn(
                "--prefix /src/.build_tmp/ffmpeg/container/x86_64-unknown-linux-gnu",
                command,
            )
            # A host build of the same target keeps the host prefix to itself.
            self.assertNotIn(
                "--prefix /src/.build_tmp/ffmpeg/x86_64-unknown-linux-gnu", command
            )
            self.assertEqual(
                builder.env["FFMPEG_DIR"],
                str(Path(temporary) / ".build_tmp" / "ffmpeg" / "container"
                    / "x86_64-unknown-linux-gnu"),
            )

    def test_a_missing_ffmpeg_prefix_is_an_error_not_an_empty_image(self):
        with tempfile.TemporaryDirectory() as temporary:
            builder = self.builder(Path(temporary))
            builder.env.pop("FFMPEG_DIR", None)
            with self.assertRaises(RuntimeError):
                builder.bundle_linux_libraries(Path(temporary) / "lib")

    def test_the_container_is_told_where_cargo_writes_and_what_it_links(self):
        # `podman exec` starts from the container's own environment, so both have
        # to be exported into the command: otherwise cargo writes to its default
        # target directory, which packaging never reads, and `ffmpeg-sys` links
        # whatever FFmpeg the image happens to carry.
        with tempfile.TemporaryDirectory() as temporary:
            source = Path(temporary)
            config = config_for(source, target="appimage")
            config.use_container = True
            builder = AppImageBuilder(config, Logger(lambda _m: None, lambda *_a: None))
            builder.container_engine = "podman"
            builder.env["FFMPEG_DIR"] = str(source / ".build_tmp" / "ffmpeg" / "custom")
            builder.env["CARGO_TARGET_DIR"] = str(source / ".build_tmp" / "arch" / "cargo-target")
            environment = builder.container_environment()
            self.assertEqual(
                environment["FFMPEG_DIR"], "/src/.build_tmp/ffmpeg/custom"
            )
            self.assertEqual(
                environment["CARGO_TARGET_DIR"], "/src/.build_tmp/arch/cargo-target"
            )

    def test_a_configured_prefix_is_not_replaced_by_a_default(self):
        # The prefix the caller exported is the one to translate; substituting a
        # default would silently link a different FFmpeg.
        with tempfile.TemporaryDirectory() as temporary:
            source = Path(temporary)
            config = config_for(source, target="appimage")
            config.use_container = True
            builder = AppImageBuilder(config, Logger(lambda _m: None, lambda *_a: None))
            builder.container_engine = "podman"
            custom = source / "vendor" / "ffmpeg"
            builder.env["FFMPEG_DIR"] = str(custom)
            self.assertEqual(
                builder.container_environment()["FFMPEG_DIR"], "/src/vendor/ffmpeg"
            )
            # The host-side value packaging uses is untouched.
            self.assertEqual(builder.env["FFMPEG_DIR"], str(custom))

    def test_distrobox_keeps_host_paths_for_the_build_variables(self):
        with tempfile.TemporaryDirectory() as temporary:
            source = Path(temporary)
            config = config_for(source, target="appimage")
            config.use_container = True
            builder = AppImageBuilder(config, Logger(lambda _m: None, lambda *_a: None))
            builder.container_engine = "distrobox"
            builder.env["FFMPEG_DIR"] = str(source / ".build_tmp" / "ffmpeg" / "x")
            self.assertEqual(
                builder.container_environment()["FFMPEG_DIR"],
                str(source / ".build_tmp" / "ffmpeg" / "x"),
            )

    def test_the_tool_is_pinned_by_checksum(self):
        # A downloaded build tool is a supply chain entry point, so the digest is
        # part of the source and a mismatch has to stop the build.
        with tempfile.TemporaryDirectory() as temporary:
            builder = self.builder(Path(temporary))
            for architecture, spec in builder.IMAGE_TOOLS.items():
                with self.subTest(architecture=architecture):
                    self.assertRegex(spec["sha256"], r"^[0-9a-f]{64}$")
                    # The tool is an AppImage itself, so the name has to say which
                    # architecture it runs on; one pin cannot serve both builds.
                    self.assertIn(architecture, spec["name"])
            self.assertTrue(builder.image_tool_url().startswith("https://"))
            # A tagged release cannot be swapped under the pinned digest the way
            # the rolling `continuous` asset can.
            self.assertNotIn("/continuous/", builder.image_tool_url())

    def test_each_architecture_packs_with_its_own_tool(self):
        # An x86_64 appimagetool cannot run in the arm64 container, and using the
        # wrong one would either fail to start or embed the wrong runtime.
        with tempfile.TemporaryDirectory() as temporary:
            builder = self.builder(Path(temporary))
            chosen = {}
            for triple in ("x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu"):
                builder.config.target_triple = triple
                chosen[triple] = (builder.appimage_architecture(),
                                  builder.image_tool()["name"],
                                  builder.image_tool_url())
            self.assertEqual(chosen["x86_64-unknown-linux-gnu"][:2],
                             ("x86_64", "appimagetool-x86_64.AppImage"))
            self.assertEqual(chosen["aarch64-unknown-linux-gnu"][:2],
                             ("aarch64", "appimagetool-aarch64.AppImage"))
            # Two digests, so a swapped asset can never satisfy the other build.
            self.assertNotEqual(chosen["x86_64-unknown-linux-gnu"][2],
                                chosen["aarch64-unknown-linux-gnu"][2])

    def test_a_corrupt_tool_is_rejected(self):
        with tempfile.TemporaryDirectory() as temporary:
            source = Path(temporary)
            builder = self.builder(source)
            tool = source / ".build_tmp" / builder.image_tool()["name"]
            tool.parent.mkdir(parents=True)
            tool.write_text("not an appimage", encoding="utf-8")
            builder.IMAGE_TOOLS = {
                "x86_64": dict(builder.IMAGE_TOOLS["x86_64"], sha256="0" * 64)
            }
            builder.download = lambda url, destination: destination.write_text("x", encoding="utf-8")
            with self.assertRaises(RuntimeError):
                builder.appimagetool_path()

    def test_dependencies_are_declared_for_the_portable_track(self):
        # Only ALSA is a host library the image cannot carry; the graphics stack
        # is the driver's business.
        with tempfile.TemporaryDirectory() as temporary:
            builder = self.builder(Path(temporary))
            builder.maximum_glibc_version = lambda: "2.35"
            builder.ffmpeg_sonames = lambda: []
            requirements = builder.runtime_requirements()
            self.assertEqual(requirements["host_libraries"], ["libasound.so.2"])
            self.assertEqual(requirements["glibc"], "2.35")


class TestMacosMinimumSystem(unittest.TestCase):
    """`LSMinimumSystemVersion` must not promise more than the bundle supports.

    Before this check the plist said 12.0 while every Homebrew dylib in the
    shipped archive needed macOS 15.0, so the bundle refused to start on the
    systems it claimed to support.
    """

    def builder(self, source):
        return XcodeBuilder(
            config_for(source, target="xcode"), Logger(lambda _message: None, lambda *_args: None)
        )

    def test_reads_the_highest_declared_minimum(self):
        with tempfile.TemporaryDirectory() as temporary:
            builder = self.builder(Path(temporary))
            reported = {"path": ""}
            builder.macho_minimum_system = lambda path: reported["path"]
            app = Path(temporary) / "Freya NLE.app"
            frameworks = app / "Contents" / "Frameworks"
            frameworks.mkdir(parents=True)
            (app / "Contents" / "MacOS").mkdir(parents=True)
            (app / "Contents" / "MacOS" / "freya-nle").write_text("x", encoding="utf-8")
            (frameworks / "libavcodec.63.dylib").write_text("x", encoding="utf-8")
            reported["path"] = "15.0"
            self.assertEqual(builder.effective_minimum_system(app), "15.0")

    def test_a_helper_executable_is_measured_too(self):
        # `carla-discovery-native` ships beside the application binary, so a
        # floor only it declares would otherwise never reach the plist.
        with tempfile.TemporaryDirectory() as temporary:
            builder = self.builder(Path(temporary))
            app = Path(temporary) / "Freya NLE.app"
            macos = app / "Contents" / "MacOS"
            macos.mkdir(parents=True)
            (macos / "freya-nle").write_text("x", encoding="utf-8")
            (macos / "carla-discovery-native").write_text("x", encoding="utf-8")
            floors = {"freya-nle": "12.0", "carla-discovery-native": "14.1"}
            builder.macho_minimum_system = lambda path: floors.get(Path(path).name, "")
            self.assertEqual(builder.effective_minimum_system(app), "14.1")

    def test_a_thin_binary_contributes_its_floor(self):
        # A thin file reports one labelled block too; an unlabelled parse used to
        # drop it, which silently disabled the overshoot check for the archives.
        with tempfile.TemporaryDirectory() as temporary:
            config = config_for(Path(temporary), target="xcode")
            config.target_triple = "aarch64-apple-darwin"
            builder = XcodeBuilder(
                config, Logger(lambda _message: None, lambda *_args: None)
            )
            report = "/x/frey (architecture arm64):\n   minos 12.0\n"
            with patch("BUILD.subprocess.run") as run:
                run.return_value = subprocess.CompletedProcess([], 0, report, "")
                self.assertEqual(builder.macho_minimum_system(Path("frey")), "12.0")
                self.assertIn("-arch", run.call_args.args[0])
                self.assertIn("arm64", run.call_args.args[0])

    def test_only_the_slice_this_bundle_loads_is_measured(self):
        # A universal file can carry a higher floor in a slice this bundle never
        # executes; adopting it would narrow the declared minimum for nothing.
        with tempfile.TemporaryDirectory() as temporary:
            config = config_for(Path(temporary), target="xcode")
            config.target_triple = "aarch64-apple-darwin"
            builder = XcodeBuilder(
                config, Logger(lambda _message: None, lambda *_args: None)
            )
            report = (
                "/x/frey (architecture arm64):\n   minos 12.0\n"
                "/x/frey (architecture x86_64):\n   minos 15.0\n"
            )
            with patch("BUILD.subprocess.run") as run:
                run.return_value = subprocess.CompletedProcess([], 0, report, "")
                self.assertEqual(builder.macho_minimum_system(Path("frey")), "12.0")

    def test_declares_the_measured_floor_when_a_dependency_overshoots(self):
        with tempfile.TemporaryDirectory() as temporary:
            builder = self.builder(Path(temporary))
            builder.effective_minimum_system = lambda app: "15.0"
            self.assertEqual(builder.declared_minimum_system(Path("app")), "15.0")

    def test_strict_mode_refuses_the_overshoot(self):
        with tempfile.TemporaryDirectory() as temporary:
            config = config_for(Path(temporary), target="xcode")
            config.strict_deployment_target = True
            builder = XcodeBuilder(
                config, Logger(lambda _message: None, lambda *_args: None)
            )
            builder.effective_minimum_system = lambda app: "15.0"
            with self.assertRaises(RuntimeError):
                builder.declared_minimum_system(Path("app"))

    def test_a_bundle_inside_the_target_keeps_the_target(self):
        with tempfile.TemporaryDirectory() as temporary:
            builder = self.builder(Path(temporary))
            builder.effective_minimum_system = lambda app: "11.0"
            self.assertEqual(builder.declared_minimum_system(Path("app")), builder.DEPLOYMENT_TARGET)


class TestMacosBundling(unittest.TestCase):
    """Relocating the libraries is what makes the bundle runnable elsewhere.

    Three specific mistakes here produce a bundle that signs cleanly and then
    refuses to start, which is how the previous release shipped a broken floor.
    """

    def builder(self, source):
        return XcodeBuilder(
            config_for(source, target="xcode"), Logger(lambda _m: None, lambda *_a: None)
        )

    def test_a_resolved_library_keeps_the_name_it_is_referred_to_by(self):
        # FFmpeg installs `libswscale.10.dylib` as a symlink to the fully
        # versioned file. Collapsing it to the target name leaves the bundle's
        # libraries disagreeing about each other's names.
        with tempfile.TemporaryDirectory() as temporary:
            source = Path(temporary)
            lib = source / "lib" / "libswscale.10.dylib"
            lib.parent.mkdir(parents=True)
            lib.write_text("dylib", encoding="utf-8")
            builder = self.builder(source)
            builder.env["FFMPEG_DIR"] = str(source)
            resolved = builder.resolve_rpath_dependency("@rpath/libswscale.10.dylib")
            self.assertIsNotNone(resolved)
            self.assertEqual(resolved.name, "libswscale.10.dylib")

    def test_the_link_leaves_room_to_rewrite_library_paths(self):
        # `install_name_tool` cannot lengthen a load command unless the linker
        # reserved space for it, so the flag has to be in the repository config
        # rather than in someone's shell.
        config = (SOURCE_DIR / ".cargo" / "config.toml").read_text(encoding="utf-8")
        self.assertIn("headerpad_max_install_names", config)
        for triple in ("aarch64-apple-darwin", "x86_64-apple-darwin"):
            with self.subTest(triple=triple):
                self.assertIn(f"[target.{triple}]", config)

    def test_the_build_does_not_override_the_repository_link_flags(self):
        with tempfile.TemporaryDirectory() as temporary:
            builder = self.builder(Path(temporary))
            environment = builder.cargo_environment()
            # RUSTFLAGS would replace the config file's rustflags wholesale.
            self.assertNotIn("RUSTFLAGS", environment)
            self.assertEqual(environment["MACOSX_DEPLOYMENT_TARGET"], builder.DEPLOYMENT_TARGET)

    def test_packaging_refuses_to_delete_the_libraries_it_links(self):
        # The FFmpeg prefix must not live under `build/`, which packaging empties.
        with tempfile.TemporaryDirectory() as temporary:
            source = Path(temporary)
            builder = self.builder(source)
            builder.env["FFMPEG_DIR"] = str(source / "build" / "ffmpeg" / "arm64")
            with self.assertRaises(RuntimeError):
                builder.prepare_output_dir()

    def test_a_prefix_outside_the_output_directory_is_allowed(self):
        with tempfile.TemporaryDirectory() as temporary:
            source = Path(temporary)
            builder = self.builder(source)
            builder.env["FFMPEG_DIR"] = str(source / ".build_tmp" / "ffmpeg" / "arm64")
            builder.prepare_output_dir()
            self.assertTrue((source / "build").is_dir())


class TestWindowsPortability(unittest.TestCase):
    def test_runtime_dlls_are_the_ones_the_binaries_import(self):
        with tempfile.TemporaryDirectory() as temporary:
            builder = WindowsTestBuilder(
                config_for(Path(temporary), target="msvc"),
                Logger(lambda _message: None, lambda *_args: None),
            )
            # The shipped archive imported VCRUNTIME140.dll and never bundled it,
            # so "portable" depended on an installed redistributable.
            self.assertIn("vcruntime140.dll", builder.WINDOWS_RUNTIME_DLLS)
            self.assertIn("vcruntime140_1.dll", builder.WINDOWS_RUNTIME_DLLS)

    def test_a_missing_vcruntime_fails_the_packaging(self):
        # The archive cannot start without the CRT, so silently omitting it would
        # ship a "portable" build that only runs where VS happens to be installed.
        with tempfile.TemporaryDirectory() as temporary:
            source = Path(temporary)
            (source / "build").mkdir()
            builder = WindowsTestBuilder(
                config_for(source, target="msvc"),
                Logger(lambda _message: None, lambda *_args: None),
            )
            builder.windows_runtime_directories = lambda: []
            with self.assertRaises(FileNotFoundError):
                builder.copy_windows_runtime()

    def test_a_missing_msvcp_is_tolerated_when_nothing_imports_it(self):
        # msvcp140 is only imported when something was compiled against the C++
        # standard library, so its absence is legal and must not fail a build.
        with tempfile.TemporaryDirectory() as temporary:
            source = Path(temporary)
            output = source / "build"
            output.mkdir()
            builder = WindowsTestBuilder(
                config_for(source, target="msvc"),
                Logger(lambda _message: None, lambda *_args: None),
            )
            available = source / "redist"
            available.mkdir()
            for name in ("vcruntime140.dll", "vcruntime140_1.dll"):
                (available / name).write_text("dll", encoding="utf-8")
            builder.windows_runtime_directories = lambda: [available]
            builder.copy_windows_runtime(lambda binary: [])
            self.assertTrue((output / "vcruntime140.dll").is_file())
            self.assertFalse((output / "msvcp140.dll").exists())

    def test_a_missing_msvcp_fails_when_a_binary_imports_it(self):
        # Shipping it missing would make the "portable" archive need a separately
        # installed redistributable.
        with tempfile.TemporaryDirectory() as temporary:
            source = Path(temporary)
            output = source / "build"
            output.mkdir()
            (output / "freya-nle.exe").write_text("pe", encoding="utf-8")
            builder = WindowsTestBuilder(
                config_for(source, target="msvc"),
                Logger(lambda _message: None, lambda *_args: None),
            )
            available = source / "redist"
            available.mkdir()
            for name in ("vcruntime140.dll", "vcruntime140_1.dll"):
                (available / name).write_text("dll", encoding="utf-8")
            builder.windows_runtime_directories = lambda: [available]
            with self.assertRaises(FileNotFoundError):
                builder.copy_windows_runtime(lambda binary: ["MSVCP140.dll"])

    def test_the_redistributable_directory_is_found_through_its_normalised_key(self):
        # vcvarsall output is upper-cased, so the mixed-case name never matches.
        with tempfile.TemporaryDirectory() as temporary:
            builder = WindowsTestBuilder(
                config_for(Path(temporary), target="msvc"),
                Logger(lambda _message: None, lambda *_args: None),
            )
            builder.env.pop("VCToolsRedistDir", None)
            redist = Path(temporary) / "redist"
            crt = redist / "14.44.35207" / "x64" / "Microsoft.VC143.CRT"
            crt.mkdir(parents=True)
            builder.env["VCTOOLSREDISTDIR"] = str(redist)
            # The CRT directory the vcvarsall key points at is searched first.
            self.assertEqual(builder.windows_runtime_directories()[0], crt)


class TestContainerBuild(unittest.TestCase):
    """The podman path has to behave like the distrobox one.

    `apt-get install` only survives to the next command if every command shares
    one container, and a command built from host paths is not valid inside a
    container that only sees a mount.
    """

    def builder(self, source, target="appimage"):
        return AppImageBuilder(
            config_for(source, target=target),
            Logger(lambda _message: None, lambda *_args: None),
        )

    def test_host_paths_are_rewritten_for_the_container(self):
        with tempfile.TemporaryDirectory() as temporary:
            source = Path(temporary)
            builder = self.builder(source)
            builder.container_engine = "podman"
            command = ["cargo", "build", "--manifest-path", str(source / "rust" / "Cargo.toml")]
            translated = builder.container_command(command)
            self.assertEqual(
                translated[-1], "/src/rust/Cargo.toml",
            )
            # The container mounts the tree at /src, so that is where the shell
            # has to start as well.
            self.assertEqual(builder.container_source_dir(), "/src")

    def test_a_windows_host_path_becomes_a_posix_path(self):
        # The container is Linux: a translated path that still carries
        # backslashes is not a path the container can open.
        with tempfile.TemporaryDirectory() as temporary:
            builder = self.builder(Path(temporary))
            builder.container_engine = "podman"
            translated = builder.container_path(r"D:\a\Freya-NLE\rust\Cargo.toml",
                                                r"D:\a\Freya-NLE")
            self.assertEqual(translated, "/src/rust/Cargo.toml")

    def test_the_source_root_itself_becomes_the_mount_point(self):
        with tempfile.TemporaryDirectory() as temporary:
            builder = self.builder(Path(temporary))
            builder.container_engine = "podman"
            self.assertEqual(builder.container_path("/home/x/repo", "/home/x/repo"), "/src")
            # A path that does not start with the tree is left alone.
            self.assertEqual(builder.container_path("/usr/bin/cargo", "/home/x/repo"),
                             "/usr/bin/cargo")

    def test_distrobox_keeps_host_paths(self):
        # distrobox shares the host filesystem, so rewriting its paths would
        # break a build that works today.
        with tempfile.TemporaryDirectory() as temporary:
            source = Path(temporary)
            builder = self.builder(source)
            builder.container_engine = "distrobox"
            command = ["cargo", "build", "--manifest-path", str(source / "rust" / "Cargo.toml")]
            self.assertEqual(builder.container_command(command), command)
            self.assertEqual(builder.container_source_dir(), str(source))

    def test_every_command_targets_one_long_lived_container(self):
        with tempfile.TemporaryDirectory() as temporary:
            builder = self.builder(Path(temporary))
            builder.container_engine = "podman"
            builder.container_name = "freya-nle-test"
            first = builder.container_invocation("echo one")
            second = builder.container_invocation("echo two")
            # `podman exec` into a named container, not a fresh `podman run`:
            # the latter would lose everything the previous command installed.
            self.assertEqual(first[:3], ["podman", "exec", "-w"])
            self.assertIn("freya-nle-test", first)
            self.assertEqual(second[:2], ["podman", "exec"])
            self.assertNotIn("--rm", first)
            # An argument list, because the program it carries is whole: a host
            # shell would have to re-parse it, and `cmd.exe` cannot.
            self.assertIsInstance(first, list)
            self.assertEqual(first[-1], "echo one")

    def test_a_failed_command_can_be_tolerated(self):
        with tempfile.TemporaryDirectory() as temporary:
            builder = self.builder(Path(temporary))
            builder.container_engine = "podman"
            code = builder.run_cmd([sys.executable, "-c", "raise SystemExit(3)"],
                                   allow_failure=True)
            self.assertEqual(code, 3)
            with self.assertRaises(subprocess.CalledProcessError):
                builder.run_cmd([sys.executable, "-c", "raise SystemExit(3)"])

    def test_the_created_container_is_named_after_this_process(self):
        with tempfile.TemporaryDirectory() as temporary:
            builder = self.builder(Path(temporary))
            builder.run_cmd = lambda *_args, **_kwargs: 0
            builder.create_podman_container()
            # A shared name is a name a second build can `rm --force`.
            self.assertEqual(
                builder.podman_container, f"{builder.container_name}-{os.getpid()}"
            )
            self.assertTrue(builder.owns_container)
            self.assertIn(builder.podman_container, builder.container_invocation("true"))

    def test_a_container_this_build_did_not_create_is_left_alone(self):
        # `--appimage --no-container` on a host that has podman creates nothing,
        # so cleanup must not remove a container it never made.
        with tempfile.TemporaryDirectory() as temporary:
            builder = self.builder(Path(temporary))
            with patch("BUILD.subprocess.run") as run, \
                    patch("BUILD.shutil.which", return_value="/usr/bin/podman"):
                builder.remove_podman_container()
            run.assert_not_called()
            builder.owns_container = True
            builder.podman_container = "freya-nle-appimage-1"
            with patch("BUILD.subprocess.run") as run, \
                    patch("BUILD.shutil.which", return_value="/usr/bin/podman"):
                builder.remove_podman_container()
            self.assertEqual(
                run.call_args.args[0],
                ["podman", "container", "rm", "--force", "freya-nle-appimage-1"],
            )
            self.assertFalse(builder.owns_container)

    def test_a_cancelled_build_still_removes_its_container(self):
        # `run_cmd` refuses to start once `cancel()` has fired, which is exactly
        # the state an interrupted build is in when the container has to go.
        with tempfile.TemporaryDirectory() as temporary:
            builder = self.builder(Path(temporary))
            builder.cancelled = True
            builder.owns_container = True
            builder.podman_container = "freya-nle-appimage-2"
            with patch("BUILD.subprocess.run") as run, \
                    patch("BUILD.shutil.which", return_value="/usr/bin/podman"):
                builder.cleanup()
            run.assert_called_once()

    def packed_members(self, builder):
        """Pack the staged tree and return the tar entries, keyed by name."""
        source = builder.config.source_dir
        staged = builder.config.output_dir
        staged.mkdir(parents=True, exist_ok=True)
        (source / "dist").mkdir(exist_ok=True)
        executable = staged / "freya-nle"
        executable.write_text("#!/bin/sh\n", encoding="utf-8")
        executable.chmod(0o755)
        captured = []

        def capture(command, **_kwargs):
            # `create_tar_zst` deletes the tar after zstd; keep its bytes.
            tar_argument = next(arg for arg in command if str(arg).endswith(".tar"))
            captured.append(Path(tar_argument).read_bytes())
            return 0

        builder.run_cmd = capture
        with patch("BUILD.shutil.which", return_value="/usr/bin/zstd"):
            builder.create_tar_zst("Freya-NLE-0.7.0-linux-x86_64-archlinux")
        with tarfile.open(fileobj=io.BytesIO(captured[0])) as tar:
            return {member.name: member for member in tar.getmembers()}

    def test_archive_entries_are_stamped_with_the_pinned_time(self):
        with tempfile.TemporaryDirectory() as temporary:
            builder = self.builder(Path(temporary))
            builder.env["SOURCE_DATE_EPOCH"] = "1700000000"
            members = self.packed_members(builder)
            # Repacking one commit has to produce one archive, byte for byte.
            self.assertEqual({member.mtime for member in members.values()}, {1700000000})
            self.assertEqual(members["freya-nle"].uid, 0)
            # The executable bit is a POSIX fact; Windows cannot express it.
            if os.name != "nt":
                self.assertEqual(members["freya-nle"].mode & 0o777, 0o755)

    def test_the_commit_time_is_used_when_nothing_pins_one(self):
        with tempfile.TemporaryDirectory() as temporary:
            builder = self.builder(Path(temporary))
            builder.env.pop("SOURCE_DATE_EPOCH", None)
            builder.command_output = (
                lambda command: "1700000123\n" if command[1] == "log" else ""
            )
            members = self.packed_members(builder)
            self.assertEqual({member.mtime for member in members.values()}, {1700000123})

    def test_the_icon_conversion_keeps_host_paths_out_of_the_program(self):
        with tempfile.TemporaryDirectory() as temporary:
            source = Path(temporary)
            builder = self.builder(source)
            builder.container_engine = "podman"
            commands = []
            builder.run_cmd = lambda command, **_kwargs: commands.append(command) or 0
            with patch("BUILD.shutil.copy2"):
                builder.write_appimage_icon(source / "appdir")
            program = commands[0][2]
            # The container rewrite only maps arguments that are paths; a
            # program with one baked into its text came back as `/srcif ...`.
            self.assertNotIn(str(source), program)
            translated = builder.container_command(commands[0])
            self.assertEqual(translated[2], program)
            self.assertEqual(translated[4], "/src/assets/icon.svg")

    def test_the_image_tool_runs_where_the_build_runs(self):
        # `appimagetool` is a Linux AppImage: a macOS host cannot execute it, and
        # the build container is the only place it can run there.
        with tempfile.TemporaryDirectory() as temporary:
            source = Path(temporary)
            config = config_for(source, target="appimage")
            config.use_container = True
            builder = AppImageBuilder(
                config, Logger(lambda _message: None, lambda *_args: None)
            )
            builder.container_engine = "podman"
            builder.podman_container = "freya-nle-appimage-1"
            tool = source / ".build_tmp" / "appimagetool-x86_64.AppImage"
            builder.appimagetool_path = lambda: tool
            (builder.config.output_dir / "FreyaNLE.AppDir").mkdir(parents=True)
            calls = []
            builder.run_cmd = lambda command, **kwargs: calls.append((command, kwargs)) or 0
            builder.create_appimage("Freya-NLE-0.7.0-linux-x86_64")
            command, kwargs = calls[0]
            self.assertFalse(kwargs.get("force_host", False))
            translated = builder.container_command(command)
            self.assertEqual(translated[0], "/src/.build_tmp/appimagetool-x86_64.AppImage")
            # The architecture variable has to cross into the container too.
            self.assertEqual(builder.container_environment()["ARCH"], "x86_64")

    def test_a_container_build_compresses_with_the_container_zstd(self):
        # A macOS host has no zstd; the container the archive was built in does.
        with tempfile.TemporaryDirectory() as temporary:
            source = Path(temporary)
            builder = self.builder(source)
            builder.use_container = True
            builder.config.output_dir.mkdir(parents=True)
            (source / "dist").mkdir()
            calls = []
            builder.run_cmd = lambda command, **kwargs: calls.append((command, kwargs)) or 0
            with patch("BUILD.shutil.which", return_value=None):
                builder.create_tar_zst("Freya-NLE-0.7.0-linux-x86_64-archlinux")
            command, kwargs = calls[0]
            self.assertEqual(command[0], "zstd")
            self.assertFalse(kwargs.get("force_host", False))

    def test_requirements_are_read_where_the_tools_are_installed(self):
        # `objdump` and `readelf` exist in the container, not on a macOS host;
        # reading from the host would record no glibc floor and no sonames.
        with tempfile.TemporaryDirectory() as temporary:
            source = Path(temporary)
            config = config_for(source, target="arch")
            config.use_container = True
            builder = ArchBuilder(
                config, Logger(lambda _message: None, lambda *_args: None)
            )
            builder.container_engine = "podman"
            builder.podman_container = "archlinux-aviqtl-1"
            with patch("BUILD.subprocess.run") as run:
                run.return_value = subprocess.CompletedProcess(
                    [], 0, "GLIBC_2.35\n", ""
                )
                output = builder.inspect_output(
                    ["objdump", "-T", str(builder.cargo_binary())]
                )
            self.assertIn("GLIBC_2.35", output)
            command = run.call_args.args[0]
            self.assertEqual(command[:2], ["podman", "exec"])
            self.assertIn("/src/", " ".join(command))

    def test_an_offline_appimage_build_refuses_the_container(self):
        # Every container is created for one build, so `--offline` has nothing
        # to reuse inside it; the build has to say that instead of downloading.
        with tempfile.TemporaryDirectory() as temporary:
            config = config_for(Path(temporary), target="appimage", offline=True)
            config.use_container = True
            builder = AppImageBuilder(
                config, Logger(lambda _message: None, lambda *_args: None)
            )
            created = []
            builder.create_podman_container = lambda: created.append(True)
            with self.assertRaises(RuntimeError):
                builder.install_dependencies()
            self.assertEqual(created, [])

    def test_the_container_installs_what_the_ffmpeg_build_needs(self):
        # `build-ffmpeg.sh` exits when nasm is missing, and the CI host installs
        # it separately, so the container package list is the only source of it.
        with tempfile.TemporaryDirectory() as temporary:
            builder = self.builder(Path(temporary))
            builder.use_container = True
            builder.create_podman_container = lambda: None
            builder.install_rust = lambda: None
            builder.build_ffmpeg = lambda: None
            commands = []
            builder.run_cmd = lambda command, **_kwargs: commands.append(command) or 0
            builder.install_dependencies()
            apt = next(command for command in commands if "apt-get install" in command[-1])
            self.assertIn("nasm", apt[-1].split())


class TestReleasePipeline(unittest.TestCase):
    """The release has to publish a complete, named, verified set.

    These read the workflow files rather than a fixture because the checks they
    describe live there: a platform silently dropped from the matrix, or an
    artifact published under a name the scheme does not produce, is exactly the
    kind of regression a build script cannot catch.
    """

    def workflow(self, name):
        return (SOURCE_DIR / ".github" / "workflows" / name).read_text(encoding="utf-8")

    def test_a_tag_publishes_every_target(self):
        release = self.workflow("release.yml")
        package = self.workflow("package.yml")
        # One release asset per target the scheme defines.
        for target in (
            "-linux-x86_64.AppImage",
            "-linux-arm64.AppImage",
            "-linux-x86_64-archlinux.tar.zst",
            "-macos-arm64.zip",
            "-macos-x86_64.zip",
            "-windows-x86_64.zip",
        ):
            with self.subTest(target=target):
                self.assertIn(target, release)
        # And a job building each of them.
        for job in ("linux-appimage", "linux-arch", "macos", "windows"):
            with self.subTest(job=job):
                self.assertIn(f"{job}:", package)

    def steps(self, block):
        """The steps of a job block, in the order the job runs them."""
        return ["      - " + part for part in block.split("\n      - ")[1:]]

    def step(self, block, name):
        """The workflow text of one named step inside a job block."""
        for step in self.steps(block):
            if step.splitlines()[0].strip() == f"- name: {name}":
                return step
        raise AssertionError(f"no step named {name}")

    def test_the_ffmpeg_build_is_cached_by_the_pinned_script(self):
        # Building FFmpeg from source is the slowest step this workflow added, so
        # each job that needs it restores the install before running the script.
        key = "ffmpeg-${{ hashFiles('scripts/build-ffmpeg.sh') }}"
        build_step = {
            "linux-appimage": "Build the pinned FFmpeg",
            "macos": "Build the pinned FFmpeg for both architectures",
        }
        for job in ("linux-appimage", "macos"):
            block = self.job_block(job)
            restore = self.step(block, "Restore the pinned FFmpeg")
            save = self.step(block, "Save the pinned FFmpeg")
            order = [step.splitlines()[0].strip().removeprefix("- name: ") for step in self.steps(block)]
            with self.subTest(job=job):
                self.assertIn("actions/cache/restore@v4", restore)
                self.assertIn(key, restore)
                # The script reuses any prefix that has a pkg-config file, so a
                # fallback key would link an FFmpeg this commit did not pin.
                self.assertNotRegex(restore, r"(?m)^\s+restore-keys:")
                self.assertIn("actions/cache/save@v4", save)
                self.assertIn(key, save)
                # Directly after the build, so packaging cannot fail first.
                self.assertEqual(order[order.index(build_step[job]) + 1], "Save the pinned FFmpeg")
                # Skipped on a hit: saving an existing key fails the step.
                self.assertIn("cache-hit != 'true'", save)

    def test_the_old_workflow_is_gone(self):
        # Two workflows that both react to a tag would race to publish, and one
        # of them would publish the unverified set this change replaced.
        self.assertFalse((SOURCE_DIR / ".github" / "workflows" / "build.yml").exists())

    def job_block(self, name):
        """The workflow text for one job, so an assertion cannot match another.

        Jobs sit at two spaces of indentation, so the block ends at the next line
        that looks like a job header rather than at the next indented line.
        """
        lines = self.workflow("package.yml").splitlines()
        header = f"  {name}:"
        try:
            start = lines.index(header)
        except ValueError:
            raise AssertionError(f"no job named {name}") from None
        block = []
        for line in lines[start + 1:]:
            if re.match(r"^  [a-z][a-z0-9-]*:$", line):
                break
            block.append(line)
        return "\n".join(block)

    def test_the_appimage_is_built_against_the_oldest_ubuntu(self):
        # glibc is fixed when the ELF is linked, so the portable track has to be
        # built on the oldest Ubuntu the CI still hosts. Building it on the
        # default 24.04 runner would ask for glibc 2.39.
        job = self.job_block("linux-appimage")
        # Both architectures come from the same image version, so neither may
        # drift to a newer runner and raise its own glibc floor.
        self.assertEqual(
            re.findall(r"runner: (ubuntu-[0-9.]+(?:-arm)?)", job),
            ["ubuntu-22.04", "ubuntu-22.04-arm"],
        )
        # The job installs its own dependencies on the runner; a container would
        # only reintroduce a second place for the toolchain to disagree.
        self.assertNotIn("container:", job)

    def test_each_appimage_architecture_is_built_on_its_own_runner(self):
        # Crossing architectures would need emulation for the whole FFmpeg and
        # Slint build, and an emulated build is the one place a mistyped
        # architecture would go unnoticed: the payload would still be the host's.
        job = self.job_block("linux-appimage")
        for arch, runner in (("x86_64", "ubuntu-22.04"), ("arm64", "ubuntu-22.04-arm")):
            with self.subTest(arch=arch):
                self.assertRegex(job, rf"- arch: {arch}\n\s+runner: {runner}\n")
        # A build that failed on one architecture must not cancel the other; both
        # are release requirements, so both verdicts are wanted.
        self.assertIn("fail-fast: false", job)
        # The artifact a job uploads is per architecture, or `download-artifact`
        # would merge the two images under one name and keep whichever arrived last.
        self.assertIn("name: linux-appimage-${{ matrix.arch }}", job)
        self.assertIn("--target ${{ matrix.triple }}", job)

    def test_the_arm64_image_is_published_and_smoke_tested_on_arm64(self):
        # An x86_64 runner cannot start the arm64 image, and the mistake this
        # guards against -- an arm64 name around an x86_64 payload -- looks
        # perfectly fine from an x86_64 host.
        release = self.workflow("release.yml")
        self.assertIn("-linux-arm64.AppImage", release)
        publish = release[release.index("  publish:"):]
        needs = publish[publish.index("needs:"):publish.index("runs-on:")]
        self.assertIn("smoke-arm64", needs)
        arm = release[release.index("  smoke-arm64:"):release.index("  smoke-arch:")]
        self.assertIn("ubuntu-24.04-arm", arm)
        self.assertIn("linux-arm64.AppImage", arm)
        # `--version` proves the runtime starts; the unpacked tree proves which
        # architecture the payload is. The job must check out the built commit
        # for the licence comparison the script performs.
        self.assertIn("scripts/verify-release-contents.py", arm)
        self.assertIn("linux arm64", arm)
        self.assertIn("needs.resolve.outputs.source_ref", arm)
        # The reported version is a field, not a substring: a prefix match
        # would accept 0.7.0-rc.10 for a 0.7.0-rc.1 release.
        self.assertIn('!= "$VERSION"', arm)
        # `uname -m` and `build-info.json` are labels; the ELF machine type is
        # the payload's own statement, and a host that emulates both
        # architectures would run the wrong one without this.
        self.assertIn("readelf --file-header", arm)
        self.assertIn("Machine:.*AArch64", arm)

    def test_pacman_runs_without_sudo_in_the_podman_container(self):
        # The pinned Arch image has no sudo, and the podman fallback executes as
        # the container's root already.
        with tempfile.TemporaryDirectory() as temporary:
            builder = ArchBuilder(
                config_for(Path(temporary), target="arch"),
                Logger(lambda _m: None, lambda *_a: None),
            )
            builder.use_container = True
            builder.container_engine = "podman"
            builder.create_container = lambda: None
            commands = []
            builder.run_cmd = lambda command, **kwargs: commands.append(command) or 0
            builder.install_dependencies()
            self.assertTrue(commands)
            self.assertNotIn("sudo", commands[0])
            self.assertEqual(commands[0][0], "pacman")

    def test_pacman_uses_sudo_under_distrobox(self):
        with tempfile.TemporaryDirectory() as temporary:
            builder = ArchBuilder(
                config_for(Path(temporary), target="arch"),
                Logger(lambda _m: None, lambda *_a: None),
            )
            builder.use_container = True
            builder.container_engine = "distrobox"
            builder.create_container = lambda: None
            commands = []
            builder.run_cmd = lambda command, **kwargs: commands.append(command) or 0
            builder.install_dependencies()
            self.assertEqual(commands[0][0], "sudo")

    def test_the_arch_archive_is_built_in_its_pinned_image(self):
        job = self.job_block("linux-arch")
        self.assertIn(ARCH_LINUX_IMAGE, job)
        self.assertNotIn("archlinux:latest", job)

    def test_a_reusable_call_defaults_to_building_every_platform(self):
        # A `workflow_call` leaves an undeclared input empty, which would make
        # every `inputs.platform == ...` false and a release publish nothing.
        package = self.workflow("package.yml")
        start = package.index("  workflow_call:")
        call = package[start:package.index("  workflow_dispatch:")]
        self.assertIn("platform:", call)
        self.assertIn("default: all", call)

    def test_the_arch_smoke_test_gates_publishing(self):
        # It is the only job that runs the Arch archive on Arch, so publishing
        # must wait for it like it waits for the other smoke test.
        release = self.workflow("release.yml")
        publish = release[release.index("  publish:"):]
        needs = publish[publish.index("needs:"):publish.index("runs-on:")]
        self.assertIn("smoke-arch", needs)

    def test_the_windows_check_waits_for_the_process(self):
        # A GUI-subsystem executable returns immediately from `&`, so the exit
        # code would describe the previous command.
        package = self.workflow("package.yml")
        self.assertIn("Start-Process", package)
        self.assertIn("-PassThru", package)

    def test_checksums_are_published(self):
        self.assertIn("SHA256SUMS", self.workflow("release.yml"))

    def test_macos_checks_every_mach_o_against_the_plist(self):
        # The defect this guards: a bundle whose plist promised macOS 12 while
        # its libraries needed 15.
        package = self.workflow("package.yml")
        self.assertIn("vtool -show-build", package)
        self.assertIn("LSMinimumSystemVersion", package)

    def test_windows_archive_is_checked_for_the_runtime(self):
        package = self.workflow("package.yml")
        self.assertIn("vcruntime140.dll", package)

    def test_the_baseline_workflow_follows_the_naming_scheme(self):
        # It asserted one fixed `Freya-NLE-MSVC-x86_64.zip` for a while after the
        # names changed, so a green packaging run still left the PR red.
        baseline = self.workflow("msvc.yml")
        self.assertIn("-windows-x86_64.zip", baseline)
        self.assertNotIn("Freya-NLE-MSVC", baseline)
        self.assertIn("vcruntime140.dll", baseline)


class TestCrossBuildTargets(unittest.TestCase):
    """One runner produces both Apple architectures."""

    def test_the_target_triple_separates_intermediate_trees(self):
        with tempfile.TemporaryDirectory() as temporary:
            source = Path(temporary)
            native = PlatformBuilder(
                config_for(source, target="xcode"), Logger(lambda _m: None, lambda *_a: None)
            )
            cross = PlatformBuilder(
                config_for(source, target="xcode"), Logger(lambda _m: None, lambda *_a: None)
            )
            cross.config.target_triple = "x86_64-apple-darwin"
            # Sharing a directory would let the second build reuse the first's
            # objects and report success without producing a new binary.
            self.assertNotEqual(native.config.work_dir, cross.config.work_dir)

    def test_a_cross_build_asks_cargo_for_that_target(self):
        with tempfile.TemporaryDirectory() as temporary:
            builder = PlatformBuilder(
                config_for(Path(temporary)), Logger(lambda _m: None, lambda *_a: None)
            )
            builder.config.target_triple = "x86_64-apple-darwin"
            command = builder.get_cargo_build_cmd()
            self.assertEqual(command[command.index("--target") + 1], "x86_64-apple-darwin")
            # The binary lands in the per-target directory cargo uses.
            self.assertIn("x86_64-apple-darwin", str(builder.cargo_binary()))

    def test_macos_names_a_cross_build_after_its_target(self):
        with tempfile.TemporaryDirectory() as temporary:
            builder = XcodeBuilder(
                config_for(Path(temporary), target="xcode"),
                Logger(lambda _m: None, lambda *_a: None),
            )
            builder.config.target_triple = "x86_64-apple-darwin"
            self.assertEqual(builder.platform_architecture(), "x86_64")
            self.assertTrue(builder.get_archive_name().endswith("-macos-x86_64"))

    def test_a_native_build_has_no_target_argument(self):
        with tempfile.TemporaryDirectory() as temporary:
            builder = PlatformBuilder(
                config_for(Path(temporary)), Logger(lambda _m: None, lambda *_a: None)
            )
            self.assertNotIn("--target", builder.get_cargo_build_cmd())


class TestLinuxArm64Recipe(unittest.TestCase):
    """The FFmpeg recipe has to know the arm64 Linux target.

    `build-ffmpeg.sh` rejects an unknown `--target` outright, so an arm64 image
    whose triple the script does not list fails before anything compiles. The x264
    `--host` matters just as much: it decides which assembly kernels are built,
    and the x86_64 one silently produces an archive that cannot link on arm64.
    """

    def setUp(self):
        self.script = (SOURCE_DIR / "scripts" / "build-ffmpeg.sh").read_text(encoding="utf-8")

    def test_the_arm64_linux_triple_is_a_supported_case(self):
        self.assertIn("aarch64-unknown-linux-gnu)", self.script)
        # armv8-a, not `native`: the image has to run on any ARMv8.0 system, and
        # `native` would bake in the build machine's extensions.
        self.assertIn("--arch=aarch64 --cpu=armv8-a", self.script)

    def test_it_documents_the_arm64_triple(self):
        # The usage block is what a maintainer reads before running the script.
        self.assertIn("aarch64-unknown-linux-gnu", self.script.split("set -euo")[0])

    def test_x264_is_built_for_the_target_it_is_asked_for(self):
        # A hard-coded `--host=x86_64-linux-gnu` selects the x86 assembly kernels
        # for every non-Apple target, arm64 included.
        self.assertNotIn("--host=x86_64-linux-gnu", self.script)
        self.assertIn('"--host=$TARGET"', self.script)


@unittest.skipIf(os.name == "nt", "the guard lives in a bash script MSVC builds do not run")
class TestLinuxArm64RecipeGuard(unittest.TestCase):
    """The Linux native-only guard, exercised against a controlled host.

    The real script runs with a stand-in `uname` and a cached prefix, so the
    cases need neither a compiler nor a network.
    """

    def run_recipe(self, *, host_system, host_machine, target, cached):
        """Run the real script against a controlled host and cached prefix."""
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            scripts = root / "scripts"
            scripts.mkdir()
            shutil.copy2(SOURCE_DIR / "scripts" / "build-ffmpeg.sh", scripts)
            # A stand-in `uname` keeps the host out of the test: the guard has
            # to answer from these two values alone.
            bin_dir = root / "bin"
            bin_dir.mkdir()
            uname = bin_dir / "uname"
            uname.write_text(
                "#!/bin/sh\n"
                'case "$1" in\n'
                '  -s) echo "$FAKE_UNAME_S" ;;\n'
                '  -m) echo "$FAKE_UNAME_M" ;;\n'
                "  *) exit 1 ;;\n"
                "esac\n",
                encoding="utf-8",
            )
            uname.chmod(0o755)
            prefix = root / "prefix"
            if cached:
                pkgconfig = prefix / "lib" / "pkgconfig"
                pkgconfig.mkdir(parents=True)
                (pkgconfig / "libavcodec.pc").write_text("", encoding="utf-8")
            env = {
                "PATH": f"{bin_dir}{os.pathsep}{os.environ.get('PATH', '')}",
                "HOME": os.environ.get("HOME", str(root)),
                "JOBS": "1",
                "WORK_DIR": str(root / "work"),
                "FAKE_UNAME_S": host_system,
                "FAKE_UNAME_M": host_machine,
            }
            return subprocess.run(
                [
                    "bash",
                    str(scripts / "build-ffmpeg.sh"),
                    "--target", target,
                    "--prefix", str(prefix),
                ],
                capture_output=True,
                text=True,
                env=env,
                check=False,
            )

    def test_a_mismatched_linux_target_fails_before_the_cache_is_reused(self):
        # There is no cross toolchain here, so the wrong host would compile the
        # host architecture into the target's prefix and only fail much later,
        # at the link — or not at all, if a cached prefix then satisfied the
        # reuse check. The refusal still has to come first.
        completed = self.run_recipe(
            host_system="Linux",
            host_machine="x86_64",
            target="aarch64-unknown-linux-gnu",
            cached=True,
        )
        self.assertNotEqual(completed.returncode, 0)
        self.assertIn("Linux builds are native-only", completed.stderr)
        self.assertNotIn("Reusing the FFmpeg install", completed.stdout)

    def test_a_matching_linux_target_reaches_the_cached_install(self):
        completed = self.run_recipe(
            host_system="Linux",
            host_machine="aarch64",
            target="aarch64-unknown-linux-gnu",
            cached=True,
        )
        self.assertEqual(completed.returncode, 0, completed.stderr)
        self.assertIn("Reusing the FFmpeg install", completed.stdout)

    def test_a_darwin_target_is_not_held_to_the_host_architecture(self):
        # clang there crosses Apple architectures by design, so the Linux
        # refusal may not reach the Darwin triples.
        completed = self.run_recipe(
            host_system="Linux",
            host_machine="x86_64",
            target="aarch64-apple-darwin",
            cached=True,
        )
        self.assertEqual(completed.returncode, 0, completed.stderr)
        self.assertIn("Reusing the FFmpeg install", completed.stdout)


class TestVendoredCrates(unittest.TestCase):
    """Two crates assume `c_char` is signed, which AArch64 Linux contradicts.

    Every other dependency compiled for arm64; these two were the failures, and
    crates.io has no newer release of either. The patches are asserted here
    because removing one would only surface as a release failure on an
    architecture a normal test run never uses.
    """

    CRATES = ("truce-rack-clap", "truce-rack-vst3")

    def test_the_patched_crates_are_selected_from_the_workspace(self):
        manifest = (SOURCE_DIR / "rust" / "Cargo.toml").read_text(encoding="utf-8")
        for crate in self.CRATES:
            with self.subTest(crate=crate):
                self.assertIn(f'{crate} = {{ path = "../vendor/{crate}" }}', manifest)
                self.assertTrue(
                    (SOURCE_DIR / "vendor" / crate / "Cargo.toml").is_file()
                )
                # A path patch with no `source` in the lock is what makes
                # `--locked` use it instead of the registry copy.
                lock = (SOURCE_DIR / "rust" / "Cargo.lock").read_text(encoding="utf-8")
                entry = lock[lock.index(f'name = "{crate}"'):]
                entry = entry.split("[[package]]", 1)[0]
                self.assertNotIn('source = "registry+', entry)

    def test_the_patches_use_c_char_where_the_releases_used_i8(self):
        clap = (SOURCE_DIR / "vendor" / "truce-rack-clap" / "src" / "lib.rs").read_text(
            encoding="utf-8"
        )
        vst3 = (SOURCE_DIR / "vendor" / "truce-rack-vst3" / "src" / "lib.rs").read_text(
            encoding="utf-8"
        )
        self.assertIn("0 as c_char", clap)
        self.assertNotIn("0i8;", clap)
        self.assertIn("fn char8_array_to_string(array: &[c_char])", vst3)
        self.assertIn("fn tuid_to_hex(cid: &[c_char; 16])", vst3)
        self.assertIn("*const c_char", vst3)
        # The ID parser may not slice the string: a project file can carry a
        # multi-byte character across one of the pair boundaries, and a string
        # index would panic where the parse has to fail.
        self.assertIn("fn hex_to_tuid", vst3)
        self.assertNotIn("&hex[i * 2..i * 2 + 2]", vst3)

    def test_the_vendored_crates_carry_their_licence_texts(self):
        # The crates are MIT OR Apache-2.0 and the sources live in this tree,
        # so the notices belong beside them — and `copy_licenses` ships
        # everything under `licenses/`, which is how the artifacts get them.
        for crate in self.CRATES:
            for notice in ("LICENSE-MIT", "LICENSE-APACHE"):
                with self.subTest(crate=crate, notice=notice):
                    source = SOURCE_DIR / "vendor" / crate / notice
                    self.assertTrue(source.is_file(), f"{source} is missing")
                    shipped = SOURCE_DIR / "licenses" / {
                        "LICENSE-MIT": "MIT.txt", "LICENSE-APACHE": "Apache-2.0.txt"
                    }[notice]
                    self.assertEqual(
                        shipped.read_bytes(), source.read_bytes(),
                        f"{shipped} differs from {source}",
                    )


class TestReleaseVersionBaseline(unittest.TestCase):
    """The checked-in version, tag form, and changelog must agree.

    These read the real tree instead of a fixture: a release tag is rejected by
    the workflow only when it disagrees with ``rust/Cargo.toml``, so a missing
    changelog section would otherwise ship.
    """

    def setUp(self):
        self.version = read_project_version(SOURCE_DIR)

    def test_workspace_version_is_semantic(self):
        # `parse_semver` raises for anything that is not SemVer, and the shape check
        # pins the three-part core the release tools and the bundle metadata assume.
        parse_semver(self.version)
        self.assertRegex(
            self.version,
            r"^[0-9]+\.[0-9]+\.[0-9]+(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?$",
        )

    def test_prerelease_uses_the_dotted_counter_form(self):
        # SemVer compares prerelease identifiers one by one, so `rc.10` is newer
        # than `rc.9`. `rc10` would sort before `rc9`, and `rc-1` parses as a
        # single identifier, so neither is accepted.
        if "-" not in self.version:
            return
        prerelease = self.version.split("-", 1)[1].split("+", 1)[0]
        self.assertRegex(prerelease, r"^rc\.[1-9][0-9]*$")

    def test_changelog_documents_the_workspace_version(self):
        changelog = (SOURCE_DIR / "CHANGELOG.md").read_text(encoding="utf-8")
        # A dated heading of its own, so a prerelease section cannot satisfy the
        # check for its numeric core and body text cannot satisfy it either.
        heading = re.compile(
            rf"^## \[{re.escape(self.version)}\] - [0-9]{{4}}-[0-9]{{2}}-[0-9]{{2}}$",
            flags=re.MULTILINE,
        )
        self.assertRegex(changelog, heading)


if __name__ == "__main__":
    unittest.main()
