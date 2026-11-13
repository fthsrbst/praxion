"""Extract the article body text from saved wiki HTML with ``html.parser``.

The output is ``local/wiki/text/<slug>.json``::

    {"title": "Iron Ore", "url": "https://minecraft.wiki/w/Iron_Ore",
     "sections": [{"heading": "Introduction", "text": "..."}, ...]}

Dropped on purpose: navigation and page furniture, infobox and message-box raw
markup, image figures, reference markers and the reference list, edit links,
maintenance tables (sounds, data values, achievements, videos) and the
``History`` section. Section headings are kept; a level-3 heading becomes
``"Parent > Child"`` so a passage keeps its context.
"""

from __future__ import annotations

import json
import re
from dataclasses import dataclass, field
from html.parser import HTMLParser
from pathlib import Path

from . import paths
from .fetch import slug

#: Section names whose whole subtree carries no survival knowledge.
DROP_SECTIONS = {
    "history",
    "gallery",
    "sounds",
    "data values",
    "achievements",
    "advancements",
    "videos",
    "references",
    "external links",
    "navigation",
    "issues",
    "see also",
}

#: Elements with no readable article text.
SKIP_TAGS = {
    "script",
    "style",
    "noscript",
    "audio",
    "video",
    "img",
    "svg",
    "math",
    "figure",
    "template",
}

#: Class names (exact match) whose subtree is page furniture, not article text.
SKIP_CLASSES = {
    "infobox",
    "navbox",
    "navbox-styles",
    "vertical-navbox",
    "mw-editsection",
    "mw-editsection-like",
    "load-page-content",
    "chestcontents-json",
    "navigation-not-searchable",
    "toc",
    "references",
    "mw-references-wrap",
    "droptable-references",
    "msgbox",
    "hatnote",
    "thumb",
    "gallery",
    "metadata",
    "mw-empty-elt",
    "calculator-field",
    "calculator-container",
    "breakingrow-container",
    "id-table",
    "mcw-calc",
    "noprint",
    "mw-indicators",
}

#: Tables whose ``data-description`` marks them as data noise.
SKIP_TABLE_DESCRIPTIONS = {"Sound", "History"}

#: Elements that end the current paragraph when they close.
BLOCK_TAGS = {
    "p",
    "div",
    "li",
    "dt",
    "dd",
    "td",
    "th",
    "tr",
    "table",
    "blockquote",
    "ul",
    "ol",
    "dl",
    "section",
    "pre",
    "br",
    "figcaption",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
}

VOID_TAGS = {
    "br",
    "img",
    "hr",
    "input",
    "meta",
    "link",
    "source",
    "wbr",
    "col",
    "area",
    "base",
    "embed",
    "param",
    "track",
}

HEADING_TAGS = {"h1": 1, "h2": 2, "h3": 3, "h4": 4, "h5": 5, "h6": 6}

_WHITESPACE = re.compile(r"\s+")


def normalize(text: str) -> str:
    """Collapse all whitespace and trim."""
    return _WHITESPACE.sub(" ", text).strip()


@dataclass
class Section:
    heading: str
    paragraphs: list[str] = field(default_factory=list)

    @property
    def text(self) -> str:
        return "\n\n".join(self.paragraphs)


@dataclass
class Article:
    title: str
    url: str
    sections: list[Section]

    def as_dict(self) -> dict[str, object]:
        return {
            "title": self.title,
            "url": self.url,
            "sections": [
                {"heading": section.heading, "text": section.text}
                for section in self.sections
                if section.text
            ],
        }


