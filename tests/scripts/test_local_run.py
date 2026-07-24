from __future__ import annotations

import json
import os
import subprocess
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
RUNNER = ROOT / "scripts" / "local-run.sh"


class LocalRunHarnessTests(unittest.TestCase):
    def run_harness(
        self, env_text: str, *, inherited: dict[str, str] | None = None
    ) -> subprocess.CompletedProcess[str]:
        with tempfile.TemporaryDirectory() as directory:
            env_file = Path(directory) / ".env"
            env_file.write_text(env_text, encoding="utf-8")
            env_file.chmod(0o600)
            environment = os.environ.copy()
            environment.update(inherited or {})
            environment["NOVUS_ENV_FILE"] = str(env_file)
            return subprocess.run(
                [
                    str(RUNNER),
                    "run",
                    "--",
                    "python3",
                    "-c",
                    (
                        "import json, os; print(json.dumps({key: os.environ.get(key) "
                        "for key in ('NOVUS_ENVIRONMENT', 'ENVIRONMENT', "
                        "'NOVUS_AI_PROVIDER_KEY', 'TELEGRAM_SECRET_TOKEN', "
                        "'OAUTH_CLIENT_ID', 'OAUTH_REDIRECT_URI')}))"
                    ),
                ],
                cwd=ROOT,
                env=environment,
                capture_output=True,
                text=True,
                check=False,
            )

    def test_loads_dotenv_and_maps_runtime_aliases(self) -> None:
        result = self.run_harness(
            """
NOVUS_ENVIRONMENT=local
GEMINI_API_KEY=gemini-test-key
TELEGRAM_WEBHOOK_SECRET=telegram-webhook-secret
GOOGLE_OAUTH_CLIENT_ID=client-id
GOOGLE_OAUTH_REDIRECT_URI=http://localhost:3000/oauth/google/callback
"""
        )

        self.assertEqual(result.returncode, 0, result.stderr)
        values = json.loads(result.stdout)
        self.assertEqual(values["ENVIRONMENT"], "local")
        self.assertEqual(values["NOVUS_AI_PROVIDER_KEY"], "gemini-test-key")
        self.assertEqual(values["TELEGRAM_SECRET_TOKEN"], "telegram-webhook-secret")
        self.assertEqual(values["OAUTH_CLIENT_ID"], "client-id")
        self.assertEqual(
            values["OAUTH_REDIRECT_URI"],
            "http://localhost:3000/oauth/google/callback",
        )

    def test_process_environment_overrides_dotenv(self) -> None:
        result = self.run_harness(
            "GEMINI_API_KEY=file-key\nNOVUS_ENVIRONMENT=local\n",
            inherited={"GEMINI_API_KEY": "process-key"},
        )

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(
            json.loads(result.stdout)["NOVUS_AI_PROVIDER_KEY"], "process-key"
        )

    def test_dotenv_values_are_not_evaluated_by_the_shell(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            marker = Path(directory) / "executed"
            result = self.run_harness(
                f"GEMINI_API_KEY=$(touch {marker})\nNOVUS_ENVIRONMENT=local\n"
            )

            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertFalse(marker.exists())
            self.assertEqual(
                json.loads(result.stdout)["NOVUS_AI_PROVIDER_KEY"],
                f"$(touch {marker})",
            )

    def test_telegram_webhook_rejects_non_https_url_without_network_call(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            env_file = Path(directory) / ".env"
            env_file.write_text(
                "TELEGRAM_BOT_TOKEN=123:test-token\n"
                "TELEGRAM_WEBHOOK_SECRET=valid_secret\n",
                encoding="utf-8",
            )
            env_file.chmod(0o600)
            environment = os.environ.copy()
            environment["NOVUS_ENV_FILE"] = str(env_file)

            result = subprocess.run(
                [str(RUNNER), "telegram-set-webhook", "http://localhost:3000"],
                cwd=ROOT,
                env=environment,
                capture_output=True,
                text=True,
                check=False,
            )

        self.assertNotEqual(result.returncode, 0)
        self.assertIn("HTTPS", result.stderr)


if __name__ == "__main__":
    unittest.main()
