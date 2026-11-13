"""ElevenLabs voice for the web console: speech out, push-to-talk in.

Decision 2026-11-12 "Voice": the console speaks the narration lines the bot
already sends to chat (:mod:`.speech`, evidence kinds ``narrate`` /
``narrate_sent``) and transcribes the operator's held-key speech, which then
goes down the existing free-text task path (``POST /api/control`` action
``task`` -> ``Loop._handle_user_text``).

Rules this module keeps:

* the API key comes from the environment (``.env`` via :mod:`.config`) and never
  leaves the process: the browser only ever gets mp3 bytes and a transcript;
* a line that would be read aloud carries condition guard text
  (``(count(#logs) >= 1)``) and short ids (``c-0001``); :func:`speakable` strips
  them so the voice does not spell a guard out;
* repeated lines are not paid for twice: a 64-entry LRU cache keeps the last
  clips of one process;
* no ElevenLabs price is in :data:`praxion_brain.ledger.PRICES`, so no cost line
  is invented: each call writes a ``voice_tts`` / ``voice_stt`` evidence line
  with its characters and seconds instead, and :meth:`Ledger.record` stays
  untouched until a real price is added.

Model ids and the endpoint shapes were checked against the ElevenLabs docs on
2026-11-12: ``POST /v1/text-to-speech/{voice_id}`` (JSON body, mp3 output),
``POST /v1/speech-to-text`` (multipart ``file``) and ``GET /v1/voices``.
``eleven_flash_v2_5`` is the low-latency multilingual model that lists Turkish;
``scribe_v2`` is the current Scribe transcription model.
"""

from __future__ import annotations

import json
import os
import re
import threading
import uuid
from collections import OrderedDict
from typing import Any, Callable, Mapping, NamedTuple, Optional
from urllib.parse import quote

from . import config  # noqa: F401 - import loads the machine-local .env

#: The API origin; every request goes here, nothing else.
API_BASE = "https://api.elevenlabs.io"
#: Low-latency multilingual text-to-speech; its language list includes `tr`.
TTS_MODEL = "eleven_flash_v2_5"
#: The current Scribe speech-to-text model.
STT_MODEL = "scribe_v2"
#: The documented premade multilingual voice ("Sarah"), used as the last
#: fallback when neither the environment nor a labelled account voice fits.
#: Rachel's `21m00Tcm4TlvDq8ikWAM` is a library voice now: a free plan gets a
#: 402 for it, while this premade voice answers on every plan (measured
#: 2026-11-12).
DEFAULT_VOICE_ID = "EXAVITQu4vr4xnSDxMaL"
#: The mp3 shape the browser plays; a voice reply is a few seconds at most.
OUTPUT_FORMAT = "mp3_44100_128"
TTS_TIMEOUT = 15.0
STT_TIMEOUT = 20.0
#: A cached clip per distinct spoken line; repeated narrations are free.
CACHE_SIZE = 64
#: What one spoken line may carry; the speech templates stay under 200 anyway.
MAX_SPEAK_CHARS = 300

API_KEY_ENV = "ELEVENLABS_API_KEY"
_TTS_MODEL_ENV = "ELEVENLABS_TTS_MODEL"
_STT_MODEL_ENV = "ELEVENLABS_STT_MODEL"
_VOICE_ID_ENV = "ELEVENLABS_VOICE_ID"

#: A guard group is dropped only when it carries condition syntax: dropping
#: every parenthesis would eat a real aside from a model-written sentence.
_CONDITION_MARKS = (">=", "<=", "count(", "has(", "#", " not ", " and ", " or ")
#: Chat ids such as `c-0001` and goal ids such as `g-0012`.
_LINE_ID = re.compile(r"\b[a-z]-\d{3,}\b", re.IGNORECASE)
#: `#logs` is an inventory tag; the word is what should be spoken.
_TAG = re.compile(r"#(\w+)")
_EXTENSIONS = {
    "audio/webm": "webm",
    "audio/ogg": "ogg",
    "audio/wav": "wav",
    "audio/x-wav": "wav",
    "audio/mp4": "mp4",
    "audio/mpeg": "mp3",
    "audio/mp3": "mp3",
    "audio/flac": "flac",
}


class VoiceError(RuntimeError):
    """A voice call failed: no key, nothing to say, or the provider refused."""


