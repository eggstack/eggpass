import asyncio
import json
import threading
from contextlib import contextmanager
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import parse_qs, urlsplit

import pytest

from eggpass_auth import (
    CallbackMismatchError,
    CancellationToken,
    Credentials,
    HttpStatusError,
    InvalidConfigurationError,
    NonceMismatchError,
    OAuthClient,
    OAuthError,
    OperationCancelledError,
    ProviderProfile,
    StateMismatchError,
)


class StubServer(ThreadingHTTPServer):
    daemon_threads = True
    allow_reuse_address = True

    def __init__(self, responses):
        self.responses = responses
        self.requests = []
        self.requests_lock = threading.Lock()
        super().__init__(("127.0.0.1", 0), self.handler_type())

    def handler_type(self):
        server = self

        class Handler(BaseHTTPRequestHandler):
            protocol_version = "HTTP/1.1"

            def do_GET(self):
                self._respond()

            def do_POST(self):
                self._respond()

            def _respond(self):
                length = int(self.headers.get("Content-Length", "0"))
                body = self.rfile.read(length) if length else b""
                record = {
                    "method": self.command,
                    "path": urlsplit(self.path).path,
                    "headers": dict(self.headers.items()),
                    "body": body,
                }
                with server.requests_lock:
                    server.requests.append(record)
                    configured = server.responses.get(record["path"], (404, b"{}"))
                    if isinstance(configured, list):
                        configured = configured.pop(0)
                status, response_body = configured
                self.send_response(status)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(response_body)))
                self.send_header("Connection", "close")
                self.end_headers()
                try:
                    self.wfile.write(response_body)
                except (BrokenPipeError, ConnectionResetError):
                    pass

            def log_message(self, _format, *_args):
                pass

        return Handler


@contextmanager
def serve(responses):
    server = StubServer(responses)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        yield f"http://127.0.0.1:{server.server_address[1]}", server
    finally:
        server.shutdown()
        server.server_close()
        thread.join(timeout=2)


def profile_for(base, scopes=()):
    builder = ProviderProfile.custom("local-test", base, "public-client")
    builder.authorization_endpoint(f"{base}/authorize")
    builder.token_endpoint(f"{base}/token")
    builder.device_authorization_endpoint(f"{base}/device")
    builder.userinfo_endpoint(f"{base}/userinfo")
    builder.revocation_endpoint(f"{base}/revoke")
    builder.callback_path("/callback")
    builder.scopes(list(scopes))
    builder.trusted_device_hosts(["accounts.test"])
    return builder.build()


def as_json(value):
    return json.dumps(value).encode()


async def call_userinfo(oauth, access_token):
    return await oauth.userinfo(access_token)


async def complete_authorization(oauth, pending, callback_url):
    return await oauth.complete_authorization(pending, callback_url)


async def refresh_credentials(oauth, credentials):
    return await oauth.refresh_credentials(credentials)


async def refresh_token(oauth, token):
    return await oauth.refresh_token(token)


async def revoke_token(oauth, token):
    return await oauth.revoke(token)


def test_async_calls_reuse_and_recreate_python_event_loops():
    with serve(
        {
            "/userinfo": (200, as_json({"sub": "account-7", "email": "user@example.test"})),
            "/revoke": (200, b"{}"),
        }
    ) as (base, server):
        oauth = OAuthClient(profile_for(base))
        async def fetch_parallel():
            return await asyncio.gather(
                call_userinfo(oauth, "access-secret"),
                call_userinfo(oauth, "access-secret"),
            )

        first, second = asyncio.run(fetch_parallel())
        third = asyncio.run(call_userinfo(oauth, "access-secret"))
        asyncio.run(revoke_token(oauth, "refresh-secret"))
        assert first.user_id == "account-7"
        assert second.email == "user@example.test"
        assert third.email == "user@example.test"
        assert len(server.requests) == 4
        assert all(request["method"] == "GET" for request in server.requests[:3])
        assert server.requests[3]["path"] == "/revoke"


