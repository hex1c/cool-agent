#!/bin/sh
# Seed the Task 40 local sandbox with sanitized, deterministic resources.
# Runs automatically in the sandbox-init container and can also be invoked
# manually when AWS CLI v2 is available on the host.
set -eu

export AWS_ACCESS_KEY_ID="${AWS_ACCESS_KEY_ID:-test}"
export AWS_SECRET_ACCESS_KEY="${AWS_SECRET_ACCESS_KEY:-test}"
export AWS_DEFAULT_REGION="${AWS_DEFAULT_REGION:-us-east-1}"
export AWS_EC2_METADATA_DISABLED=true

DYNAMODB_ENDPOINT="${DYNAMODB_ENDPOINT:-http://127.0.0.1:8000}"
LOCALSTACK_ENDPOINT="${LOCALSTACK_ENDPOINT:-http://127.0.0.1:4566}"
STEPFUNCTIONS_ENDPOINT="${STEPFUNCTIONS_ENDPOINT:-http://127.0.0.1:8083}"
APPLICATION_TABLE="${APPLICATION_TABLE:-novus-development-application}"
BUDGET_TABLE="${BUDGET_TABLE:-novus-development-budget}"
ARTIFACT_BUCKET="${ARTIFACT_BUCKET:-novus-development-artifacts-local}"
INVOICE_MONTH="${INVOICE_MONTH:-$(date -u +%Y-%m)}"
RECONCILIATION_OBSERVED_AT="${RECONCILIATION_OBSERVED_AT:-$(date -u +%s)}"

wait_for_localstack() {
  attempts=0
  until aws --endpoint-url "$LOCALSTACK_ENDPOINT" s3api list-buckets >/dev/null 2>&1; do
    attempts=$((attempts + 1))
    if [ "$attempts" -ge 60 ]; then
      echo "timed out waiting for LocalStack at $LOCALSTACK_ENDPOINT" >&2
      return 1
    fi
    sleep 1
  done
}

wait_for_stepfunctions() {
  attempts=0
  until aws --endpoint-url "$STEPFUNCTIONS_ENDPOINT" stepfunctions list-state-machines >/dev/null 2>&1; do
    attempts=$((attempts + 1))
    if [ "$attempts" -ge 60 ]; then
      echo "timed out waiting for Step Functions Local at $STEPFUNCTIONS_ENDPOINT" >&2
      return 1
    fi
    sleep 1
  done
}

wait_for_dynamodb() {
  attempts=0
  until aws --endpoint-url "$DYNAMODB_ENDPOINT" dynamodb list-tables >/dev/null 2>&1; do
    attempts=$((attempts + 1))
    if [ "$attempts" -ge 60 ]; then
      echo "timed out waiting for DynamoDB Local at $DYNAMODB_ENDPOINT" >&2
      return 1
    fi
    sleep 1
  done
}

create_table() {
  table="$1"
  if aws --endpoint-url "$DYNAMODB_ENDPOINT" dynamodb describe-table \
    --table-name "$table" >/dev/null 2>&1; then
    return
  fi
  if ! aws --endpoint-url "$DYNAMODB_ENDPOINT" dynamodb create-table \
    --table-name "$table" \
    --billing-mode PAY_PER_REQUEST \
    --attribute-definitions AttributeName=pk,AttributeType=S AttributeName=sk,AttributeType=S \
    --key-schema AttributeName=pk,KeyType=HASH AttributeName=sk,KeyType=RANGE \
    >/dev/null 2>&1; then
    # Another initializer may have won the create race.
    aws --endpoint-url "$DYNAMODB_ENDPOINT" dynamodb describe-table \
      --table-name "$table" >/dev/null
  fi
}

wait_for_dynamodb
wait_for_localstack
wait_for_stepfunctions

create_table "$APPLICATION_TABLE"
create_table "$BUDGET_TABLE"

aws --endpoint-url "$DYNAMODB_ENDPOINT" dynamodb put-item \
  --table-name "$APPLICATION_TABLE" \
  --item '{"pk":{"S":"SANDBOX#SEED"},"sk":{"S":"METADATA"},"schemaVersion":{"S":"novus.local.seed.v1"},"sanitized":{"BOOL":true}}' \
  >/dev/null

cat > /tmp/budget-aggregate.json <<EOF
{"pk":{"S":"MONTH#$INVOICE_MONTH"},"sk":{"S":"AGGREGATE"},"entity":{"S":"budget_aggregate"},"invoice_month":{"S":"$INVOICE_MONTH"},"settled_micro_inr":{"N":"0"},"reserved_micro_inr":{"N":"0"},"reconciled_micro_inr":{"N":"0"},"reconciliation_observed_at":{"N":"$RECONCILIATION_OBSERVED_AT"},"attribution_complete":{"BOOL":true},"pricing_version":{"S":"task41-2026-07-23"},"pricing_approval_id":{"S":"task41-2026-07-23"},"pricing_approved_at":{"N":"1784764800"},"optimistic_version":{"N":"0"}}
EOF
if ! aws --endpoint-url "$DYNAMODB_ENDPOINT" dynamodb put-item \
  --table-name "$BUDGET_TABLE" \
  --item file:///tmp/budget-aggregate.json \
  --condition-expression 'attribute_not_exists(pk)' >/dev/null 2>&1; then
  # Preserve existing counters on repeated initialization.
  aws --endpoint-url "$DYNAMODB_ENDPOINT" dynamodb get-item \
    --table-name "$BUDGET_TABLE" \
    --key "{\"pk\":{\"S\":\"MONTH#$INVOICE_MONTH\"},\"sk\":{\"S\":\"AGGREGATE\"}}" \
    --consistent-read \
    --query 'Item.entity.S' \
    --output text | grep -qx budget_aggregate
