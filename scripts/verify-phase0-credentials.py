#!/usr/bin/env python3
"""Safely preflight Phase 0 Telegram, Google OAuth, and Hostinger credentials.

The checks are deliberately non-mutating: they do not send Telegram messages,
create Google resources, revoke grants, or submit SMTP messages.

Secrets are read from the process environment and, when present, from
.credentials/phase0.env. Existing ignored files named ``telpass`` (one raw bot
token) and ``googleoauth`` (a downloaded Google Web OAuth JSON file) are also
supported for local compatibility.
"""

from __future__ import annotations

import argparse
import http.client
import ipaddress
import json
import os
import re
import smtplib
import socket
import ssl
from smtplib import SMTPAuthenticationError, SMTPException
from ssl import SSLError
import stat
import sys
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Mapping
from urllib.parse import urlencode, urlparse

DEFAULT_ENV_FILE = Path(".credentials/phase0.env")
DEFAULT_TELEGRAM_TOKEN_FILE = Path("telpass")
DEFAULT_GOOGLE_CREDENTIALS_FILE = Path("googleoauth")
MAX_RESPONSE_BYTES = 1_000_000

REQUIRED_GOOGLE_SCOPES = frozenset(
    {
        "openid",
        "email",
        "https://www.googleapis.com/auth/drive.file",
        "https://www.googleapis.com/auth/calendar.calendarlist.readonly",
        "https://www.googleapis.com/auth/calendar.events",
    }
)


@dataclass(frozen=True)
class HttpResult:
    status: int
    payload: Mapping[str, Any]


class Reporter:
    def __init__(self) -> None:
        self.passed = 0
        self.failed = 0
        self.incomplete = 0

    def pass_(self, label: str, detail: str) -> None:
        self.passed += 1
        print(f"[PASS] {label}: {detail}")

    def fail(self, label: str, detail: str) -> None:
        self.failed += 1
        print(f"[FAIL] {label}: {detail}")

    def incomplete_(self, label: str, detail: str) -> None:
        self.incomplete += 1
        print(f"[INCOMPLETE] {label}: {detail}")

    def exit_code(self) -> int:
        print(
            f"\nSummary: {self.passed} passed, {self.failed} failed, "
            f"{self.incomplete} incomplete"
        )
        if self.failed:
            return 1
        if self.incomplete:
            return 2
        return 0


def warn_if_permissions_are_broad(path: Path) -> None:
    """Warn when a local secret file is readable by group or other users."""
    try:
        mode = stat.S_IMODE(path.stat().st_mode)
    except OSError:
        return
    if mode & 0o077:
        print(
            f"[WARN] local credential file {path} has mode {mode:04o}; "
            f"use: chmod 600 {path}",
            file=sys.stderr,
        )


def load_env_file(path: Path) -> dict[str, str]:
    """Load a small KEY=value file without shell expansion or interpolation."""
    values: dict[str, str] = {}
    if not path.exists():
        return values
    if not path.is_file():
        raise ValueError(f"environment path is not a regular file: {path}")

    warn_if_permissions_are_broad(path)
    for line_number, raw_line in enumerate(
        path.read_text(encoding="utf-8").splitlines(), start=1
    ):
        line = raw_line.strip()
        if not line or line.startswith("#"):
            continue
        if line.startswith("export "):
            line = line[7:].lstrip()
        if "=" not in line:
            raise ValueError(f"{path}:{line_number}: expected KEY=value")
        key, value = line.split("=", 1)
        key = key.strip()
        value = value.strip()
        if not re.fullmatch(r"[A-Za-z_][A-Za-z0-9_]*", key):
            raise ValueError(f"{path}:{line_number}: invalid variable name")
        if len(value) >= 2 and value[0] == value[-1] and value[0] in {"'", '"'}:
            value = value[1:-1]
        values[key] = value
    return values


def load_settings(env_file: Path) -> dict[str, str]:
    values = load_env_file(env_file)
    values.update(os.environ)
    return values


def read_legacy_telegram_token(settings: Mapping[str, str]) -> str | None:
    token = settings.get("TELEGRAM_BOT_TOKEN", "").strip()
    if token:
        return token
    if not DEFAULT_TELEGRAM_TOKEN_FILE.exists():
        return None

    warn_if_permissions_are_broad(DEFAULT_TELEGRAM_TOKEN_FILE)
    lines = DEFAULT_TELEGRAM_TOKEN_FILE.read_text(encoding="utf-8").splitlines()
    if len(lines) != 1 or not lines[0].strip():
        raise ValueError("telpass must contain exactly one non-empty token line")
    return lines[0].strip()