def test_callback_validation_pending_one_use_and_redaction():
    token_body = as_json(
        {
            "access_token": "access-secret",
            "refresh_token": "refresh-secret",
            "expires_in": 3600,
        }
    )
    with serve({"/token": (200, token_body)}) as (base, server):
        oauth = OAuthClient(profile_for(base))
        redirect = "http://127.0.0.1:43123/callback"

        wrong_origin = oauth.begin_authorization(redirect)
        assert "redacted" in repr(wrong_origin)
        assert wrong_origin.authorization_url not in repr(wrong_origin)
        with pytest.raises(CallbackMismatchError):
            asyncio.run(
                complete_authorization(
                    oauth,
                    wrong_origin,
                    "http://example.test/callback?code=code-secret&state=wrong",
                )
            )
        assert wrong_origin.consumed

        wrong_path = oauth.begin_authorization(redirect)
        with pytest.raises(CallbackMismatchError):
            asyncio.run(
                complete_authorization(
                    oauth,
                    wrong_path,
                    "http://127.0.0.1:43123/wrong?code=code-secret&state=wrong",
                )
            )

        missing_state = oauth.begin_authorization(redirect)
        with pytest.raises(StateMismatchError):
            asyncio.run(
                complete_authorization(
                    oauth,
                    missing_state,
                    "http://127.0.0.1:43123/callback?code=code-secret",
                )
            )

        wrong_state = oauth.begin_authorization(redirect)
        with pytest.raises(StateMismatchError):
            asyncio.run(
                complete_authorization(
                    oauth,
                    wrong_state,
                    "http://127.0.0.1:43123/callback?code=code-secret&state=wrong",
                )
            )
        assert wrong_state.consumed

        duplicate_state = oauth.begin_authorization(redirect)
        with pytest.raises(CallbackMismatchError):
            asyncio.run(
                complete_authorization(
                    oauth,
                    duplicate_state,
                    "http://127.0.0.1:43123/callback?code=one&state=a&state=b",
                )
            )

        pending = oauth.begin_authorization(redirect)
        state = parse_qs(urlsplit(pending.authorization_url).query)["state"][0]
        callback = f"{redirect}?code=code-secret&state={state}"
        credentials = asyncio.run(complete_authorization(oauth, pending, callback))
        assert credentials.access_token == "access-secret"
        assert credentials.refresh_token == "refresh-secret"
        assert pending.consumed
        with pytest.raises(InvalidConfigurationError):
            oauth.complete_authorization(pending, callback)

        for rendered in (repr(credentials), repr(pending)):
            for secret in ("access-secret", "refresh-secret", "code-secret", state):
                assert secret not in rendered
        serialized = credentials.to_json()
        assert "access-secret" in serialized  # explicitly secret-bearing API
        assert Credentials.from_json(serialized).refresh_token == "refresh-secret"

        oidc_oauth = OAuthClient(profile_for(base, scopes=["openid"]))
        oidc_pending = oidc_oauth.begin_authorization(redirect)
        oidc_state = parse_qs(urlsplit(oidc_pending.authorization_url).query)["state"][0]
        with pytest.raises(NonceMismatchError):
            asyncio.run(
                complete_authorization(
                    oidc_oauth,
                    oidc_pending,
                    f"{redirect}?code=code-secret&state={oidc_state}",
                )
            )
        assert oidc_pending.consumed
        assert len(server.requests) == 2
        assert all(request["path"] == "/token" for request in server.requests)