class Reply(NamedTuple):
    """What one transport call returns; :data:`_send` is the only transport."""

    status: int
    content: bytes


#: ``(method, url, headers, content, timeout) -> Reply``.
Transport = Callable[..., Reply]


def _default_send(
    method: str,
    url: str,
    *,
    headers: Mapping[str, str],
    content: bytes,
    timeout: float,
) -> Reply:
    """The real transport: one short-lived ``httpx2`` client per request."""
    import httpx2

    with httpx2.Client(timeout=timeout) as client:
        response = client.request(method, url, headers=dict(headers), content=content)
    return Reply(response.status_code, response.content)


class _Unset:
    """Distinguishes "leave the setting alone" from an explicit ``None``."""


_UNSET = _Unset()
_send: Transport = _default_send
_evidence: Optional[Any] = None
_lock = threading.Lock()
_CACHE: "OrderedDict[str, bytes]" = OrderedDict()
_resolved_voice: Optional[str] = None


def configure(*, evidence: Any = _UNSET, send: Any = _UNSET) -> None:
    """Point the module at this run's evidence log and (in tests) a transport.

    The server calls this once with its :class:`~.evidence.Evidence`; tests
    replace ``send`` so no call reaches the network. Omitted arguments keep
    their current value, so a test may set the transport and let the server set
    the evidence.
    """
    global _evidence, _send
    if evidence is not _UNSET:
        _evidence = evidence
    if send is not _UNSET:
        _send = _default_send if send is None else send


def reset() -> None:
    """Forget the resolved voice and every cached clip (tests, provider change)."""
    global _resolved_voice
    with _lock:
        _CACHE.clear()
        _resolved_voice = None


# ---------------------------------------------------------------------- #
# Settings
# ---------------------------------------------------------------------- #
def _key() -> str:
    return os.environ.get(API_KEY_ENV, "").strip()


def enabled() -> bool:
    """Whether the key needed for both directions is present."""
    return bool(_key())


def tts_model() -> str:
    return os.environ.get(_TTS_MODEL_ENV, "").strip() or TTS_MODEL


def stt_model() -> str:
    return os.environ.get(_STT_MODEL_ENV, "").strip() or STT_MODEL


def _fits_turkish(voice: Mapping[str, Any], language: str = "tr") -> bool:
    """Whether one account voice advertises Turkish or multilingual speech.

    Two honest signals exist in ``GET /v1/voices``: ``verified_languages``
    (the languages a cloned voice was verified in) and the free-form ``labels``
    (for example ``{"accent": "Turkish"}``). Anything else would be a guess,
    and the documented premade fallback covers the rest.
    """
    for item in voice.get("verified_languages") or []:
        if isinstance(item, Mapping):
            found = str(item.get("language") or "").strip().lower().split("-")[0]
            if found == language:
                return True
    labels = voice.get("labels") or {}
    if isinstance(labels, Mapping):
        words = re.split(r"[^a-z0-9]+", " ".join(str(value).lower() for value in labels.values()))
        if language in words or "turkish" in words or "multilingual" in words:
            return True
    return False


def _account_voice() -> Optional[str]:
    """The first account voice that fits Turkish; ``None`` when none does."""
    response = _send(
        "GET",
        f"{API_BASE}/v1/voices",
        headers={"xi-api-key": _key()},
        content=b"",
        timeout=TTS_TIMEOUT,
    )
    if response.status >= 400:
        return None
    try:
        payload = json.loads(response.content or b"{}")
    except ValueError:
        return None
    voices = payload.get("voices") if isinstance(payload, Mapping) else None
    for voice in voices or []:
        if isinstance(voice, Mapping) and voice.get("voice_id") and _fits_turkish(voice):
            return str(voice["voice_id"])
    return None


def voice_id() -> str:
    """The voice every request uses.

    An explicit ``ELEVENLABS_VOICE_ID`` wins. Otherwise the first account voice
    whose labels or verified languages say Turkish/multilingual is resolved
    once and cached; when the account has none (or cannot be asked) the
    documented premade voice is used. Without a key nothing is asked at all.
    """
    global _resolved_voice
    override = os.environ.get(_VOICE_ID_ENV, "").strip()
    if override:
        return override
    with _lock:
        cached = _resolved_voice
    if cached is not None:
        return cached
    resolved: Optional[str] = None
    if enabled():
        try:
            resolved = _account_voice()
        except Exception:  # a listing failure must not stop the panel
            resolved = None
    # The outcome is cached, the fallback included: one lookup per process.
    chosen = resolved or DEFAULT_VOICE_ID
    with _lock:
        _resolved_voice = chosen
    return chosen


