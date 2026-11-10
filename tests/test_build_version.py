import argparse
import json
import os
import re
import subprocess
import sys
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

    def test_licences_are_staged_when_present(self):
        # The application is AGPLv3 and bundles GPL FFmpeg, so an archive with no
        # licence text is a compliance gap rather than a cosmetic omission.
        with tempfile.TemporaryDirectory() as temporary:
            source = Path(temporary)
            (source / "LICENSE.md").write_text("AGPL", encoding="utf-8")
            destination = source / "out"
            builder = PlatformBuilder(
                config_for(source), Logger(lambda _message: None, lambda *_args: None)
            )
            builder.copy_licenses(destination)
            self.assertEqual(
                (destination / "licenses" / "LICENSE.md").read_text(encoding="utf-8"), "AGPL"
            )

    def test_missing_licences_do_not_fail_a_fixture_tree(self):
        with tempfile.TemporaryDirectory() as temporary:
            source = Path(temporary)
            builder = PlatformBuilder(
                config_for(source), Logger(lambda _message: None, lambda *_args: None)
            )
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

    def test_appdir_staging_puts_resources_next_to_the_executable(self):
        with tempfile.TemporaryDirectory() as temporary:
            source = Path(temporary)
            output = source / "build"
            output.mkdir()
            (output / "freya-nle").write_text("binary", encoding="utf-8")
            (output / "effects").mkdir()
            (output / "effects" / "blur.json").write_text("{}", encoding="utf-8")
            builder = self.builder(source)
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

    def test_the_tool_is_pinned_by_checksum(self):
        # A downloaded build tool is a supply chain entry point, so the digest is
        # part of the source and a mismatch has to stop the build.
        with tempfile.TemporaryDirectory() as temporary:
            builder = self.builder(Path(temporary))
            self.assertRegex(builder.IMAGE_TOOL_SHA256, r"^[0-9a-f]{64}$")
            self.assertTrue(builder.IMAGE_TOOL_URL.startswith("https://"))

    def test_a_corrupt_tool_is_rejected(self):
        with tempfile.TemporaryDirectory() as temporary:
            source = Path(temporary)
            builder = self.builder(source)
            tool = source / ".build_tmp" / builder.IMAGE_TOOL
            tool.parent.mkdir(parents=True)
            tool.write_text("not an appimage", encoding="utf-8")
            builder.IMAGE_TOOL_SHA256 = "0" * 64
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

    def test_a_missing_msvcp_is_tolerated(self):
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
            builder.copy_windows_runtime()
            self.assertTrue((output / "vcruntime140.dll").is_file())
            self.assertFalse((output / "msvcp140.dll").exists())


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
            self.assertIn("podman exec", first)
            self.assertIn("freya-nle-test", first)
            self.assertIn("podman exec", second)
            self.assertNotIn("--rm", first)

    def test_a_failed_command_can_be_tolerated(self):
        with tempfile.TemporaryDirectory() as temporary:
            builder = self.builder(Path(temporary))
            builder.container_engine = "podman"
            code = builder.run_cmd([sys.executable, "-c", "raise SystemExit(3)"],
                                   allow_failure=True)
            self.assertEqual(code, 3)
            with self.assertRaises(subprocess.CalledProcessError):
                builder.run_cmd([sys.executable, "-c", "raise SystemExit(3)"])


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

    def test_the_old_workflow_is_gone(self):
        # Two workflows that both react to a tag would race to publish, and one
        # of them would publish the unverified set this change replaced.
        self.assertFalse((SOURCE_DIR / ".github" / "workflows" / "build.yml").exists())

    def test_the_appimage_is_built_against_its_pinned_image(self):
        package = self.workflow("package.yml")
        # Building the portable track on the runner's own Ubuntu would link that
        # Ubuntu's glibc and defeat the reason the track exists.
        self.assertIn(APPIMAGE_BASE_IMAGE, package)

    def test_the_arch_image_is_pinned_to_a_digest(self):
        package = self.workflow("package.yml")
        self.assertIn(ARCH_LINUX_IMAGE, package)
        # A floating tag is what let the glibc requirement drift between builds.
        self.assertNotIn("archlinux:latest", package)

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