def request_json(
    host: str,
    path: str,
    *,
    timeout: float,
    method: str = "GET",
    form: Mapping[str, str] | None = None,
    bearer_token: str | None = None,
) -> HttpResult:
    headers = {"Accept": "application/json", "User-Agent": "novus-phase0-preflight/1"}
    body: bytes | None = None
    if form is not None:
        body = urlencode(form).encode("utf-8")
        headers["Content-Type"] = "application/x-www-form-urlencoded"
    if bearer_token is not None:
        headers["Authorization"] = f"Bearer {bearer_token}"

    connection = http.client.HTTPSConnection(
        host, timeout=timeout, context=ssl.create_default_context()
    )
    try:
        connection.request(method, path, body=body, headers=headers)
        response = connection.getresponse()
        raw = response.read(MAX_RESPONSE_BYTES + 1)
        if len(raw) > MAX_RESPONSE_BYTES:
            raise ValueError("provider response exceeded the safety limit")
        try:
            payload = json.loads(raw.decode("utf-8")) if raw else {}
        except (UnicodeDecodeError, json.JSONDecodeError) as error:
            raise ValueError("provider returned a non-JSON response") from error
        if not isinstance(payload, dict):
            raise ValueError("provider returned an unexpected JSON shape")
        return HttpResult(response.status, payload)
    finally:
        connection.close()


def is_json_true(value: Any) -> bool:
    return isinstance(value, bool) and value


def verify_telegram(
    settings: Mapping[str, str], reporter: Reporter, timeout: float
) -> None:
    try:
        token = read_legacy_telegram_token(settings)
    except (OSError, UnicodeError, ValueError) as error:
        reporter.fail("Telegram configuration", str(error))
        return

    if not token:
        reporter.incomplete_(
            "Telegram credentials",
            "set TELEGRAM_BOT_TOKEN or place one raw token line in ./telpass",
        )
        return

    try:
        result = request_json(
            "api.telegram.org", f"/bot{token}/getMe", timeout=timeout, method="POST"
        )
    except (OSError, ValueError, http.client.HTTPException):
        reporter.fail("Telegram credentials", "Bot API request failed (details redacted)")
        return

    bot = result.payload.get("result")
    if result.status != 200 or not is_json_true(result.payload.get("ok")):
        reporter.fail(
            "Telegram credentials",
            f"getMe rejected the token (HTTP {result.status}; response redacted)",
        )
        return
    if not isinstance(bot, dict) or not is_json_true(bot.get("is_bot")):
        reporter.fail("Telegram credentials", "getMe returned an unexpected bot identity")
        return
    reporter.pass_("Telegram credentials", "getMe authenticated the development bot")

    try:
        webhook = request_json(
            "api.telegram.org",
            f"/bot{token}/getWebhookInfo",
            timeout=timeout,
            method="POST",
        )
    except (OSError, ValueError, http.client.HTTPException):
        reporter.fail("Telegram webhook read", "getWebhookInfo failed (details redacted)")
    else:
        if webhook.status == 200 and is_json_true(webhook.payload.get("ok")):
            reporter.pass_("Telegram webhook read", "getWebhookInfo succeeded")
        else:
            reporter.fail(
                "Telegram webhook read",
                f"getWebhookInfo failed (HTTP {webhook.status}; response redacted)",
            )

    chat_id = settings.get("TELEGRAM_FORUM_CHAT_ID", "").strip()
    if not chat_id:
        return
    try:
        chat_result = request_json(
            "api.telegram.org",
            f"/bot{token}/getChat",
            timeout=timeout,
            method="POST",
            form={"chat_id": chat_id},
        )
    except (OSError, ValueError, http.client.HTTPException):
        reporter.fail("Telegram forum", "getChat failed (details redacted)")
        return

    chat = chat_result.payload.get("result")
    if chat_result.status != 200 or not isinstance(chat, dict):
        reporter.fail(
            "Telegram forum",
            f"getChat rejected the configured chat (HTTP {chat_result.status})",
        )
    elif chat.get("type") != "supergroup" or not is_json_true(chat.get("is_forum")):
        reporter.fail(
            "Telegram forum", "configured chat is not a forum supergroup"
        )
    else:
        reporter.pass_("Telegram forum", "getChat confirmed a forum supergroup")


