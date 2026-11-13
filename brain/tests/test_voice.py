"""The ElevenLabs voice layer and its console routes.

No test here reaches the network: :func:`praxion_brain.voice.configure` swaps
the one transport call, so every provider answer is scripted. The key is only
ever a fake string; the route that needs it must answer 503 without one.
"""

from __future__ import annotations

import http.client
import json
from pathlib import Path

import pytest

from praxion_brain import voice
from praxion_brain.console.hub import Console
from praxion_brain.console.server import MAX_AUDIO_BYTES, MAX_BODY_BYTES, ConsoleServer
from praxion_brain.evidence import Evidence
from praxion_brain.ledger import Ledger
from praxion_brain.memory import Memory

MP3 = b"ID3\x04\x00\x00\x00\x00\x00\x00" + b"\x00" * 40
VOICES = {
    "voices": [
        {"voice_id": "v-en", "name": "Ada", "labels": {"accent": "British"}},
        {
            "voice_id": "v-tr",
            "name": "Deniz",
            "labels": {"accent": "Turkish", "gender": "female"},
            "verified_languages": [{"language": "en", "model_id": "x"}],
        },
    ]
}
STT_PAYLOAD = {
    "language_code": "tr",
    "language_probability": 0.97,
    "text": "sekiz taş getir",
    "words": [
        {"text": "sekiz", "start": 0.0, "end": 0.6},
        {"text": "taş", "start": 0.6, "end": 1.1},
    ],
}


class FakeTransport:
    """One scripted transport: records every call, answers by endpoint."""

    def __init__(self) -> None:
        self.calls: list[dict] = []
        self.tts = MP3
        self.voices = VOICES
        self.stt = STT_PAYLOAD
        self.refusal: tuple[int, dict] | None = None

    def __call__(self, method, url, *, headers, content, timeout):
        self.calls.append(
            {"method": method, "url": url, "headers": dict(headers), "content": content, "timeout": timeout}
        )
        if self.refusal is not None:
            status, payload = self.refusal
            return voice.Reply(status, json.dumps(payload).encode("utf-8"))
        if url.endswith("/v1/voices"):
            return voice.Reply(200, json.dumps(self.voices).encode("utf-8"))
        if "/v1/text-to-speech/" in url:
            return voice.Reply(200, self.tts)
        if url.endswith("/v1/speech-to-text"):
            return voice.Reply(200, json.dumps(self.stt).encode("utf-8"))
        return voice.Reply(404, b"{}")

    def endpoints(self, needle: str) -> list[dict]:
        return [call for call in self.calls if needle in call["url"]]


@pytest.fixture(autouse=True)
def clean_voice(monkeypatch):
    """Every test starts with no key, no resolved voice and no cached clip."""
    voice.configure(evidence=None, send=None)
    voice.reset()
    for name in (
        "ELEVENLABS_API_KEY",
        "ELEVENLABS_VOICE_ID",
        "ELEVENLABS_TTS_MODEL",
        "ELEVENLABS_STT_MODEL",
    ):
        monkeypatch.delenv(name, raising=False)
    yield
    voice.configure(evidence=None, send=None)
    voice.reset()


@pytest.fixture
def transport():
    fake = FakeTransport()
    voice.configure(send=fake)
    return fake


# --------------------------------------------------------------------------- #
# The speakable line
# --------------------------------------------------------------------------- #
def test_speakable_strips_guards_ids_and_inventory_tags() -> None:
    assert (
        voice.speakable("sıradaki: get one log (count(#logs) >= 1) - neden: wood")
        == "sıradaki: get one log - neden: wood"
    )
    # A nested guard is removed as one balanced group, and the sentence's own
    # closing period survives.
    assert voice.speakable("tamam: get coal (has(coal)).") == "tamam: get coal."
    assert voice.speakable("line c-0001 - neden: wood") == "line - neden: wood"
    assert voice.speakable("logs: #oak_log 3") == "logs: oak log 3"
    # A parenthesis that is not a guard is prose, not syntax: keep it.
    assert voice.speakable("selam (mutluyum)") == "selam (mutluyum)"