fi

if ! aws --endpoint-url "$LOCALSTACK_ENDPOINT" s3api head-bucket \
  --bucket "$ARTIFACT_BUCKET" >/dev/null 2>&1; then
  if ! aws --endpoint-url "$LOCALSTACK_ENDPOINT" s3api create-bucket \
    --bucket "$ARTIFACT_BUCKET" >/dev/null 2>&1; then
    # Another initializer may have won the create race.
    aws --endpoint-url "$LOCALSTACK_ENDPOINT" s3api head-bucket \
      --bucket "$ARTIFACT_BUCKET" >/dev/null
  fi
fi

put_secret() {
  name="$1"
  value="$2"
  aws --endpoint-url "$LOCALSTACK_ENDPOINT" ssm put-parameter \
    --name "$name" \
    --type SecureString \
    --value "$value" \
    --overwrite >/dev/null
}

put_secret /novus/development/telegram/bot-token local-test-token
put_secret /novus/development/telegram/webhook-secret local-webhook-secret-not-real
put_secret /novus/development/google/client-secret local-google-secret-not-real
put_secret /novus/development/smtp/credentials local-smtp-credentials-not-real
put_secret /novus/development/ai/provider-key local-ai-key-not-real

state_machine_name=novus-local-wait-resume
state_machine_definition='{"Comment":"Sanitized local wait/resume proof","StartAt":"Wait","States":{"Wait":{"Type":"Wait","Seconds":1,"Next":"Done"},"Done":{"Type":"Succeed"}}}'
state_machine_arn=$(aws --endpoint-url "$STEPFUNCTIONS_ENDPOINT" stepfunctions list-state-machines \
  --query "stateMachines[?name=='$state_machine_name'].stateMachineArn | [0]" \
  --output text)
if [ "$state_machine_arn" = "None" ] || [ -z "$state_machine_arn" ]; then
  if ! state_machine_arn=$(aws --endpoint-url "$STEPFUNCTIONS_ENDPOINT" stepfunctions create-state-machine \
    --name "$state_machine_name" \
    --definition "$state_machine_definition" \
    --role-arn arn:aws:iam::123456789012:role/NovusLocalDummyRole \
    --query stateMachineArn \
    --output text); then
    # Another initializer may have won the create race.
    state_machine_arn=$(aws --endpoint-url "$STEPFUNCTIONS_ENDPOINT" stepfunctions list-state-machines \
      --query "stateMachines[?name=='$state_machine_name'].stateMachineArn | [0]" \
      --output text)
  fi
else
  aws --endpoint-url "$STEPFUNCTIONS_ENDPOINT" stepfunctions update-state-machine \
    --state-machine-arn "$state_machine_arn" \
    --definition "$state_machine_definition" >/dev/null
fi
execution_arn=$(aws --endpoint-url "$STEPFUNCTIONS_ENDPOINT" stepfunctions start-execution \
  --state-machine-arn "$state_machine_arn" \
  --input '{"schemaVersion":"novus.local.wait.v1","sanitized":true}' \
  --query executionArn \
  --output text)
attempts=0
while :; do
  execution_status=$(aws --endpoint-url "$STEPFUNCTIONS_ENDPOINT" stepfunctions describe-execution \
    --execution-arn "$execution_arn" --query status --output text)
  [ "$execution_status" = "SUCCEEDED" ] && break
  if [ "$execution_status" != "RUNNING" ]; then
    echo "local wait/resume execution failed with status $execution_status" >&2
    exit 1
  fi
  attempts=$((attempts + 1))
  if [ "$attempts" -ge 30 ]; then
    echo "local wait/resume execution did not complete" >&2
    exit 1
  fi
  sleep 1
done

printf '%s\n' '{"schemaVersion":"novus.history.v1","sanitized":true,"messages":[{"role":"user","content":"Create a sanitized sample quotation"}]}' \
  > /tmp/seed-history.json
aws --endpoint-url "$LOCALSTACK_ENDPOINT" s3api put-object \
  --bucket "$ARTIFACT_BUCKET" \
  --key history/sandbox-workflow/00000000000000000001.json \
  --content-type application/json \
  --body /tmp/seed-history.json >/dev/null

echo "Novus local sandbox seeded with sanitized data."