def is_allowed_google_redirect_uri(redirect_uri: str) -> bool:
    parsed = urlparse(redirect_uri)
    if not parsed.netloc or parsed.username or parsed.password or parsed.fragment:
        return False
    if parsed.scheme == "https":
        return True
    if parsed.scheme != "http" or parsed.hostname is None:
        return False
    if parsed.hostname.lower() == "localhost":
        return True
    try:
        return ipaddress.ip_address(parsed.hostname).is_loopback
    except ValueError:
        return False


def load_google_credentials(settings: Mapping[str, str]) -> dict[str, Any]:
    client_id = settings.get("GOOGLE_OAUTH_CLIENT_ID", "").strip()
    client_secret = settings.get("GOOGLE_OAUTH_CLIENT_SECRET", "").strip()
    redirect_uri = settings.get("GOOGLE_OAUTH_REDIRECT_URI", "").strip()
    configured_path = settings.get("GOOGLE_OAUTH_CREDENTIALS_FILE", "").strip()

    path: Path | None = Path(configured_path).expanduser() if configured_path else None
    if path is None and DEFAULT_GOOGLE_CREDENTIALS_FILE.exists():
        path = DEFAULT_GOOGLE_CREDENTIALS_FILE

    source_redirect_uris: list[str] = []
    if path is not None:
        if not path.is_file():
            raise ValueError("Google OAuth credential path is not a regular file")
        warn_if_permissions_are_broad(path)
        try:
            payload = json.loads(path.read_text(encoding="utf-8"))
        except (OSError, UnicodeError, json.JSONDecodeError) as error:
            raise ValueError("Google OAuth credential file is not valid JSON") from error
        if not isinstance(payload, dict) or not isinstance(payload.get("web"), dict):
            raise ValueError("Google OAuth credential JSON must contain a Web client")
        web = payload["web"]
        client_id = client_id or str(web.get("client_id", "")).strip()
        client_secret = client_secret or str(web.get("client_secret", "")).strip()
        raw_redirects = web.get("redirect_uris", [])
        if isinstance(raw_redirects, list):
            source_redirect_uris = [
                item.strip() for item in raw_redirects if isinstance(item, str) and item.strip()
            ]
        if not redirect_uri and len(source_redirect_uris) == 1:
            redirect_uri = source_redirect_uris[0]

    if not client_id or not client_secret:
        raise ValueError(
            "set GOOGLE_OAUTH_CLIENT_ID and GOOGLE_OAUTH_CLIENT_SECRET, or provide "
            "GOOGLE_OAUTH_CREDENTIALS_FILE/./googleoauth"
        )
    if not redirect_uri:
        raise ValueError(
            "set GOOGLE_OAUTH_REDIRECT_URI (it must exactly match a registered URI)"
        )
    if not is_allowed_google_redirect_uri(redirect_uri):
        raise ValueError(
            "GOOGLE_OAUTH_REDIRECT_URI must use HTTPS, except for an HTTP "
            "localhost/loopback development callback"
        )
    if source_redirect_uris and redirect_uri not in source_redirect_uris:
        raise ValueError("configured redirect URI is absent from the OAuth client JSON")

    return {
        "client_id": client_id,
        "client_secret": client_secret,
        "redirect_uri": redirect_uri,
        "refresh_token": settings.get("GOOGLE_OAUTH_REFRESH_TOKEN", "").strip(),
    }


def normalize_google_scopes(scopes: set[str]) -> set[str]:
    aliases = {
        "https://www.googleapis.com/auth/userinfo.email": "email",
        "https://www.googleapis.com/auth/userinfo.profile": "profile",
    }
    return {aliases.get(scope, scope) for scope in scopes}


def has_forbidden_google_scope(scopes: set[str]) -> bool:
    return any(
        scope == "https://mail.google.com/"
        or scope.startswith("https://www.googleapis.com/auth/gmail.")
        for scope in scopes
    )


