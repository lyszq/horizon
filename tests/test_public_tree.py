from __future__ import annotations

import unittest

from tools import check_public_tree


class PublicTreeTests(unittest.TestCase):
    def test_rejects_private_roots_even_if_force_added(self) -> None:
        paths = (
            ".local/minecraft/26.3/client.jar",
            ".tools/cache/token.json",
            "saves/world/level.dat",
            "backups/world.zip",
            "screenshots/world.png",
            "crash-reports/crash-2026-10-25.txt",
        )
        for path in paths:
            with self.subTest(path=path):
                self.assertTrue(check_public_tree.prohibited(path))

    def test_private_roots_are_case_insensitive(self) -> None:
        paths = (
            ".LOCAL/minecraft/26.3/client.jar",
            ".TOOLS/cache/token.json",
            "SAVES/world/level.dat",
            "Backups/world.zip",
            "SCREENSHOTS/world.png",
            "Crash-Reports/crash.txt",
        )
        for path in paths:
            with self.subTest(path=path):
                self.assertTrue(check_public_tree.prohibited(path))

    def test_rejects_root_runtime_settings_and_session_files_case_insensitively(self) -> None:
        paths = (
            "options.txt",
            "HORIZON.TXT",
            "horizon-graphics.txt",
            "HORIZON-CONTROLLER.TXT",
            "horizon-extras.txt",
            "horizon-player.txt",
            "SERVERS.DAT",
            ".local-check.log",
        )
        for path in paths:
            with self.subTest(path=path):
                self.assertTrue(check_public_tree.prohibited(path))

    def test_rejects_world_region_files_with_case_insensitive_extensions(self) -> None:
        for path in ("world/region/r.0.0.mca", "world/region/r.0.0.MCR"):
            with self.subTest(path=path):
                self.assertTrue(check_public_tree.prohibited(path))

    def test_preserves_horizon_code_fonts_icons_mod_assets_and_java_allowlists(self) -> None:
        paths = (
            "crates/horizon-client/src/game.rs",
            "crates/horizon-theme/fonts/Geist-Regular.ttf",
            "crates/horizon-theme/icons/horizon-64.png",
            "crates/horizon-mod-sdk/examples/ruby-content/assets/ruby/textures/block/ruby_block.png",
            "tools/JfrSummary.java",
            "tests/java/JfrFixture.java",
            "mods/horizon/src/test/java/dev/horizonlab/parity/ExampleOracle.java",
        )
        for path in paths:
            with self.subTest(path=path):
                self.assertFalse(check_public_tree.prohibited(path))

    def test_rejects_non_horizon_java_sources(self) -> None:
        paths = (
            "mods/horizon/src/net/minecraft/client/Minecraft.java",
            "mods/horizon-lab/src/org/example/Tool.java",
        )
        for path in paths:
            with self.subTest(path=path):
                self.assertTrue(check_public_tree.prohibited(path))


if __name__ == "__main__":
    unittest.main()
