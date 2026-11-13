"""Fetch the fixed page list into ``local/wiki/raw/``, slowly and politely.

Rules (decided 2026-11-12, see ``docs/decisions.md``):

- plain article URLs only: ``https://minecraft.wiki/w/<Title>`` (no ``api.php``,
  no ``?action=``, no ``rest_v1``),
- ``robots.txt`` is parsed and checked before every fetch,
- at most one request every :data:`DELAY_SECONDS` seconds,
- an identifying user agent (:data:`USER_AGENT`),
- stop on HTTP 429 or 403 and report it.

The list is resumable: an existing ``raw/<slug>.html`` is skipped.
"""

from __future__ import annotations

import json
import re
import time
import urllib.error
import urllib.parse
import urllib.request
from dataclasses import dataclass, field
from datetime import datetime, timezone
from pathlib import Path
from typing import Callable, Iterable
from urllib.robotparser import RobotFileParser

from . import paths

SITE = "https://minecraft.wiki"
USER_AGENT = (
    "PraxionWikiIndexer/0.1 (local, non-commercial research index; not redistributed)"
)
DELAY_SECONDS = 1.5
TIMEOUT_SECONDS = 60
#: Statuses that mean "stop now", not "this page failed".
BLOCKING_STATUSES = (403, 429)


class RobotsDenied(RuntimeError):
    """``robots.txt`` does not allow this request; nothing was sent."""


class RobotsUnavailable(RuntimeError):
    """``robots.txt`` could not be read; nothing is fetched without it."""


def page_url(title: str) -> str:
    """The plain article URL for a wiki page title."""
    quoted = urllib.parse.quote(title.strip().replace(" ", "_"), safe="/:()',!&+")
    return f"{SITE}/w/{quoted}"


def robots_url() -> str:
    return f"{SITE}/robots.txt"


def slug(title: str) -> str:
    """A filesystem-safe name for a page title (``Creeper``, ``Tutorial_Mining``).

    ``:`` and ``/`` become ``_`` so the name is valid on Windows too. The page
    URL is kept separately, so the article link stays exact.
    """
    cleaned = title.strip().replace(" ", "_")
    cleaned = re.sub(r"[^A-Za-z0-9_.()'-]+", "_", cleaned)
    cleaned = re.sub(r"_+", "_", cleaned).strip("_")
    return cleaned or "page"


def read_pages(path: Path | None = None) -> list[str]:
    """The article titles: comments, blank lines and duplicates removed."""
    source = Path(path) if path is not None else paths.pages_path()
    titles: list[str] = []
    seen: set[str] = set()
    for raw in source.read_text(encoding="utf-8").splitlines():
        line = raw.strip()
        if not line or line.startswith("#"):
            continue
        key = " ".join(line.split()).lower()
        if key in seen:
            continue
        seen.add(key)
        titles.append(" ".join(line.split()))
    return titles


def parse_robots(text: str) -> RobotFileParser:
    """A ``RobotFileParser`` over already-downloaded ``robots.txt`` text."""
    parser = RobotFileParser()
    parser.set_url(robots_url())
    parser.parse(text.splitlines())
    return parser


@dataclass
class FetchResult:
    """One page's outcome; ``status`` is 0 when a local file was reused."""

    title: str
    url: str
    status: int
    final_url: str = ""
    path: str = ""
    note: str = ""

    @property
    def ok(self) -> bool:
        return self.status == 200 or self.note == "cached"

    def as_dict(self) -> dict[str, object]:
        return {
            "title": self.title,
            "url": self.url,
            "status": self.status,
            "final_url": self.final_url,
            "path": self.path,
            "note": self.note,
        }


@dataclass
class FetchReport:
    fetched: list[FetchResult] = field(default_factory=list)
    reused: list[FetchResult] = field(default_factory=list)
    missing: list[FetchResult] = field(default_factory=list)
    failed: list[FetchResult] = field(default_factory=list)
    stopped: str = ""

    @property
    def dropped(self) -> list[FetchResult]:
        return [*self.missing, *self.failed]

    def as_dict(self) -> dict[str, object]:
        return {
            "at": datetime.now(timezone.utc).isoformat(timespec="seconds"),
            "site": SITE,
            "user_agent": USER_AGENT,
            "fetched": [result.as_dict() for result in self.fetched],
            "reused": [result.as_dict() for result in self.reused],
            "missing": [result.as_dict() for result in self.missing],
            "failed": [result.as_dict() for result in self.failed],
            "stopped": self.stopped,
            "dropped_titles": [result.title for result in self.dropped],
        }