def verify_google(
    settings: Mapping[str, str], reporter: Reporter, timeout: float
) -> None:
    try:
        credentials = load_google_credentials(settings)
    except ValueError as error:
        reporter.incomplete_("Google OAuth configuration", str(error))
        return

    try:
        discovery = request_json(
            "accounts.google.com",
            "/.well-known/openid-configuration",
            timeout=timeout,
        )
    except (OSError, ValueError, http.client.HTTPException):
        reporter.fail("Google OAuth discovery", "request failed (details redacted)")
        return

    token_endpoint = discovery.payload.get("token_endpoint")
    jwks_uri = discovery.payload.get("jwks_uri")
    if (
        discovery.status != 200
        or token_endpoint != "https://oauth2.googleapis.com/token"
        or not isinstance(jwks_uri, str)
        or not jwks_uri.startswith("https://")
    ):
        reporter.fail("Google OAuth discovery", "unexpected provider metadata")
        return
    reporter.pass_(
        "Google OAuth configuration",
        "Web client, allowed redirect URI, and provider metadata are valid",
    )

    refresh_token = credentials["refresh_token"]
    if not refresh_token:
        reporter.incomplete_(
            "Google OAuth live grant",
            "set GOOGLE_OAUTH_REFRESH_TOKEN to prove the client secret and grant",
        )
        return

    parsed_endpoint = urlparse(token_endpoint)
    try:
        token_result = request_json(
            parsed_endpoint.netloc,
            parsed_endpoint.path,
            timeout=timeout,
            method="POST",
            form={
                "client_id": credentials["client_id"],
                "client_secret": credentials["client_secret"],
                "refresh_token": refresh_token,
                "grant_type": "refresh_token",
            },
        )
    except (OSError, ValueError, http.client.HTTPException):
        reporter.fail("Google OAuth live grant", "token refresh failed (details redacted)")
        return

    access_token = token_result.payload.get("access_token")
    if token_result.status != 200 or not isinstance(access_token, str) or not access_token:
        error_code = token_result.payload.get("error")
        safe_code = error_code if isinstance(error_code, str) else "unknown_error"
        reporter.fail(
            "Google OAuth live grant",
            f"token endpoint rejected the grant ({safe_code}; details redacted)",
        )
        return

    raw_scope = token_result.payload.get("scope")
    if not isinstance(raw_scope, str):
        reporter.fail("Google OAuth scopes", "refresh response omitted granted scopes")
        return
    granted_scopes = normalize_google_scopes(set(raw_scope.split()))
    if has_forbidden_google_scope(granted_scopes):
        reporter.fail("Google OAuth scopes", "a forbidden Gmail scope is granted")
        return
    missing_scopes = REQUIRED_GOOGLE_SCOPES - granted_scopes
    if missing_scopes:
        reporter.fail(
            "Google OAuth scopes",
            f"grant is missing {len(missing_scopes)} required scope(s)",
        )
        return
    reporter.pass_(
        "Google OAuth live grant", "refresh succeeded with the approved non-Gmail scopes"
    )

    read_checks = (
        ("Google identity read", "www.googleapis.com", "/oauth2/v3/userinfo"),
        ("Google Drive read", "www.googleapis.com", "/drive/v3/about?fields=kind"),
        (
            "Google Calendar read",
            "www.googleapis.com",
            "/calendar/v3/users/me/calendarList?maxResults=1&fields=kind",
        ),
    )
    for label, host, path in read_checks:
        try:
            check = request_json(
                host, path, timeout=timeout, bearer_token=access_token
            )
        except (OSError, ValueError, http.client.HTTPException):
            reporter.fail(label, "read-only API request failed (details redacted)")
            continue
        if check.status == 200:
            reporter.pass_(label, "read-only API request succeeded")
        else:
            reporter.fail(label, f"read-only API returned HTTP {check.status}")