def test_speakable_is_one_line_of_at_most_300_chars() -> None:
    long = "kelime " * 80
    cleaned = voice.speakable(long)
    assert len(cleaned) <= voice.MAX_SPEAK_CHARS
    assert cleaned.split()[-1] == "kelime"  # cut at a word boundary
    assert voice.speakable("(count(#logs) >= 1)") == ""
    assert "\n" not in voice.speakable("bir\niki\tüç")


def test_tts_and_stt_models_have_env_overrides(monkeypatch) -> None:
    assert voice.tts_model() == voice.TTS_MODEL
    assert voice.stt_model() == voice.STT_MODEL
    monkeypatch.setenv("ELEVENLABS_TTS_MODEL", "eleven_test_tts")
    monkeypatch.setenv("ELEVENLABS_STT_MODEL", "scribe_test")
    assert voice.tts_model() == "eleven_test_tts"
    assert voice.stt_model() == "scribe_test"


# --------------------------------------------------------------------------- #
# Speech out
# --------------------------------------------------------------------------- #
def test_a_repeated_line_is_synthesized_and_recorded_once(monkeypatch, transport, tmp_path) -> None:
    monkeypatch.setenv("ELEVENLABS_API_KEY", "test-key")
    voice.configure(evidence=Evidence(tmp_path / "evidence"))

    first = voice.synthesize("sıradaki: get one log (count(#logs) >= 1)")
    second = voice.synthesize("sıradaki: get one log (count(#logs) >= 1)")

    assert first == second == MP3
    calls = transport.endpoints("/v1/text-to-speech/")
    assert len(calls) == 1
    assert calls[0]["method"] == "POST"
    assert calls[0]["timeout"] == voice.TTS_TIMEOUT
    assert calls[0]["headers"]["xi-api-key"] == "test-key"
    assert "output_format=mp3_44100_128" in calls[0]["url"]
    body = json.loads(calls[0]["content"])
    assert body["model_id"] == voice.TTS_MODEL
    # The guard and the inventory tag never reach the provider.
    assert body["text"] == "sıradaki: get one log"

    lines = [json.loads(line) for line in (tmp_path / "evidence" / "brain.jsonl").read_text().splitlines()]
    assert [line["kind"] for line in lines] == ["voice_tts"]
    assert lines[0]["chars"] == len("sıradaki: get one log")
    assert lines[0]["bytes"] == len(MP3)
    assert "cost" not in lines[0]  # no invented price


def test_the_cache_keeps_64_clips_and_evicts_the_oldest(monkeypatch, transport) -> None:
    monkeypatch.setenv("ELEVENLABS_API_KEY", "test-key")
    for index in range(voice.CACHE_SIZE + 1):
        voice.synthesize(f"satır {index}")
    assert len(transport.endpoints("/v1/text-to-speech/")) == voice.CACHE_SIZE + 1

    # The newest clip is still cached; the first one was evicted and is paid
    # for again.
    voice.synthesize(f"satır {voice.CACHE_SIZE}")
    assert len(transport.endpoints("/v1/text-to-speech/")) == voice.CACHE_SIZE + 1
    voice.synthesize("satır 0")
    assert len(transport.endpoints("/v1/text-to-speech/")) == voice.CACHE_SIZE + 2


def test_synthesize_without_a_key_raises_before_any_call(transport) -> None:
    with pytest.raises(voice.VoiceError, match="ELEVENLABS_API_KEY"):
        voice.synthesize("merhaba")
    assert transport.calls == []


def test_a_provider_refusal_is_a_typed_error(monkeypatch, transport) -> None:
    monkeypatch.setenv("ELEVENLABS_API_KEY", "test-key")
    transport.refusal = (429, {"detail": "rate limit exceeded for this key"})
    with pytest.raises(voice.VoiceError) as caught:
        voice.synthesize("merhaba")
    assert "429" in str(caught.value)
    assert "rate limit" in str(caught.value)
    assert "test-key" not in str(caught.value)


