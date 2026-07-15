from __future__ import annotations

import importlib.util
import json
import os
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

SCRIPT_PATH = (
    Path(__file__).resolve().parents[2] / "scripts" / "verify-phase0-credentials.py"
)
SPEC = importlib.util.spec_from_file_location("verify_phase0_credentials", SCRIPT_PATH)
if SPEC is None or SPEC.loader is None:
    raise RuntimeError("unable to load credential verifier")
verifier = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = verifier
SPEC.loader.exec_module(verifier)


class CredentialVerifierTests(unittest.TestCase):
    def test_env_file_preserves_password_punctuation(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "phase0.env"
            path.write_text(
                "export HOSTINGER_SMTP_PASSWORD='part=one#part-two'\n",
                encoding="utf-8",
            )

            values = verifier.load_env_file(path)

        self.assertEqual(values["HOSTINGER_SMTP_PASSWORD"], "part=one#part-two")

    def test_process_environment_overrides_env_file(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "phase0.env"
            path.write_text("TELEGRAM_BOT_TOKEN=file-value\n", encoding="utf-8")
            with mock.patch.dict(
                os.environ, {"TELEGRAM_BOT_TOKEN": "environment-value"}, clear=False
            ):
                values = verifier.load_settings(path)

        self.assertEqual(values["TELEGRAM_BOT_TOKEN"], "environment-value")

    def test_google_web_client_file_is_loaded_without_exposing_values(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "google.json"
            path.write_text(
                json.dumps(
                    {
                        "web": {
                            "client_id": "client-id",
                            "client_secret": "client-secret",
                            "redirect_uris": ["https://example.test/oauth/callback"],
                        }
                    }
                ),
                encoding="utf-8",
            )

            credentials = verifier.load_google_credentials(
                {"GOOGLE_OAUTH_CREDENTIALS_FILE": str(path)}
            )

        self.assertEqual(credentials["redirect_uri"], "https://example.test/oauth/callback")
        self.assertEqual(credentials["refresh_token"], "")

    def test_google_http_localhost_redirect_is_allowed_for_development(self) -> None:
        self.assertTrue(
            verifier.is_allowed_google_redirect_uri(
                "http://localhost:3000/auth/google/callback"
            )
        )
        self.assertTrue(
            verifier.is_allowed_google_redirect_uri(
                "http://127.0.0.1:3000/auth/google/callback"
            )
        )
        self.assertFalse(
            verifier.is_allowed_google_redirect_uri(
                "http://example.test/auth/google/callback"
            )
        )

    def test_google_installed_client_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "google.json"
            path.write_text(
                json.dumps(
                    {
                        "installed": {
                            "client_id": "client-id",
                            "client_secret": "client-secret",
                            "redirect_uris": ["http://localhost"],
                        }
                    }
                ),
                encoding="utf-8",
            )

            with self.assertRaisesRegex(ValueError, "Web client"):
                verifier.load_google_credentials(
                    {"GOOGLE_OAUTH_CREDENTIALS_FILE": str(path)}
                )

    def test_google_userinfo_scope_aliases_are_normalized(self) -> None:
        scopes = verifier.normalize_google_scopes(
            {"openid", "https://www.googleapis.com/auth/userinfo.email"}
        )

        self.assertEqual(scopes, {"openid", "email"})

    def test_gmail_scopes_are_rejected(self) -> None:
        self.assertTrue(
            verifier.has_forbidden_google_scope(
                {"https://www.googleapis.com/auth/gmail.send"}
            )
        )
        self.assertFalse(
            verifier.has_forbidden_google_scope(verifier.REQUIRED_GOOGLE_SCOPES)
        )


if __name__ == "__main__":
    unittest.main()
