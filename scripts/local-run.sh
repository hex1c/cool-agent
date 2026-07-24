#!/usr/bin/env bash
set -euo pipefail

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
ENV_FILE=${NOVUS_ENV_FILE:-"$ROOT/.env"}
COMPOSE_FILE="$ROOT/infrastructure/local/docker-compose.yaml"
SAM_TEMPLATE="$ROOT/.aws-sam/build/template.yaml"

fail() {
	printf 'local-run: %s\n' "$*" >&2
	exit 1
}

require_command() {
	command -v "$1" >/dev/null 2>&1 || fail "required command not found: $1"
}

compose() {
	if docker compose version >/dev/null 2>&1; then
		docker compose "$@"
	elif command -v docker-compose >/dev/null 2>&1; then
		docker-compose "$@"
	else
		docker run --rm \
			-v /var/run/docker.sock:/var/run/docker.sock \
			-v "$ROOT:$ROOT" -w "$ROOT" \
			docker/compose:1.29.2 "$@"
	fi
}

load_dotenv() {
	[[ -f "$ENV_FILE" ]] || fail "environment file not found: $ENV_FILE (copy .env.example to .env)"
	require_command python3

	local parsed
	parsed=$(mktemp "${TMPDIR:-/tmp}/novus-env.XXXXXX")
	chmod 600 "$parsed"
	trap 'rm -f "$parsed"' RETURN

	python3 - "$ENV_FILE" "$parsed" <<'PY'
import os
import re
import stat
import sys
from pathlib import Path

env_path = Path(sys.argv[1])
out_path = Path(sys.argv[2])
mode = stat.S_IMODE(env_path.stat().st_mode)
if mode & 0o077:
    print(
        f"local-run: warning: {env_path} has mode {mode:04o}; use chmod 600 {env_path}",
        file=sys.stderr,
    )

pairs: list[tuple[str, str]] = []
for line_number, raw in enumerate(env_path.read_text(encoding="utf-8").splitlines(), 1):
    line = raw.strip()
    if not line or line.startswith("#"):
        continue
    if line.startswith("export "):
        line = line[7:].lstrip()
    if "=" not in line:
        raise SystemExit(f"local-run: {env_path}:{line_number}: expected KEY=value")
    key, value = line.split("=", 1)
    key = key.strip()
    value = value.strip()
    if not re.fullmatch(r"[A-Za-z_][A-Za-z0-9_]*", key):
        raise SystemExit(f"local-run: {env_path}:{line_number}: invalid variable name")
    if len(value) >= 2 and value[0] == value[-1] and value[0] in {"'", '"'}:
        value = value[1:-1]
    if key not in os.environ:
        pairs.append((key, value))

with out_path.open("wb") as output:
    for key, value in pairs:
        output.write(key.encode() + b"\0" + value.encode() + b"\0")
PY

	while IFS= read -r -d '' key && IFS= read -r -d '' value; do
		export "$key=$value"
	done <"$parsed"
	rm -f "$parsed"
	trap - RETURN
}

map_alias() {
	local target=$1 source=$2
	if [[ -z ${!target:-} && -n ${!source:-} ]]; then
		export "$target=${!source}"
	fi
}