# --------------------------------------------------------------------------- #
# Which voice
# --------------------------------------------------------------------------- #
def test_the_first_account_voice_that_fits_turkish_is_resolved_once(monkeypatch, transport) -> None:
    monkeypatch.setenv("ELEVENLABS_API_KEY", "test-key")
    assert voice.voice_id() == "v-tr"
    assert voice.voice_id() == "v-tr"
    assert len(transport.endpoints("/v1/voices")) == 1


def test_verified_languages_also_mark_a_voice_as_turkish(monkeypatch, transport) -> None:
    monkeypatch.setenv("ELEVENLABS_API_KEY", "test-key")
    transport.voices = {
        "voices": [{"voice_id": "v-clone", "labels": {}, "verified_languages": [{"language": "tr-TR"}]}]
    }
    assert voice.voice_id() == "v-clone"


def test_the_voice_env_override_needs_no_request(monkeypatch, transport) -> None:
    monkeypatch.setenv("ELEVENLABS_API_KEY", "test-key")
    monkeypatch.setenv("ELEVENLABS_VOICE_ID", "v-explicit")
    assert voice.voice_id() == "v-explicit"
    assert transport.calls == []


def test_the_premade_voice_is_the_last_fallback_and_is_cached(monkeypatch, transport) -> None:
    monkeypatch.setenv("ELEVENLABS_API_KEY", "test-key")
    transport.voices = {"voices": [{"voice_id": "v-en", "labels": {"accent": "British"}}]}
    assert voice.voice_id() == voice.DEFAULT_VOICE_ID
    assert voice.voice_id() == voice.DEFAULT_VOICE_ID
    assert len(transport.endpoints("/v1/voices")) == 1


# --------------------------------------------------------------------------- #
# Speech in
# --------------------------------------------------------------------------- #
def test_transcribe_sends_the_clip_as_multipart(monkeypatch, transport, tmp_path) -> None:
    monkeypatch.setenv("ELEVENLABS_API_KEY", "test-key")
    voice.configure(evidence=Evidence(tmp_path / "evidence"))
    audio = b"\x1aE\xdf\xa3webm-bytes"

    assert voice.transcribe(audio, "audio/webm;codecs=opus") == "sekiz taş getir"

    calls = transport.endpoints("/v1/speech-to-text")
    assert len(calls) == 1
    call = calls[0]
    assert call["method"] == "POST"
    assert call["timeout"] == voice.STT_TIMEOUT
    assert call["headers"]["Content-Type"].startswith("multipart/form-data; boundary=")
    body = call["content"]
    assert audio in body
    assert b'name="file"; filename="speech.webm"' in body
    assert b"Content-Type: audio/webm\r\n" in body
    assert b'name="model_id"' in body and voice.STT_MODEL.encode() in body
    assert b'name="language_code"' in body and b"\r\ntr\r\n" in body

    lines = [json.loads(line) for line in (tmp_path / "evidence" / "brain.jsonl").read_text().splitlines()]
    assert [line["kind"] for line in lines] == ["voice_stt"]
    assert lines[0]["seconds"] == 1.1
    assert lines[0]["bytes"] == len(audio)
    assert lines[0]["language"] == "tr"


def test_an_empty_recording_raises(monkeypatch, transport) -> None:
    monkeypatch.setenv("ELEVENLABS_API_KEY", "test-key")
    with pytest.raises(voice.VoiceError, match="empty"):
        voice.transcribe(b"", "audio/webm")
    assert transport.calls == []


# --------------------------------------------------------------------------- #
# The console routes
# --------------------------------------------------------------------------- #
def build_console(tmp_path: Path) -> Console:
    return Console(
        evidence=Evidence(tmp_path / "evidence"),
        ledger=Ledger(tmp_path / "ledger.jsonl"),
        memory=Memory("w", root=tmp_path / "worlds"),
        world_id="w",
    )