class Fetcher:
    """Polite, resumable downloader for one fixed list of article pages.

    ``opener``, ``sleep`` and ``clock`` are injectable so the tests never
    touch the network. ``robots_text`` skips the ``robots.txt`` download.
    """

    def __init__(
        self,
        raw: Path,
        *,
        delay: float = DELAY_SECONDS,
        timeout: float = TIMEOUT_SECONDS,
        opener: Callable[[urllib.request.Request, float], object] | None = None,
        sleep: Callable[[float], None] = time.sleep,
        clock: Callable[[], float] = time.monotonic,
        robots_text: str | None = None,
    ) -> None:
        self.raw = Path(raw)
        self.delay = delay
        self.timeout = timeout
        self._opener = opener or _urllib_open
        self._sleep = sleep
        self._clock = clock
        self._robots_text = robots_text
        self._robots: RobotFileParser | None = None
        self._robots_error: str = ""
        self._last_request: float | None = None

    # -- robots ---------------------------------------------------------- #
    def robots(self) -> RobotFileParser:
        if self._robots is not None:
            return self._robots
        if self._robots_error:
            raise RobotsUnavailable(self._robots_error)
        text = self._robots_text
        if text is None:
            try:
                request = urllib.request.Request(
                    robots_url(), headers={"User-Agent": USER_AGENT}
                )
                response = self._request(request)
                text = response.read().decode("utf-8", "replace")
            except Exception as error:
                self._robots_error = f"cannot read {robots_url()}: {error}"
                raise RobotsUnavailable(self._robots_error) from error
        self._robots = parse_robots(text)
        return self._robots

    def allowed(self, url: str) -> bool:
        return self.robots().can_fetch(USER_AGENT, url)

    # -- requests -------------------------------------------------------- #
    def _wait(self) -> None:
        if self._last_request is not None:
            remaining = self.delay - (self._clock() - self._last_request)
            if remaining > 0:
                self._sleep(remaining)
        self._last_request = self._clock()

    def _request(self, request: urllib.request.Request):
        self._wait()
        return self._opener(request, self.timeout)

    def fetch(self, title: str, *, force: bool = False) -> FetchResult:
        """Download one page unless ``raw/<slug>.html`` already exists."""
        url = page_url(title)
        target = self.raw / f"{slug(title)}.html"
        if target.exists() and not force:
            return FetchResult(title, url, 0, url, str(target), "cached")
        if not self.allowed(url):
            raise RobotsDenied(f"robots.txt disallows {url}")
        request = urllib.request.Request(url, headers={"User-Agent": USER_AGENT})
        try:
            response = self._request(request)
        except urllib.error.HTTPError as error:
            status = int(error.code)
            note = "missing" if status == 404 else f"HTTP {status}"
            return FetchResult(title, url, status, url, "", note)
        except urllib.error.URLError as error:
            return FetchResult(title, url, 0, url, "", f"network: {error.reason}")
        with response:
            status = int(getattr(response, "status", 200))
            body = response.read()
            final_url = str(getattr(response, "url", url) or url)
        if status != 200:
            note = "missing" if status == 404 else f"HTTP {status}"
            return FetchResult(title, url, status, final_url, "", note)
        self.raw.mkdir(parents=True, exist_ok=True)
        try:
            target.write_bytes(body)
        except OSError as error:
            return FetchResult(title, url, status, final_url, "", f"cannot save: {error}")
        return FetchResult(title, url, 200, final_url, str(target))

    def fetch_all(self, titles: Iterable[str], *, force: bool = False) -> FetchReport:
        """Fetch every title; skip cached files, stop on 403/429."""
        report = FetchReport()
        for title in titles:
            try:
                result = self.fetch(title, force=force)
            except RobotsUnavailable as error:
                report.stopped = str(error)
                break
            except RobotsDenied as error:
                report.failed.append(FetchResult(title, page_url(title), 0, "", "", str(error)))
                continue
            if result.note == "cached":
                report.reused.append(result)
            elif result.status == 200:
                report.fetched.append(result)
            elif result.status == 404:
                report.missing.append(result)
            else:
                report.failed.append(result)
            if result.status in BLOCKING_STATUSES:
                report.stopped = (
                    f"HTTP {result.status} for {result.url}; stopped after "
                    f"{len(report.fetched) + len(report.reused)} pages"
                )
                break
        return report


def _urllib_open(request: urllib.request.Request, timeout: float):
    return urllib.request.urlopen(request, timeout=timeout)


def write_report(report: FetchReport, path: Path | None = None) -> Path:
    target = Path(path) if path is not None else paths.report_path()
    target.parent.mkdir(parents=True, exist_ok=True)
    target.write_text(
        json.dumps(report.as_dict(), indent=2, ensure_ascii=False) + "\n", encoding="utf-8"
    )
    return target


__all__ = [
    "DELAY_SECONDS",
    "Fetcher",
    "FetchReport",
    "FetchResult",
    "RobotsDenied",
    "RobotsUnavailable",
    "SITE",
    "USER_AGENT",
    "page_url",
    "parse_robots",
    "read_pages",
    "robots_url",
    "slug",
    "write_report",
]