configure_runtime_environment() {
	export NOVUS_ENVIRONMENT=${NOVUS_ENVIRONMENT:-local}
	export ENVIRONMENT=${ENVIRONMENT:-$NOVUS_ENVIRONMENT}
	export AWS_ACCESS_KEY_ID=${AWS_ACCESS_KEY_ID:-test}
	export AWS_SECRET_ACCESS_KEY=${AWS_SECRET_ACCESS_KEY:-test}
	export AWS_REGION=${AWS_REGION:-us-east-1}
	export AWS_DEFAULT_REGION=${AWS_DEFAULT_REGION:-$AWS_REGION}
	export APPLICATION_TABLE=${APPLICATION_TABLE:-novus-development-application}
	export DYNAMODB_ENDPOINT=${DYNAMODB_ENDPOINT:-http://host.docker.internal:8000}
	export STEPFUNCTIONS_ENDPOINT=${STEPFUNCTIONS_ENDPOINT:-http://host.docker.internal:8083}
	export LOCALSTACK_ENDPOINT=${LOCALSTACK_ENDPOINT:-http://host.docker.internal:4566}
	export QUOTATION_STATE_MACHINE_ARN=${QUOTATION_STATE_MACHINE_ARN:-arn:aws:states:us-east-1:123456789012:stateMachine:novus-local-quotation}
	export CALENDAR_STATE_MACHINE_ARN=${CALENDAR_STATE_MACHINE_ARN:-arn:aws:states:us-east-1:123456789012:stateMachine:novus-local-calendar}
	export EMAIL_STATE_MACHINE_ARN=${EMAIL_STATE_MACHINE_ARN:-arn:aws:states:us-east-1:123456789012:stateMachine:novus-local-email}
	if [[ -z ${PAGE_TOKEN_SIGNING_KEY_HEX:-} ]]; then
		PAGE_TOKEN_SIGNING_KEY_HEX=$(python3 -c 'import secrets; print(secrets.token_hex(32))')
		export PAGE_TOKEN_SIGNING_KEY_HEX
	fi

	map_alias NOVUS_AI_PROVIDER_KEY GEMINI_API_KEY
	map_alias TELEGRAM_SECRET_TOKEN TELEGRAM_WEBHOOK_SECRET
	map_alias OAUTH_CLIENT_ID GOOGLE_OAUTH_CLIENT_ID
	map_alias OAUTH_CLIENT_SECRET GOOGLE_OAUTH_CLIENT_SECRET
	map_alias OAUTH_REDIRECT_URI GOOGLE_OAUTH_REDIRECT_URI
}

require_value() {
	[[ -n ${!1:-} ]] || fail "required variable is empty: $1"
}

write_sam_env() {
	local destination=$1
	python3 - "$destination" <<'PY'
import json
import os
import sys
from pathlib import Path

keys = {
    "WebhookFunction": (
        "ENVIRONMENT", "TELEGRAM_SECRET_TOKEN", "APPLICATION_TABLE",
        "DYNAMODB_ENDPOINT", "STEPFUNCTIONS_ENDPOINT", "PAGE_TOKEN_SIGNING_KEY_HEX",
        "QUOTATION_STATE_MACHINE_ARN", "CALENDAR_STATE_MACHINE_ARN",
        "EMAIL_STATE_MACHINE_ARN", "AWS_ACCESS_KEY_ID", "AWS_SECRET_ACCESS_KEY",
        "AWS_REGION", "AWS_DEFAULT_REGION",
    ),
    "OAuthFunction": (
        "ENVIRONMENT", "OAUTH_CLIENT_ID", "OAUTH_CLIENT_SECRET",
        "OAUTH_REDIRECT_URI", "LOCALSTACK_ENDPOINT", "DYNAMODB_ENDPOINT",
        "APPLICATION_TABLE", "AWS_ACCESS_KEY_ID",
        "AWS_SECRET_ACCESS_KEY", "AWS_REGION", "AWS_DEFAULT_REGION",
    ),
    "AgentHarnessFunction": ("ENVIRONMENT", "NOVUS_AI_PROVIDER_KEY"),
    "EmailActionsFunction": (
        "ENVIRONMENT", "HOSTINGER_SMTP_HOST", "HOSTINGER_SMTP_PORT",
        "HOSTINGER_SMTP_SECURITY", "HOSTINGER_SMTP_USERNAME",
        "HOSTINGER_SMTP_PASSWORD", "AWS_ACCESS_KEY_ID",
        "AWS_SECRET_ACCESS_KEY", "AWS_REGION", "AWS_DEFAULT_REGION",
    ),
}
payload = {
    function: {key: os.environ[key] for key in names if os.environ.get(key)}
    for function, names in keys.items()
}
Path(sys.argv[1]).write_text(json.dumps(payload), encoding="utf-8")
PY
	chmod 600 "$destination"
}

telegram_webhook_request() {
	local action=$1
	local public_url=${2:-}
	require_value TELEGRAM_BOT_TOKEN

	TELEGRAM_WEBHOOK_ACTION=$action TELEGRAM_PUBLIC_URL=$public_url python3 <<'PY'
import json
import os
import re
import urllib.error
import urllib.parse
import urllib.request


action = os.environ["TELEGRAM_WEBHOOK_ACTION"]
token = os.environ["TELEGRAM_BOT_TOKEN"]
secret = os.environ.get("TELEGRAM_WEBHOOK_SECRET", "")

if action == "setWebhook":
    if not re.fullmatch(r"[A-Za-z0-9_-]{1,256}", secret):
        raise SystemExit(
            "local-run: TELEGRAM_WEBHOOK_SECRET must contain 1-256 letters, digits, _ or -"
        )
    parsed = urllib.parse.urlparse(os.environ["TELEGRAM_PUBLIC_URL"])
    if parsed.scheme != "https" or not parsed.netloc or parsed.username or parsed.password:
        raise SystemExit("local-run: Telegram webhook requires a public HTTPS URL")
    if parsed.query or parsed.fragment:
        raise SystemExit("local-run: Telegram webhook base URL cannot contain query or fragment")
    webhook_url = os.environ["TELEGRAM_PUBLIC_URL"].rstrip("/") + "/webhook"
    form = {"url": webhook_url, "secret_token": secret}
else:
    form = {"drop_pending_updates": "false"}

request = urllib.request.Request(
    f"https://api.telegram.org/bot{token}/{action}",
    data=urllib.parse.urlencode(form).encode(),
    headers={"Content-Type": "application/x-www-form-urlencoded"},
)
try:
    with urllib.request.urlopen(request, timeout=15) as response:
        payload = json.load(response)
except (OSError, urllib.error.URLError, json.JSONDecodeError) as error:
    raise SystemExit("local-run: Telegram Bot API request failed (details redacted)") from error
if not payload.get("ok"):
    raise SystemExit("local-run: Telegram rejected the webhook request")
print("Telegram webhook configured" if action == "setWebhook" else "Telegram webhook removed")
PY
}

telegram_dev() {
	require_command cloudflared
	require_command docker
	require_command sam
	require_value TELEGRAM_BOT_TOKEN
	require_value TELEGRAM_WEBHOOK_SECRET
	require_value TELEGRAM_SECRET_TOKEN
	[[ -f "$SAM_TEMPLATE" ]] || fail "SAM build missing; run scripts/local-run.sh sam-build first"

	compose -f "$COMPOSE_FILE" up -d
	local tunnel_log sam_env
	tunnel_log=$(mktemp "${TMPDIR:-/tmp}/novus-tunnel.XXXXXX")
	sam_env=$(mktemp "${TMPDIR:-/tmp}/novus-sam-env.XXXXXX.json")
	chmod 600 "$tunnel_log" "$sam_env"
	write_sam_env "$sam_env"

	sam local start-api --template "$SAM_TEMPLATE" --env-vars "$sam_env" --port 3000 &
	local api_pid=$!
	sam local start-lambda --template "$SAM_TEMPLATE" --env-vars "$sam_env" --port 3001 &
	local lambda_pid=$!

	cleanup_telegram_dev() {
		telegram_webhook_request deleteWebhook 2>/dev/null || true
		kill "${tunnel_pid:-}" "$lambda_pid" "$api_pid" 2>/dev/null || true
		rm -f "$tunnel_log" "$sam_env"
	}
	trap cleanup_telegram_dev EXIT INT TERM

	python3 - <<'PY'
import socket
import time
for _ in range(60):
    try:
        with socket.create_connection(("127.0.0.1", 3000), timeout=1):
            break
    except OSError:
        time.sleep(1)
else:
    raise SystemExit("local-run: SAM API did not become ready on port 3000")
PY

	cloudflared tunnel --url http://127.0.0.1:3000 --no-autoupdate >"$tunnel_log" 2>&1 &
	local tunnel_pid=$!
	printf 'Waiting for Cloudflare Quick Tunnel...\n'
	local url=""
	local waited=0
	while [[ -z "$url" && $waited -lt 60 ]]; do
		sleep 1
		waited=$((waited + 1))
		url=$(grep -m1 -oE 'https://[a-z0-9-]+\.trycloudflare\.com' "$tunnel_log" 2>/dev/null || true)
		if ! kill -0 "$tunnel_pid" 2>/dev/null; then
			cat "$tunnel_log" >&2
			fail "cloudflared exited before producing a tunnel URL"
		fi
	done
	[[ -n "$url" ]] || {
		cat "$tunnel_log" >&2
		fail "tunnel URL not found within 60s"
	}

	printf 'Tunnel: %s\n' "$url"
	telegram_webhook_request setWebhook "$url"
	printf '\nWebhook registered. Send a message to your bot.\nPress Ctrl-C to stop and remove the webhook.\n\n'
	wait -n "$tunnel_pid" "$lambda_pid" "$api_pid"
}

show_config() {
	local names=(
		NOVUS_ENVIRONMENT GEMINI_API_KEY NOVUS_AI_PROVIDER_KEY
		TELEGRAM_BOT_TOKEN TELEGRAM_FORUM_CHAT_ID TELEGRAM_WEBHOOK_SECRET
		GOOGLE_OAUTH_CLIENT_ID GOOGLE_OAUTH_CLIENT_SECRET GOOGLE_OAUTH_REDIRECT_URI
		GOOGLE_OAUTH_REFRESH_TOKEN HOSTINGER_SMTP_HOST HOSTINGER_SMTP_PORT
		HOSTINGER_SMTP_USERNAME HOSTINGER_SMTP_PASSWORD
	)
	local name
	printf 'Environment file: %s\n' "$ENV_FILE"
	for name in "${names[@]}"; do
		if [[ -n ${!name:-} ]]; then
			printf '%-34s configured\n' "$name"
		else
			printf '%-34s missing\n' "$name"
		fi
	done
}

usage() {
	cat <<'EOF'
Usage: scripts/local-run.sh COMMAND [ARGS]

Commands:
  check                         Show which local settings are configured (values redacted)
  run -- COMMAND [ARGS]         Run any command with the root .env loaded
  preflight [provider ...]      Run read-only Telegram/Google/Hostinger checks
  sandbox-up                    Start and seed the credential-free Docker sandbox
  sandbox-test                  Run the local sandbox integration test
  sandbox-down                  Stop the sandbox and remove its volumes
  sam-build                     Build the SAM application
  sam-api [SAM ARGS]            Start local webhook/OAuth API with mapped environment
  sam-agent EVENT.json          Invoke the agent harness locally with the configured AI key
  telegram-tunnel               Expose local SAM port 3000 through Cloudflare Quick Tunnel
  telegram-set-webhook URL      Register URL/webhook with Telegram and the configured secret
  telegram-delete-webhook       Remove the configured Telegram webhook
  telegram-dev                  Start SAM API/Lambda, tunnel, and auto-register webhook

NOVUS_ENV_FILE may point to a file other than PROJECT_ROOT/.env.
Existing process variables override values in the dotenv file.
EOF
}

[[ $# -gt 0 ]] || {
	usage
	exit 2
}
command_name=$1
shift

case "$command_name" in
check | run | preflight | sam-api | sam-agent | telegram-set-webhook | telegram-delete-webhook | telegram-dev)
	load_dotenv
	configure_runtime_environment
	;;
-h | --help | help)
	usage
	exit 0
	;;
esac
cd "$ROOT"

case "$command_name" in
check)
	[[ $# -eq 0 ]] || fail "check accepts no arguments"
	show_config
	;;
run)
	[[ ${1:-} == -- ]] && shift
	[[ $# -gt 0 ]] || fail "run requires a command"
	exec "$@"
	;;
preflight)
	exec python3 scripts/verify-phase0-credentials.py "$@"
	;;
sandbox-up)
	require_command docker
	compose -f "$COMPOSE_FILE" up -d
	;;
sandbox-test)
	exec cargo test -p integration-tests --test local_sandbox --features integration
	;;
sandbox-down)
	require_command docker
	compose -f "$COMPOSE_FILE" down -v
	;;
