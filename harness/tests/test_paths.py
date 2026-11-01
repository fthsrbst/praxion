import os
from pathlib import Path

import pytest

from praxion_harness.paths import PathResolutionError, resolve_java, resolve_server_jar, repo_root


def test_repo_root_contains_manifest():
    assert (repo_root() / "versions" / "manifest.toml").is_file()


def test_resolve_from_env(monkeypatch, tmp_path):
    j = tmp_path / "java.exe"
    jar = tmp_path / "server.jar"
    j.write_text("x")
    jar.write_text("y")
    monkeypatch.setenv("PRAXION_JAVA", str(j))
    monkeypatch.setenv("PRAXION_SERVER_JAR", str(jar))
    assert resolve_java() == j.resolve() or resolve_java() == j
    assert resolve_server_jar() == jar.resolve() or resolve_server_jar() == jar


def test_resolve_missing_raises(monkeypatch):
    monkeypatch.delenv("PRAXION_JAVA", raising=False)
    monkeypatch.delenv("PRAXION_SERVER_JAR", raising=False)
    # Ensure no local/paths.toml provides them.
    # We'll simulate by monkeypatching _read_paths_toml.
    import praxion_harness.paths as p
    monkeypatch.setattr(p, "_read_paths_toml", lambda: {})
    with pytest.raises(PathResolutionError):
        resolve_java()
    with pytest.raises(PathResolutionError):
        resolve_server_jar()


def test_resolve_invalid_path_raises(monkeypatch):
    monkeypatch.setenv("PRAXION_JAVA", "C:/nope/java.exe")
    monkeypatch.delenv("PRAXION_SERVER_JAR", raising=False)
    import praxion_harness.paths as p
    monkeypatch.setattr(p, "_read_paths_toml", lambda: {})
    with pytest.raises(PathResolutionError):
        resolve_java()