def verify_hostinger(
    settings: Mapping[str, str], reporter: Reporter, timeout: float
) -> None:
    host = settings.get("HOSTINGER_SMTP_HOST", "smtp.hostinger.com").strip()
    port_text = settings.get("HOSTINGER_SMTP_PORT", "587").strip()
    security = settings.get("HOSTINGER_SMTP_SECURITY", "starttls").strip().lower()
    username = settings.get("HOSTINGER_SMTP_USERNAME", "").strip()
    password = settings.get("HOSTINGER_SMTP_PASSWORD", "")

    if not username or not password:
        reporter.incomplete_(
            "Hostinger SMTP credentials",
            "set HOSTINGER_SMTP_USERNAME and HOSTINGER_SMTP_PASSWORD",
        )
        return
    try:
        port = int(port_text)
    except ValueError:
        reporter.fail("Hostinger SMTP configuration", "port must be an integer")
        return
    if not 1 <= port <= 65535:
        reporter.fail("Hostinger SMTP configuration", "port is outside 1-65535")
        return
    if security not in {"starttls", "ssl"}:
        reporter.fail(
            "Hostinger SMTP configuration", "security must be starttls or ssl"
        )
        return
    if host != "smtp.hostinger.com":
        reporter.fail(
            "Hostinger SMTP configuration",
            "host must be smtp.hostinger.com to prevent credential forwarding",
        )
        return
    if (security, port) not in {("starttls", 587), ("ssl", 465)}:
        reporter.fail(
            "Hostinger SMTP configuration",
            "use STARTTLS/587 or SSL/465 as displayed by Hostinger",
        )
        return

    context = ssl.create_default_context()
    smtp: smtplib.SMTP | None = None
    phase = "connect"
    try:
        if security == "ssl":
            smtp = smtplib.SMTP_SSL(host, port, timeout=timeout, context=context)
        else:
            smtp = smtplib.SMTP(host, port, timeout=timeout)
            phase = "EHLO"
            code, _ = smtp.ehlo()
            if code != 250:
                raise RuntimeError("EHLO rejected")
            if not smtp.has_extn("starttls"):
                raise RuntimeError("STARTTLS not advertised")
            phase = "STARTTLS"
            smtp.starttls(context=context)

        phase = "post-TLS EHLO"
        code, _ = smtp.ehlo()
        if code != 250:
            raise RuntimeError("post-TLS EHLO rejected")
        if "auth" not in smtp.esmtp_features:
            raise RuntimeError("AUTH not advertised")

        phase = "AUTH"
        smtp.login(username, password)
        phase = "NOOP"
        code, _ = smtp.noop()
        if code // 100 != 2:
            raise RuntimeError("NOOP rejected")
        reporter.pass_(
            "Hostinger SMTP credentials",
            "TLS, AUTH, and NOOP succeeded; no message was submitted",
        )
    except SMTPAuthenticationError:
        reporter.fail(
            "Hostinger SMTP credentials", "authentication was rejected (details redacted)"
        )
    except SSLError:
        reporter.fail(
            "Hostinger SMTP credentials",
            f"TLS failed during {phase} (details redacted)",
        )
    except (OSError, socket.timeout, SMTPException, RuntimeError):
        reporter.fail(
            "Hostinger SMTP credentials",
            f"SMTP failed during {phase} (details redacted)",
        )
    finally:
        if smtp is not None:
            try:
                smtp.quit()
            except (OSError, SMTPException):
                smtp.close()


def parse_args(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description=(
            "Run non-mutating Phase 0 credential checks. No Telegram message, "
            "Google mutation, OAuth revocation, or email is sent."
        ),
        epilog=(
            "Default secret file: .credentials/phase0.env (ignored by git). "
            "Exit 0=all selected checks passed, 1=failed, 2=missing/incomplete."
        ),
    )
    parser.add_argument(
        "providers",
        nargs="*",
        choices=("telegram", "google", "hostinger"),
        help="providers to check (default: all three)",
    )
    parser.add_argument(
        "--env-file",
        type=Path,
        default=DEFAULT_ENV_FILE,
        help="KEY=value credential file (default: .credentials/phase0.env)",
    )
    parser.add_argument(
        "--timeout",
        type=float,
        default=10.0,
        help="network timeout in seconds (default: 10)",
    )
    args = parser.parse_args(argv)
    if not 1 <= args.timeout <= 60:
        parser.error("--timeout must be between 1 and 60 seconds")
    if not args.providers:
        args.providers = ["telegram", "google", "hostinger"]
    args.providers = list(dict.fromkeys(args.providers))
    return args


def main(argv: list[str] | None = None) -> int:
    args = parse_args(argv if argv is not None else sys.argv[1:])
    reporter = Reporter()
    try:
        settings = load_settings(args.env_file)
    except (OSError, UnicodeError, ValueError) as error:
        reporter.fail("Credential file", str(error))
        return reporter.exit_code()

    for provider in args.providers:
        if provider == "telegram":
            verify_telegram(settings, reporter, args.timeout)
        elif provider == "google":
            verify_google(settings, reporter, args.timeout)
        else:
            verify_hostinger(settings, reporter, args.timeout)
    return reporter.exit_code()


if __name__ == "__main__":
    raise SystemExit(main())