@pytest.fixture
def server(tmp_path):
    running = ConsoleServer(build_console(tmp_path), 0).start()
    yield running
    running.shutdown()


def call(server, method, path, *, body=None, raw=None, headers=None):
    connection = http.client.HTTPConnection("127.0.0.1", server.port, timeout=10)
    if raw is not None:
        payload: bytes | None = raw
    else:
        payload = json.dumps(body).encode("utf-8") if body is not None else None
    connection.request(method, path, body=payload, headers=headers or {})
    response = connection.getresponse()
    content = response.read()
    connection.close()
    return response, content


def json_body(content: bytes):
    return json.loads(content) if content else None


def test_status_answers_without_a_key(server) -> None:
    response, content = call(server, "GET", "/api/voice/status")
    assert response.status == 200
    assert json_body(content) == {"enabled": False, "voice_id": ""}


def test_status_names_the_voice_when_the_key_is_present(server, monkeypatch, transport) -> None:
    monkeypatch.setenv("ELEVENLABS_API_KEY", "test-key")
    response, content = call(server, "GET", "/api/voice/status")
    assert response.status == 200
    assert json_body(content) == {"enabled": True, "voice_id": "v-tr"}


def test_tts_route_returns_mp3_from_the_cleaned_line(server, monkeypatch, transport) -> None:
    monkeypatch.setenv("ELEVENLABS_API_KEY", "test-key")
    response, content = call(
        server,
        "POST",
        "/api/voice/tts",
        body={"text": "sıradaki: get one log (count(#logs) >= 1)"},
        headers={"Content-Type": "application/json"},
    )
    assert response.status == 200
    assert response.getheader("Content-Type") == "audio/mpeg"
    assert content == MP3
    tts = transport.endpoints("/v1/text-to-speech/")
    assert json.loads(tts[0]["content"])["text"] == "sıradaki: get one log"


def test_tts_route_refuses_bad_bodies(server, monkeypatch, transport) -> None:
    monkeypatch.setenv("ELEVENLABS_API_KEY", "test-key")
    for body in ({"text": "   "}, {"nothing": 1}, ["merhaba"], {"text": "x" * 2001}):
        response, content = call(
            server, "POST", "/api/voice/tts", body=body, headers={"Content-Type": "application/json"}
        )
        assert response.status == 400, body
        assert "error" in json_body(content)
    response, _ = call(
        server,
        "POST",
        "/api/voice/tts",
        raw=b"{",
        headers={
            "Content-Type": "application/json",
            "Content-Length": str(MAX_BODY_BYTES + 1),
        },
    )
    assert response.status == 413
    response, _ = call(
        server, "POST", "/api/voice/tts", raw=b"not json", headers={"Content-Type": "application/json"}
    )
    assert response.status == 400
    assert transport.calls == []


def test_stt_route_returns_the_transcript(server, monkeypatch, transport) -> None:
    monkeypatch.setenv("ELEVENLABS_API_KEY", "test-key")
    audio = b"\x1aE\xdf\xa3recorded"
    response, content = call(
        server, "POST", "/api/voice/stt", raw=audio, headers={"Content-Type": "audio/webm;codecs=opus"}
    )
    assert response.status == 200
    assert json_body(content) == {"text": "sekiz taş getir"}
    assert audio in transport.endpoints("/v1/speech-to-text")[0]["content"]


def test_stt_route_refuses_a_non_audio_body_and_an_oversized_clip(server, monkeypatch, transport) -> None:
    monkeypatch.setenv("ELEVENLABS_API_KEY", "test-key")
    response, _ = call(server, "POST", "/api/voice/stt", raw=b"abc", headers={"Content-Type": "text/plain"})
    assert response.status == 415
    response, _ = call(
        server,
        "POST",
        "/api/voice/stt",
        raw=b"\x00",
        headers={
            "Content-Type": "audio/webm",
            "Content-Length": str(MAX_AUDIO_BYTES + 1),
        },
    )
    assert response.status == 413
    assert transport.calls == []