# ---------------------------------------------------------------------- #
# Speakable text
# ---------------------------------------------------------------------- #
def _drop_guards(text: str) -> str:
    """Remove balanced parenthesised groups that carry condition syntax."""
    parts: list[str] = []
    index = 0
    while index < len(text):
        if text[index] != "(":
            parts.append(text[index])
            index += 1
            continue
        depth, end = 1, index + 1
        while end < len(text) and depth:
            if text[end] == "(":
                depth += 1
            elif text[end] == ")":
                depth -= 1
            end += 1
        if depth:  # unbalanced: no guard starts here, keep the rest as written
            parts.append(text[index:])
            break
        group = text[index:end]
        inner = group[1:-1]
        if not inner.strip() or any(mark in group for mark in _CONDITION_MARKS):
            parts.append(" ")
        else:
            parts.append(group)
        index = end
    return "".join(parts)


def speakable(text: str, limit: int = MAX_SPEAK_CHARS) -> str:
    """One line as it should be read: guards, ids and inventory tags removed.

    ``sıradaki: get one log (count(#logs) >= 1) - neden: wood`` becomes
    ``sıradaki: get one log - neden: wood``; ``oak_log`` becomes ``oak log`` so
    the model reads words, not identifiers. The result is one line of at most
    ``limit`` characters, cut at a word boundary so a word is never cut in two.
    """
    text = _drop_guards(str(text))
    text = _LINE_ID.sub(" ", text)
    text = _TAG.sub(r"\1", text).replace("#", "")
    text = text.replace("_", " ")
    text = "".join(ch if ch.isprintable() else " " for ch in text)
    text = re.sub(r"\s+", " ", text).strip()
    text = re.sub(r"\s+([,.;:!?])", r"\1", text)
    text = re.sub(r"^[\s,;:\-–]+", "", text)
    text = re.sub(r"[\s,;:\-–]+$", "", text)
    if len(text) > limit:
        cut = text[: limit + 1]
        space = cut.rfind(" ")
        text = cut[:space] if space > limit // 2 else text[:limit]
        text = text.rstrip(" ,;:-–")
    return text


# ---------------------------------------------------------------------- #
# Cache and records
# ---------------------------------------------------------------------- #
def _cache_get(key: str) -> Optional[bytes]:
    with _lock:
        found = _CACHE.get(key)
        if found is not None:
            _CACHE.move_to_end(key)
        return found


def _cache_put(key: str, audio: bytes) -> None:
    with _lock:
        _CACHE[key] = audio
        _CACHE.move_to_end(key)
        while len(_CACHE) > CACHE_SIZE:
            _CACHE.popitem(last=False)


def _record(kind: str, **fields: Any) -> None:
    """One evidence line; a console without an evidence log writes nothing."""
    evidence = _evidence
    if evidence is not None:
        evidence.write(kind, provider="elevenlabs", **fields)


def _failure(what: str, response: Reply) -> VoiceError:
    """The provider's refusal as one bounded sentence (never the request)."""
    detail = ""
    try:
        payload = json.loads(response.content or b"{}")
    except ValueError:
        payload = None
    if isinstance(payload, Mapping):
        value = payload.get("detail") or payload.get("message") or payload.get("error")
        if isinstance(value, (list, dict)):
            value = json.dumps(value, ensure_ascii=False)
        detail = re.sub(r"\s+", " ", str(value or "")).strip()[:200]
    return VoiceError(f"{what} failed ({response.status})" + (f": {detail}" if detail else ""))