class _ArticleParser(HTMLParser):
    """A small, dependency-free MediaWiki article-body extractor."""

    def __init__(self) -> None:
        super().__init__(convert_charrefs=True)
        self.title = ""
        self.sections: list[Section] = []
        self._stack: list[str] = []
        self._skip_depth: int | None = None
        self._in_content = False
        self._content_depth: int | None = None
        self._buffer: list[str] = []
        self._section: Section | None = None
        self._heading_level: int | None = None
        self._heading_buffer: list[str] = []
        self._in_title = False
        self._title_buffer: list[str] = []
        self._dropping: int | None = None
        self._parent_heading = ""

    # -- helpers -------------------------------------------------------- #
    def _flush(self) -> None:
        text = normalize("".join(self._buffer))
        self._buffer = []
        if not text or self._section is None:
            return
        if self._section.paragraphs and self._section.paragraphs[-1] == text:
            return
        self._section.paragraphs.append(text)

    def _close_section(self) -> None:
        self._flush()
        if self._section is not None and self._section.text:
            self.sections.append(self._section)
        self._section = None

    def _start_section(self, heading: str) -> None:
        self._section = Section(heading)

    # -- HTML callbacks -------------------------------------------------- #
    def handle_starttag(self, tag: str, attrs) -> None:
        attributes = dict(attrs)
        classes = set((attributes.get("class") or "").split())
        identifier = attributes.get("id") or ""

        if tag not in VOID_TAGS:
            self._stack.append(tag)
        if self._skip_depth is not None:
            return

        if (
            tag not in VOID_TAGS
            and (
                tag in SKIP_TAGS
                or classes & SKIP_CLASSES
                or (tag == "sup" and "reference" in classes)
                or (
                    tag == "table"
                    and attributes.get("data-description") in SKIP_TABLE_DESCRIPTIONS
                )
            )
        ):
            self._skip_depth = len(self._stack)
            return

        if not self._in_content:
            if tag == "div" and "mw-parser-output" in classes:
                self._in_content = True
                self._content_depth = len(self._stack)
                self._start_section("Introduction")
            elif tag == "h1" and identifier == "firstHeading":
                self._in_title = True
                self._title_buffer = []
            return

        if tag in HEADING_TAGS:
            self._flush()
            self._heading_level = HEADING_TAGS[tag]
            self._heading_buffer = []
            return

        if tag in BLOCK_TAGS:
            self._flush()

    def handle_startendtag(self, tag: str, attrs) -> None:
        if tag in VOID_TAGS:
            self.handle_starttag(tag, attrs)
            return
        self.handle_starttag(tag, attrs)
        self.handle_endtag(tag)

    def handle_endtag(self, tag: str) -> None:
        if tag in VOID_TAGS:
            # `<br>` and friends are handled when they open.
            if tag in BLOCK_TAGS:
                self._flush()
            return

        if tag in HEADING_TAGS and self._heading_level is not None:
            self._finish_heading(self._heading_level, normalize("".join(self._heading_buffer)))
            self._heading_level = None
            self._heading_buffer = []

        if tag == "h1" and self._in_title:
            self._in_title = False
            self.title = normalize("".join(self._title_buffer)) or self.title

        if tag in BLOCK_TAGS:
            self._flush()

        if tag in self._stack:
            while self._stack:
                open_tag = self._stack.pop()
                if open_tag == tag:
                    break

        if self._skip_depth is not None and len(self._stack) < self._skip_depth:
            self._skip_depth = None
        if (
            self._in_content
            and self._content_depth is not None
            and len(self._stack) < self._content_depth
        ):
            self._in_content = False
            self._close_section()

    def handle_data(self, data: str) -> None:
        if self._in_title:
            self._title_buffer.append(data)
            return
        if not self._in_content:
            return
        if self._heading_level is not None:
            self._heading_buffer.append(data)
            return
        if self._skip_depth is not None:
            return
        self._buffer.append(data)

    # -- sections -------------------------------------------------------- #
    def _finish_heading(self, level: int, heading: str) -> None:
        if not heading:
            return
        if self._dropping is not None:
            if level <= self._dropping:
                self._dropping = None
            else:
                return
        if level == 2:
            self._close_section()
            if heading.lower() in DROP_SECTIONS:
                self._dropping = 2
                self._parent_heading = ""
                return
            self._parent_heading = heading
            self._start_section(heading)
        elif level == 3:
            self._close_section()
            if heading.lower() in DROP_SECTIONS:
                self._dropping = 3
                return
            parent = self._parent_heading or "Introduction"
            self._start_section(f"{parent} > {heading}")
        else:
            # Deeper headings carry no useful hierarchy; keep the words.
            self._flush()
            self._buffer.append(heading)

    def finish(self) -> None:
        self._close_section()