def test_the_voice_routes_answer_503_without_a_key(server, transport) -> None:
    response, _ = call(server, "POST", "/api/voice/tts", body={"text": "merhaba"})
    assert response.status == 503
    response, content = call(server, "POST", "/api/voice/stt", raw=b"abc", headers={"Content-Type": "audio/webm"})
    assert response.status == 503
    assert "ELEVENLABS_API_KEY" in json_body(content)["error"]
    assert transport.calls == []


def test_a_provider_failure_is_a_bad_gateway_not_a_500(server, monkeypatch, transport) -> None:
    monkeypatch.setenv("ELEVENLABS_API_KEY", "test-key")
    transport.refusal = (500, {"detail": "upstream exploded"})
    response, content = call(server, "POST", "/api/voice/tts", body={"text": "merhaba"})
    assert response.status == 502
    assert "upstream exploded" in json_body(content)["error"]


# --------------------------------------------------------------------------- #
# The narration lines
# --------------------------------------------------------------------------- #
def speak(console: Console, line_id: str, text: str, topic: str = "goal_started") -> None:
    console.evidence.write("narrate", topic=topic, line_id=line_id, text=text, status="send_pending")
    console.evidence.write("narrate_sent", line_id=line_id, text=text)


def lines(server, after: str | None = None):
    path = "/api/voice/lines" + ("" if after is None else f"?after={after}")
    response, content = call(server, "GET", path)
    assert response.status == 200
    return json_body(content)


def test_a_fresh_page_gets_the_newest_id_and_no_lines(server) -> None:
    console = server.console
    speak(console, "c-0001", "bir")
    speak(console, "c-0002", "iki")
    assert lines(server) == {"lines": [], "last": "c-0002"}


def test_no_narration_yet_reports_a_null_last_id(server) -> None:
    assert lines(server) == {"lines": [], "last": None}


def test_lines_are_paged_by_line_id(server) -> None:
    console = server.console
    console.evidence.write("loop_started", world_id="w")
    speak(console, "c-0001", "sıradaki: get one log")
    speak(console, "c-0002", "bitti: get one log", topic="goal_done")

    after = lines(server, "c-0001")
    assert after["lines"] == [
        {"line_id": "c-0002", "text": "bitti: get one log", "topic": "goal_done"}
    ]
    assert after["last"] == "c-0002"


def test_an_unknown_after_gets_silence_and_the_newest_id(server) -> None:
    console = server.console
    console.evidence.write("loop_started", world_id="w")
    speak(console, "c-0001", "bitti: get one log")
    assert lines(server, "c-9999") == {"lines": [], "last": "c-0001"}


def test_an_empty_after_means_the_page_is_new_but_ready(server) -> None:
    console = server.console
    console.evidence.write("loop_started", world_id="w")
    speak(console, "c-0001", "bir")
    speak(console, "c-0002", "iki")
    found = lines(server, "")
    assert [line["line_id"] for line in found["lines"]] == ["c-0001", "c-0002"]
    assert found["last"] == "c-0002"


def test_lines_stop_at_the_current_runs_start(server) -> None:
    console = server.console
    speak(console, "c-0001", "eski satır")
    console.evidence.write("loop_started", world_id="w")
    speak(console, "c-0001", "yeni satır")
    found = lines(server, "")
    assert [line["text"] for line in found["lines"]] == ["yeni satır"]


def test_a_poll_returns_at_most_20_lines(server) -> None:
    console = server.console
    console.evidence.write("loop_started", world_id="w")
    for index in range(25):
        speak(console, f"c-{index + 1:04d}", f"satır {index}")
    found = lines(server, "")
    assert len(found["lines"]) == 20
    assert found["lines"][0]["line_id"] == "c-0006"
    assert found["last"] == "c-0025"