def test_refresh_merge_and_sanitized_oauth_error():
    with serve(
        {
            "/token": [
                (200, as_json({"access_token": "rotated-access", "expires_in": 1800})),
                (
                    200,
                    as_json(
                        {
                            "access_token": "token-set-access",
                            "refresh_token": "rotated-refresh-secret",
                            "expires_in": 1800,
                        }
                    ),
                ),
                (
                    401,
                    as_json(
                        {
                            "error": "invalid_grant",
                            "error_description": "refresh-secret access-secret code-secret",
                            "device_code": "device-secret",
                        }
                    ),
                ),
            ]
        }
    ) as (base, server):
        oauth = OAuthClient(profile_for(base))
        current = Credentials.from_json(
            json.dumps(
                {
                    "access_token": "old-access",
                    "refresh_token": "refresh-secret",
                    "token_type": "Bearer",
                    "client_id": "public-client",
                    "issuer": base,
                    "expires_at": 0,
                    "obtained_at": 1,
                }
            )
        )
        updated = asyncio.run(refresh_credentials(oauth, current))
        assert updated.access_token == "rotated-access"
        assert updated.refresh_token == "refresh-secret"
        assert "rotated-access" not in repr(updated)

        token_set = asyncio.run(refresh_token(oauth, "refresh-secret"))
        assert token_set.access_token == "token-set-access"
        assert token_set.refresh_token == "rotated-refresh-secret"
        assert "token-set-access" not in repr(token_set)
        assert "rotated-refresh-secret" not in repr(token_set)

        with pytest.raises(OAuthError) as caught:
            asyncio.run(refresh_token(oauth, "refresh-secret"))
        error = caught.value
        assert error.status == 401
        assert error.code == "invalid_grant"
        for secret in ("refresh-secret", "access-secret", "code-secret", "device-secret", "error_description"):
            assert secret not in str(error)
            assert secret not in repr(error)
        assert len(server.requests) == 3


def test_device_code_repr_and_explicit_cancellation():
    device_response = as_json(
        {
            "device_code": "device-secret",
            "user_code": "human-code-secret",
            "verification_uri": "https://accounts.test/activate",
            "verification_uri_complete": "https://accounts.test/activate?user_code=human-code-secret",
            "expires_in": 30,
            "interval": 1,
        }
    )
    with serve(
        {
            "/device": (200, device_response),
            "/token": [
                (400, as_json({"error": "authorization_pending"})),
                (400, as_json({"error": "slow_down"})),
                (400, as_json({"error": "access_denied"})),
                (400, as_json({"error": "expired_token"})),
                (200, as_json({"access_token": "device-access-secret"})),
            ],
        }
    ) as (base, server):
        oauth = OAuthClient(profile_for(base))

        async def run():
            device = await oauth.request_device_code()
            rendered = repr(device)
            for secret in (
                "device-secret",
                "human-code-secret",
                "accounts.test",
            ):
                assert secret not in rendered
            assert device.user_code == "human-code-secret"
            assert device.verification_uri_complete.endswith("human-code-secret")

            one_poll = await oauth.poll_device_code(device)
            assert one_poll.status == "pending"
            assert one_poll.tokens is None
            assert (await oauth.poll_device_code(device)).status == "slow_down"
            assert (await oauth.poll_device_code(device)).status == "denied"
            assert (await oauth.poll_device_code(device)).status == "expired"
            complete = await oauth.poll_device_code(device)
            assert complete.status == "complete"
            assert complete.tokens is not None
            assert complete.tokens.access_token == "device-access-secret"
            assert "device-access-secret" not in repr(complete)

            task_token = CancellationToken()
            task = asyncio.ensure_future(
                oauth.poll_device_authorization(device, task_token, timeout_seconds=20)
            )
            await asyncio.sleep(0.05)
            task.cancel()
            with pytest.raises(asyncio.CancelledError):
                await task
            assert not task_token.is_cancelled
            # The Rust polling future must stop when its Python awaitable is cancelled.
            await asyncio.sleep(1.1)

            explicit_token = CancellationToken()
            poll_task = asyncio.ensure_future(
                oauth.poll_device_authorization(device, explicit_token, timeout_seconds=20)
            )
            await asyncio.sleep(0.05)
            explicit_token.cancel()
            with pytest.raises(OperationCancelledError):
                await poll_task

        asyncio.run(run())
        token_requests = [request for request in server.requests if request["path"] == "/token"]
        assert len(token_requests) == 5  # five direct polls; both cancelled loops stop before polling
        assert all("device-secret" not in request["body"].decode() for request in server.requests if request["path"] != "/token")