# ---------------------------------------------------------------------- #
# Speech out
# ---------------------------------------------------------------------- #
def synthesize(text: str) -> bytes:
    """Speak ``text``; returns mp3 bytes from the account's voice.

    The cleaned line is the cache key, so the same narration line is fetched
    once per process. A cache hit writes nothing: no request, no cost.
    """
    line = speakable(text)
    if not line:
        raise VoiceError("nothing to say")
    cached = _cache_get(line)
    if cached is not None:
        return cached
    key = _key()
    if not key:
        raise VoiceError(f"voice needs {API_KEY_ENV} in .env")
    voice_used = voice_id()
    body = json.dumps({"text": line, "model_id": tts_model()}).encode("utf-8")
    response = _send(
        "POST",
        f"{API_BASE}/v1/text-to-speech/{quote(voice_used, safe='')}?output_format={OUTPUT_FORMAT}",
        headers={
            "xi-api-key": key,
            "Content-Type": "application/json",
            "Accept": "audio/mpeg",
        },
        content=body,
        timeout=TTS_TIMEOUT,
    )
    if response.status >= 400:
        raise _failure("text-to-speech", response)
    if not response.content:
        raise VoiceError("text-to-speech returned no audio")
    _cache_put(line, response.content)
    _record(
        "voice_tts",
        model=tts_model(),
        voice_id=voice_used,
        chars=len(line),
        bytes=len(response.content),
    )
    return response.content


# ---------------------------------------------------------------------- #
# Speech in
# ---------------------------------------------------------------------- #
def _multipart(
    content: bytes, content_type: str, fields: Mapping[str, str]
) -> tuple[bytes, str]:
    """One multipart body: the audio part plus the string fields."""
    base = re.sub(r"[^a-z0-9/+.-]", "", content_type.split(";")[0].strip().lower())
    if not base:
        base = "application/octet-stream"
    extension = _EXTENSIONS.get(base, "webm")
    filename = f"speech.{extension}"
    boundary = "praxion-voice-" + uuid.uuid4().hex
    parts: list[bytes] = []
    for name, value in fields.items():
        parts.append(
            (
                f'--{boundary}\r\nContent-Disposition: form-data; name="{name}"\r\n\r\n'
                f"{value}\r\n"
            ).encode("utf-8")
        )
    parts.append(
        (
            f'--{boundary}\r\nContent-Disposition: form-data; name="file"; '
            f'filename="{filename}"\r\nContent-Type: {base}\r\n\r\n'
        ).encode("utf-8")
        + content
        + b"\r\n"
    )
    parts.append(f"--{boundary}--\r\n".encode("utf-8"))
    return b"".join(parts), f"multipart/form-data; boundary={boundary}"


def _seconds(payload: Mapping[str, Any]) -> Optional[float]:
    """The clip's length from the last word timestamp, when the API sends one."""
    ends: list[float] = []
    for word in payload.get("words") or []:
        if not isinstance(word, Mapping):
            continue
        try:
            ends.append(float(word["end"]))
        except (KeyError, TypeError, ValueError):
            continue
    return round(max(ends), 2) if ends else None


def transcribe(audio: bytes, content_type: str, language: str = "tr") -> str:
    """Transcribe one recorded clip; returns the text, possibly empty."""
    if not audio:
        raise VoiceError("the recording is empty")
    key = _key()
    if not key:
        raise VoiceError(f"voice needs {API_KEY_ENV} in .env")
    body, multipart_type = _multipart(
        audio,
        content_type,
        {"model_id": stt_model(), "language_code": language},
    )
    response = _send(
        "POST",
        f"{API_BASE}/v1/speech-to-text",
        headers={"xi-api-key": key, "Content-Type": multipart_type},
        content=body,
        timeout=STT_TIMEOUT,
    )
    if response.status >= 400:
        raise _failure("speech-to-text", response)
    try:
        payload = json.loads(response.content or b"{}")
    except ValueError as error:
        raise VoiceError("speech-to-text returned a body that is not JSON") from error
    if not isinstance(payload, Mapping):
        raise VoiceError("speech-to-text returned an unexpected body")
    text = " ".join(str(payload.get("text") or "").split())
    seconds = _seconds(payload)
    _record(
        "voice_stt",
        model=stt_model(),
        language=str(payload.get("language_code") or language),
        bytes=len(audio),
        chars=len(text),
        seconds=seconds,
    )
    return text


__all__ = [
    "API_BASE",
    "API_KEY_ENV",
    "CACHE_SIZE",
    "DEFAULT_VOICE_ID",
    "MAX_SPEAK_CHARS",
    "OUTPUT_FORMAT",
    "STT_MODEL",
    "STT_TIMEOUT",
    "TTS_MODEL",
    "TTS_TIMEOUT",
    "Reply",
    "VoiceError",
    "configure",
    "enabled",
    "reset",
    "speakable",
    "stt_model",
    "synthesize",
    "transcribe",
    "tts_model",
    "voice_id",
]