def extract(html: str, url: str, *, title: str = "") -> Article:
    """Parse one saved article page into title, url and sections."""
    parser = _ArticleParser()
    parser.feed(html)
    parser.close()
    parser.finish()
    page_title = parser.title or _title_from_head(html) or title or url.rsplit("/", 1)[-1]
    return Article(page_title, _canonical_url(html) or url, parser.sections)


def _canonical_url(html: str) -> str:
    """The page's canonical URL (a redirect resolves to the real article)."""
    match = re.search(
        r'<link[^>]+rel="canonical"[^>]+href="([^"]+)"', html, re.IGNORECASE
    )
    if not match:
        match = re.search(
            r'<link[^>]+href="([^"]+)"[^>]+rel="canonical"', html, re.IGNORECASE
        )
    return match.group(1) if match else ""


def _title_from_head(html: str) -> str:
    match = re.search(r"<title>(.*?)</title>", html, re.IGNORECASE | re.DOTALL)
    if not match:
        return ""
    raw = normalize(match.group(1))
    for separator in (" – ", " - ", " | "):
        if separator in raw:
            raw = raw.split(separator, 1)[0]
            break
    return raw


def extract_file(raw_path: Path, url: str, *, title: str = "") -> Article:
    return extract(raw_path.read_text(encoding="utf-8", errors="replace"), url, title=title)


def extract_all(
    raw: Path | None = None,
    text: Path | None = None,
    titles: list[str] | None = None,
) -> tuple[list[Article], list[str], list[str]]:
    """Extract every page in the fixed list that has a saved ``raw/*.html``.

    Returns ``(articles, missing, empty)``: the extracted articles, the titles
    with no saved HTML (not fetched or dropped by the fetch report) and the
    files that produced no text. Titles that redirect to an article already
    extracted (e.g. ``Hunger`` -> ``Food``) are merged into one article. Stale
    ``text/*.json`` files are removed, so the text directory always mirrors the
    indexed articles.
    """
    from .fetch import read_pages

    raw_dir = Path(raw) if raw is not None else paths.raw_dir()
    text_dir = Path(text) if text is not None else paths.text_dir()
    text_dir.mkdir(parents=True, exist_ok=True)
    articles: list[Article] = []
    missing: list[str] = []
    empty: list[str] = []
    written: set[str] = set()
    seen: set[str] = set()
    for title in titles if titles is not None else read_pages():
        name = slug(title)
        raw_path = raw_dir / f"{name}.html"
        if not raw_path.exists():
            missing.append(title)
            continue
        try:
            article = extract_file(raw_path, f"https://minecraft.wiki/w/{name}")
        except Exception:  # a broken page must not stop the build
            empty.append(title)
            continue
        # Several list titles redirect to the same article (`Hunger` -> `Food`,
        # the stronghold tutorials -> the ender dragon tutorial). Keep the first
        # one, so the index holds each article once.
        article.url = article.url.split("#", 1)[0]
        key = article.url.casefold()
        if key in seen:
            continue
        if not article.sections:
            empty.append(title)
            continue
        seen.add(key)
        target = text_dir / f"{name}.json"
        if target.exists():
            target.unlink()
        target.write_text(
            json.dumps(article.as_dict(), indent=1, ensure_ascii=False) + "\n",
            encoding="utf-8",
        )
        written.add(target.name)
        articles.append(article)
    for stale in text_dir.glob("*.json"):
        if stale.name not in written:
            stale.unlink()
    return articles, missing, empty


def load_articles(text: Path | None = None) -> list[dict[str, object]]:
    """Read every extracted article JSON, sorted by file name."""
    text_dir = Path(text) if text is not None else paths.text_dir()
    documents = []
    for path in sorted(text_dir.glob("*.json")):
        documents.append(json.loads(path.read_text(encoding="utf-8")))
    return documents


__all__ = [
    "Article",
    "DROP_SECTIONS",
    "Section",
    "extract",
    "extract_all",
    "extract_file",
    "load_articles",
    "normalize",
]