sam-build)
	require_command sam
	exec sam build --template-file infrastructure/template.yaml
	;;
sam-api)
	require_command sam
	[[ -f "$SAM_TEMPLATE" ]] || fail "SAM build missing; run scripts/local-run.sh sam-build first"
	require_value TELEGRAM_SECRET_TOKEN
	require_value OAUTH_CLIENT_ID
	require_value OAUTH_REDIRECT_URI
	sam_env=$(mktemp "${TMPDIR:-/tmp}/novus-sam-env.XXXXXX.json")
	trap 'rm -f "$sam_env"' EXIT INT TERM
	write_sam_env "$sam_env"
	sam local start-api --template "$SAM_TEMPLATE" --env-vars "$sam_env" "$@"
	;;
sam-agent)
	require_command sam
	[[ $# -eq 1 ]] || fail "sam-agent requires one event JSON path"
	[[ -f "$SAM_TEMPLATE" ]] || fail "SAM build missing; run scripts/local-run.sh sam-build first"
	[[ -f "$1" ]] || fail "event file not found: $1"
	require_value NOVUS_AI_PROVIDER_KEY
	sam_env=$(mktemp "${TMPDIR:-/tmp}/novus-sam-env.XXXXXX.json")
	trap 'rm -f "$sam_env"' EXIT INT TERM
	write_sam_env "$sam_env"
	sam local invoke AgentHarnessFunction \
		--template "$SAM_TEMPLATE" --env-vars "$sam_env" --event "$1"
	;;
telegram-tunnel)
	[[ $# -eq 0 ]] || fail "telegram-tunnel accepts no arguments"
	require_command cloudflared
	exec cloudflared tunnel --url http://127.0.0.1:3000 --no-autoupdate
	;;
telegram-set-webhook)
	[[ $# -eq 1 ]] || fail "telegram-set-webhook requires the Cloudflare HTTPS base URL"
	telegram_webhook_request setWebhook "$1"
	;;
telegram-delete-webhook)
	[[ $# -eq 0 ]] || fail "telegram-delete-webhook accepts no arguments"
	telegram_webhook_request deleteWebhook
	;;
telegram-dev)
	[[ $# -eq 0 ]] || fail "telegram-dev accepts no arguments"
	[[ -f "$SAM_TEMPLATE" ]] || fail "SAM build missing; run scripts/local-run.sh sam-build first"
	telegram_dev
	;;
*)
	usage >&2
	fail "unknown command: $command_name"
	;;
esac
