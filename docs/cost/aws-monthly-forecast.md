<!-- markdownlint-disable MD013 -->

# AWS Monthly Cost Forecast

**Status:** Phase 0 forecast revised with project-owner decisions on 2026-07-15; dated AWS Pricing Calculator evidence and shared-budget ADR approval pending
**Task:** Phase 0, Task 8
**Budget:** ₹300 per month across development, staging, and production, **excluding GST**

## Conclusion

The original Secrets Manager design is rejected: its fixed monthly storage cost
alone exceeds the service-cost budget. The selected replacement is **AWS Systems
Manager Parameter Store standard-tier `SecureString` parameters encrypted with
the AWS managed Systems Manager KMS key**. Standard parameters have no
additional Parameter Store charge.

Keep credentials separate by environment and capability rather than combining
unrelated secrets. The planned maximum is 21 parameters: 12 provider credentials
(four per environment) plus up to nine refresh-token parameters (three employees
in each of three isolated environments). This preserves least privilege while
removing the previous USD 8.40 monthly secret-storage charge.

Application-only encryption is not an acceptable replacement: it would still
need a root encryption key, and putting that key in application code, plain
configuration, or a Lambda environment variable only moves the same compromise
risk. Application-level envelope encryption is permitted only if its key
hierarchy remains in KMS/Parameter Store and receives a separate security
review. It is not required for the selected baseline.

The principal remaining risk is now indefinite S3 retention. A worst-case 20 MB
per file with ten files per workflow reaches about 309 GiB after 12 months. At
the selected US East (N. Virginia) reference price this projects to about ₹863
per month after the approved 20% margin, so indefinite S3 Standard retention at
that maximum is not viable under the ₹300 service-cost cap. The final storage
class/lifecycle decision remains a release blocker.

Sources:

- <https://aws.amazon.com/systems-manager/pricing/>
- <https://docs.aws.amazon.com/systems-manager/latest/userguide/secure-string-parameter-kms-encryption.html>
- <https://docs.aws.amazon.com/kms/latest/developerguide/concepts.html#aws-managed-cmk>
- <https://aws.amazon.com/s3/pricing/>

## Approved secret-storage design

Store each credential as a separate standard `SecureString` under an
environment-specific path, for example:

```text
/novus/development/telegram/bot-token
/novus/development/google/client-secret
/novus/development/google/employee/<opaque-id>/refresh-token
/novus/development/smtp/password
/novus/development/ai/provider-key
```

Use the AWS managed key for Systems Manager, no customer-managed KMS key, and
least-privilege IAM that permits a function to read only its own paths. Standard
parameters are limited to 4 KB, which is sufficient for the expected provider
credentials and OAuth refresh tokens. Record KMS decrypt/request counts and
migrate only if measured use exceeds the applicable KMS allowance or a provider
secret requires managed rotation.

Secrets Manager is permitted only for a demonstrated rotation requirement whose
incremental cost passes the deployment forecast. Do not bundle credentials solely
to reduce cost.

## Forecast method

The companion worksheet is `docs/cost/aws-monthly-forecast.csv`. It uses
`us-east-1` (US East, N. Virginia) as the approved lowest-cost baseline and
USD/INR = ₹100. Region prices are service-specific, so the deployment must attach
a dated Pricing Calculator estimate for this region before deployment; a
residency, latency, or service-availability requirement can override the cost
baseline.

The ₹300 cap applies to AWS service charges before GST. GST is tracked and paid
separately; it does not reduce the ₹300 operating budget. The worksheet keeps
the 20% planning safety margin approved by the project owner.

The forecast uses two scenarios:

- **Expected:** 44 production workflows per month plus 11 development and 11
  staging workflows, or 66 total. It conservatively assumes two 20 MB files
  (40 MB) per workflow.
- **Worst-case attachment:** 44 workflows in each environment, or 132 total,
  where every workflow accepts ten 20 MB files (200 MB). This is the enforced
  maximum, not a typical-use claim.

There are three employees using Google OAuth. Their maximum environment-isolated
refresh-token count is nine, with no monthly standard-parameter storage charge.

Unless replaced by measured spike data, each workflow is modeled as:

- 50 Step Functions Standard state transitions.
- 30 Lambda invocations at 1 GB for 5 seconds each.
- 10 API Gateway HTTP API requests.
- 100 DynamoDB write request units and 200 read request units.
- Retained S3 input data as defined by the scenario above, plus generated
  artifacts and history when implementation measurements become available.

CloudWatch cost is budgeted as zero under the owner direction that it should be
negligible. This is conditional, not a guarantee: log ingestion must remain at
or below 5 GB monthly and standard alarms at or below 10, with short non-
production retention and redaction enforced.

## Currency, tax, and cap treatment

The forecast uses the owner-approved conservative conversion:

```text
service-cost INR = USD list cost × 100
```

GST is excluded from the ₹300 service-cost cap and shown separately on the
actual AWS invoice. The deployment gate must use the same ₹100/USD basis unless
a later human-approved revision changes it.

Sources:

- <https://docs.aws.amazon.com/awsaccountbilling/latest/aboutv2/manage-account-payment-aispl.html>
- <https://aws.amazon.com/tax-help/india/>

## Service forecast

### Step Functions Standard

AWS charges Standard Workflows per state transition and documents a recurring
allowance of 4,000 transitions per month. Expected workflow traffic fits that
allowance; the all-environment worst-case scenario is 6,600 transitions and
produces a small charge. Retries count as transitions.

Source: <https://aws.amazon.com/step-functions/pricing/>

### Lambda, API Gateway, and DynamoDB

The worksheet retains public US East reference prices for Lambda duration and
requests, HTTP API requests, and DynamoDB on-demand reads/writes. Lambda's
recurring allowances cover the modeled duration and request counts when the
account is eligible. Data transfer, DynamoDB storage/backups, and transaction
multipliers remain to be measured.

Sources:

- <https://aws.amazon.com/lambda/pricing/>
- <https://aws.amazon.com/api-gateway/pricing/>
- <https://aws.amazon.com/dynamodb/pricing/>

### S3 retention

At 20 MB per file, expected retention adds about 2.58 GiB per month and the
maximum scenario adds about 25.78 GiB per month. With indefinite retention,
S3 Standard storage grows approximately as follows before generated artifacts,
versions, requests, and transfer:

| Scenario | Month 1 | Month 6 | Month 12 |
| --- | ---: | ---: | ---: |
| Expected retained attachments | 2.58 GiB | 15.47 GiB | 30.94 GiB |
| Worst-case retained attachments | 25.78 GiB | 154.69 GiB | 309.38 GiB |

The expected 12-month scenario remains below the cap in the worksheet. The
worst-case scenario rises to about ₹436/month at month 6 and ₹863/month at month
12 after the 20% margin. Retention remains indefinite, but S3 Standard cannot
be its permanent class at maximum usage; the release needs a priced archival
lifecycle and retrieval policy, or a revised cap/attachment policy.

Source: <https://aws.amazon.com/s3/pricing/>

### Parameter Store and KMS

Standard Parameter Store values have no additional Parameter Store charge.
`SecureString` encryption uses KMS; AWS managed keys have no monthly key-storage
fee. The worksheet includes 21 standard parameters and assumes expected KMS use
remains within the applicable free allowance. KMS API requests are monitored and
must be added to the forecast if that assumption is exceeded.

Sources:

- <https://aws.amazon.com/systems-manager/pricing/>
- <https://aws.amazon.com/kms/pricing/>

### CloudWatch logs and alarms

Logs and alarms are expected to be negligible only while they remain within the
free allowance described above. Every deployment must set retention, alert
cardinality, and redaction explicitly; an overage changes the forecast rather
than being silently absorbed.

Source: <https://aws.amazon.com/cloudwatch/pricing/>

## Costs still requiring deployment evidence

The following are not unresolved policy decisions; they are deployment evidence
or measured-use inputs required before release:

- Dated AWS Pricing Calculator export for `us-east-1`.
- S3/API Gateway internet transfer and Telegram download/upload transfer.
- S3 archival-class, minimum-duration, retrieval, and restore charges needed to
  retain maximum-size attachments indefinitely.
- DynamoDB storage, backups, point-in-time recovery, and transaction multipliers.
- Actual KMS request count, CloudWatch ingestion, retention, and alarm count.
- Lambda architecture, memory, duration, ephemeral storage, and cold-start data.
- SNS, Route 53, custom domains, certificates, or VPC networking if added.

## Required manual recalculation

Before approving the deployment:

1. Use `us-east-1` and attach a dated AWS Pricing Calculator estimate.
2. Recalculate expected and worst-case usage with measured workflow,
   attachment, generated-artifact, and history sizes.
3. Count deployed parameters, KMS requests, alarms, tables, buckets, and log
   groups from the SAM template.
4. Show gross list cost, recurring allowances, net service cost, 20% safety
   margin, and GST separately; only net service cost plus margin is compared
   with the ₹300 cap.
5. Forecast months 1, 6, and 12. Retention is indefinite, but the approved
   planning horizon is 12 months; any later forecast is required when the
   lifecycle or cap changes.
6. Confirm new intake suspends at ₹270 after the safety margin and that no
   decision reaches the ₹300 hard cap.
7. Record the archival lifecycle decision or approved replacement cap/policy.

## Approval

- [ ] Region and dated pricing-calculator evidence attached.
- [x] Expected and worst-case usage approved: two/ten files at 20 MB each.
- [x] Secret-storage architecture and cost approved: separate standard
  Parameter Store `SecureString` values using the AWS managed key.
- [ ] Shared-budget ADR approved.
- [x] ₹300 excludes GST; USD/INR is fixed at ₹100 for this forecast.
- [x] The Parameter Store replacement is approved; unrelated credential
  bundling and application-only key handling are rejected.
- [ ] Retention growth remains viable or the S3 archival lifecycle has an
  approved price and retrieval policy.
- [x] Human approver/date: **Approved by project owner, 2026-07-15**
