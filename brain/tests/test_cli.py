"""The CLI's startup path: a bad recipe export is one readable line, exit 2.

``needs.load`` raises :class:`~praxion_brain.needs.StaleGamedata` when the
export was made from another jar than the one on this machine (design §7:
"a repinned jar with a stale export"). The operator must see the reason and
the remedy, not a traceback, and the process must not start.
"""

from __future__ import annotations

from praxion_brain import cli, needs


def test_a_stale_export_is_a_readable_startup_error(monkeypatch, capsys) -> None:
    def stale():
        raise needs.StaleGamedata(
            "local/gamedata/26.3.json was exported from jar 0123456789ab, but "
            "third_party/minecraft/server-26.3.jar is fedcba987654: "
            "run `praxion-body export-gamedata`"
        )

    # Nothing before the load touches the machine: the loop is never built.
    monkeypatch.setattr(cli, "Ledger", lambda *a, **k: object())
    monkeypatch.setattr(cli, "Evidence", lambda *a, **k: object())
    monkeypatch.setattr(cli, "Memory", lambda *a, **k: object())
    monkeypatch.setattr(cli, "load_catalog", lambda: object())
    monkeypatch.setattr(cli.needs, "load", stale)

    assert cli.main(["run", "--port", "1", "--console-port", "0"]) == 2
    captured = capsys.readouterr()
    assert "export-gamedata" in captured.err
    assert "Traceback" not in captured.err
