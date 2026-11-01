"""Tests for the harness's version-manifest reader."""

from pathlib import Path

import pytest

from praxion_harness.manifest import ManifestError, load

FIXTURES = Path(__file__).resolve().parent / "fixtures"


def test_loads_the_repository_manifest():
    manifest = load()
    assert manifest.minecraft.protocol == 777
    assert manifest.minecraft.version == "26.3"


def test_rejects_a_missing_key():
    with pytest.raises(ManifestError, match="protocol"):
        load(FIXTURES / "missing-key.toml")


def test_rejects_a_wrong_type():
    with pytest.raises(ManifestError, match="protocol"):
        load(FIXTURES / "wrong-type.toml")
