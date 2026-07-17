<!-- markdownlint-disable MD013 -->

# Google OAuth Lifecycle Feasibility Spike

**Status:** Test protocol approved; development OAuth run pending
**Task:** Phase 0, Task 6
**Environment:** Dedicated development Google Cloud project and test accounts only

## Purpose

Prove that Novus can connect personal Google accounts, obtain the minimum Drive,
Sheets, Docs, and Calendar permissions, refresh access without user interaction,
revoke safely, and reconnect without exposing OAuth material.

This protocol is not provider evidence. Do not mark Task 6 complete until the
live cases are recorded and `tests/fixtures/oauth/callback-cases.json` contains
only sanitized callback outcomes.

## Proposed client and scope set

Use a Google OAuth **Web application** client because the server-side callback
can protect a client secret. Configure an External audience so explicitly listed
personal Google test accounts can authorize the development application.

Start the controlled test with:

```text
openid
email
https://www.googleapis.com/auth/drive.file
https://www.googleapis.com/auth/calendar.calendarlist.readonly
https://www.googleapis.com/auth/calendar.events
```

Rationale:

- `drive.file` is Google's recommended non-sensitive per-file scope and is
  accepted by the Drive, Sheets, and Docs APIs. It allows app-created files and
  files the user explicitly opens or shares with the app, including through
  Google Picker.
- `calendar.calendarlist.readonly` lists accessible calendars and exposes the
  primary Calendar entry and timezone without granting calendar-list mutation.
- `calendar.events` permits event operations on calendars the user can access.
  The narrower `calendar.events.owned` scope must also be tested, but it cannot
  satisfy the PRD requirement for an explicitly selected non-owned calendar.
- `openid email` requests an ID token and display email without Gmail access.
  The immutable verified `sub` claim, not the email address, identifies the
  connected Google account.
- No `gmail.*` or `mail.google.com` scope is permitted.

Do not add `drive`, `spreadsheets`, `documents`, or full `calendar` scope unless
the controlled test proves a required PRD operation cannot work and a human
approves the broader permission.

Official scope references:

- <https://developers.google.com/workspace/drive/api/guides/api-specific-auth>
- <https://developers.google.com/workspace/sheets/api/scopes>
- <https://developers.google.com/workspace/docs/api/auth>
- <https://developers.google.com/workspace/calendar/api/auth>

## Development-project configuration

Record these values with non-secret aliases, never real IDs or credentials:

- Google Cloud project alias.
- OAuth client type and audience.
- Publishing status.
- Explicit test-user aliases.
- Enabled Drive, Sheets, Docs, and Calendar APIs.
- Exact authorized redirect URI. Development uses
  `http://localhost:3000/auth/google/callback`, Google's HTTP loopback exception;
  staging and production must use HTTPS.
- Consent-screen application name and support contact.
- Requested and actually granted scopes.
- Test account type: personal Google account or Workspace account.

Google documents that External applications in Testing are limited to listed
test users and that grants involving Workspace scopes expire after seven days.
Use separate development, staging, and production projects. Production
verification requirements remain pending until the final scope set is approved.

Sources:

- <https://support.google.com/cloud/answer/15549945?hl=en>
- <https://support.google.com/cloud/answer/13464323?hl=en>
- <https://developers.google.com/identity/protocols/oauth2/policies>

## Authorization request contract

Use the authorization-code flow at:

```text
https://accounts.google.com/o/oauth2/v2/auth
```

Each authorization request must include:

- The development `client_id`.
- An exact registered HTTPS `redirect_uri`, except that the dedicated local
  development client may use Google's HTTP localhost/loopback exception.
- `response_type=code`.
- A space-delimited approved `scope` set.
- `access_type=offline`.
- A fresh, unpredictable, single-use `state` bound to the initiating Telegram
  participant and private-chat onboarding request.
- A fresh OpenID Connect `nonce` bound to that state.
- A fresh PKCE verifier and `code_challenge_method=S256`.

The callback exchanges the code at:

```text
POST https://oauth2.googleapis.com/token
```

The exchange must use the same redirect URI and PKCE verifier. Google explicitly
recommends state for CSRF protection. Its provider metadata advertises S256 PKCE
support, although the confidential web-server guide does not require PKCE. The
live test must prove this exact combination rather than assuming native-client
documentation applies unchanged.

Validate the ID token before binding an account: verify Google's signature using
its current published keys, accepted issuer, exact OAuth client audience,
expiration, issued-at time, and the state-bound nonce. Require a non-empty `sub`
and store that immutable subject as the Google identity. Treat `email` as display
metadata only and record `email_verified` where present. Reject an ID token with
an invalid signature, issuer, audience, expiry, nonce, or missing subject.

Source: <https://developers.google.com/identity/openid-connect/openid-connect#validatinganidtoken>

Sources:

- <https://developers.google.com/identity/protocols/oauth2/web-server>
- <https://developers.google.com/identity/protocols/oauth2/resources/best-practices>
- <https://accounts.google.com/.well-known/openid-configuration>

## Application state policy to test

Use a proposed ten-minute state lifetime for the spike. This is an application
policy, not a Google-published authorization-code lifetime.

A state record contains only:

- A one-way digest of the random state value.
- Telegram participant ID alias.
- Private chat ID alias.
- PKCE verifier and OpenID Connect nonce encrypted outside the fixture.
- Creation and expiry timestamps.
- Consumed timestamp.
- Requested scope-set version.

Reject missing, mismatched, expired, participant-mismatched, chat-mismatched, and
already-consumed state before token exchange. Consume state atomically so two
concurrent callbacks cannot both exchange the code.

OAuth links, state values, authorization codes, client secrets, access tokens,
refresh tokens, ID tokens, token endpoint payloads, and credential-bearing
errors must never enter a forum topic, logs, or committed fixtures.

## Scenario matrix

Use **Personal A** as the primary test user and, if available, **Workspace B** as
an additional compatibility check.

| ID | Action | Required evidence | Expected before test | Observed |
| --- | --- | --- | --- | --- |
| O01 | Authorize Personal A with the proposed scopes, offline access, state, and S256 PKCE | Sanitized request parameters, granted scope names, callback result, token presence booleans | Successful private onboarding; refresh token expected on initial consent | Pending |
| O02 | Omit state | Application rejection before token exchange | Rejected | Pending |
| O03 | Replace state with an unknown value | Application rejection before token exchange | Rejected | Pending |
| O04 | Reuse O01 state and callback | One accepted callback and one replay rejection | Second callback rejected | Pending |
| O05 | Use valid state from another Telegram participant/private chat | Binding mismatch without token exchange | Rejected | Pending |
| O06 | Wait beyond the proposed ten-minute state lifetime | Expiry rejection before token exchange | Rejected | Pending |
| O07 | Exchange one Google authorization code twice | Sanitized status/error category from both attempts | First succeeds; exact replay error must be observed | Pending |
| O08 | Change redirect URI scheme, host, port, path, case, and trailing slash one at a time | Error category for each mismatch | Exact match required | Pending |
| O09 | Refresh after discarding/expiring the access token | Refresh result and granted capability checks | Succeeds without user interaction | Pending |
| O10 | Reconnect while consent already exists and without forced consent | Presence/absence of a new refresh token | Must not overwrite an existing refresh token with null | Pending |
| O11 | Reconnect with explicit consent after securely removing the prior grant | Fresh state/verifier and token presence booleans | Fresh usable grant | Pending |
| O12 | Revoke at Google's revocation endpoint, then refresh | Revocation status and sanitized refresh error | Refresh becomes unusable; timing must be observed | Pending |
| O13 | Reconnect after revocation | New authorization transaction and API capability checks | Access restored with fresh state/verifier | Pending |
| O14 | Retain a Testing-mode grant beyond seven days | Date-stamped refresh result | Development refresh token expected to expire under Google's Testing policy | Pending |
| O15 | Create a Sheet and Doc using `drive.file`, then read and modify them | Resource aliases and success categories | Allowed | Pending |
| O16 | Select an existing Sheet and Doc through Google Picker, then read and modify them | Picker grant and operation categories | Allowed | Pending |
| O17 | Attempt an unselected Drive file by copied URL/ID | Sanitized denial category | Denied under `drive.file` | Pending |
| O18 | Create an event on the primary owned calendar | Event alias and success category | Allowed by either Calendar candidate scope | Pending |
| O19 | Create an event on a non-owned calendar where Personal A has write access | Compare `calendar.events.owned` and `calendar.events` | Owned scope should be insufficient; broader events scope should satisfy PRD | Pending |
| O20 | Attempt a calendar without write access | Sanitized denial category | Denied | Pending |
| O21 | Inspect granted scopes and verify no Gmail scope | Exact sanitized scope names | Gmail absent | Pending |
| O22 | Complete the full flow using a personal Google account | Account type and capability results | Supported | Pending |
| O23 | Validate ID tokens with wrong issuer, audience, expiry, nonce, signature, and missing subject | One sanitized rejection per claim/signature case | Every invalid token rejected before account binding | Pending |
| O24 | List accessible calendars and read the primary Calendar timezone | Calendar aliases, primary flag, timezone, and granted scopes | Works with `calendar.calendarlist.readonly` | Pending |
| O25 | Create files in configured My Drive and Shared Drive folders explicitly selected for the app | Folder aliases, selection provenance, operation result | Works only where `drive.file` grants access and account permissions allow | Pending |

## Refresh and revocation rules

Request offline access during initial consent. Google notes that a refresh token
may only be returned on the first authorization and can later become invalid due
to revocation, inactivity, token limits, account policy, or Testing-mode expiry.

Implementation rules to confirm in the spike:

- Never replace a valid stored refresh token when a successful token response
  omits `refresh_token`.
- Treat `invalid_grant` during refresh as reauthorization-required, not as a
  retry loop.
- After disconnect, immediately mark the credential unusable by Novus and pause
  Google-dependent work without deleting workflow history.
- Delete the encrypted credential only after Google conclusively accepts
  revocation. If revocation times out after the request may have reached Google,
  keep an encrypted `pending_revocation` record that no application path may use,
  enter manual review, and direct the user to remove Novus from Google Account
  permissions. An ambiguous revocation must not be reported as successful.
- Retry only conclusive pre-submission or explicit transient revocation failures,
  subject to the shared three-attempt limit.
- Reconnect with a new state, nonce, and PKCE verifier. A new grant does not erase
  unresolved evidence about the old pending revocation.
- Record only outcome categories and opaque internal credential references.

Sources:

- <https://developers.google.com/identity/protocols/oauth2#expiration>
- <https://developers.google.com/identity/protocols/oauth2/web-server#tokenrevoke>

## Sanitized callback fixture

Create `tests/fixtures/oauth/callback-cases.json` only after the live run. The
fixture records callback inputs with synthetic values and expected application
outcomes, not provider tokens or raw provider responses.

```json
{
  "schema_version": 1,
  "cases": [
    {
      "case_id": "O02",
      "query": { "code": "sanitized-code" },
      "expected": "missing_state"
    }
  ]
}
```

Sanitization must:

- Replace client, project, account, Telegram, state, code, verifier, and token
  references consistently.
- Remove query strings from logged callback URLs.
- Preserve only HTTP status, stable provider error category, field presence,
  scope names, timestamps rounded enough to avoid correlation, and application
  decision.
- Exclude access tokens, refresh tokens, ID tokens, client secrets, raw
  authorization codes, PKCE verifiers, cookies, and authorization headers.

## Secret-storage cost evidence

Record the number of OAuth client secrets and refresh-token records required in
each environment and price them in `docs/cost/aws-monthly-forecast.csv`.
The current Secrets Manager interpretation is already above each environment's
₹300 budget when credentials remain properly isolated.
Task 6 cannot approve a storage mechanism that Task 8 deems unaffordable.

## Approval

- [ ] Exact scope set supports app-created and explicitly selected Drive files.
- [ ] Exact Calendar scopes support calendar discovery, primary timezone, and
  approved alternate-calendar behavior.
- [ ] The approved existing-file selection mechanism covers configured My Drive
  and Shared Drive folders; if Google Picker is required, its UI and scopes have
  explicit architecture approval.
- [ ] ID-token signature and claims bind the immutable Google `sub`, not email.
- [ ] No Gmail scope is requested or granted.
- [ ] State and PKCE success, mismatch, replay, and expiry are demonstrated.
- [ ] Refresh, revocation, Testing-mode expiry, and reconnect are demonstrated.
- [ ] Personal Google-account behavior is demonstrated.
- [ ] Test-user and production-verification requirements are recorded.
- [ ] Secret-storage count and cost are reconciled with Task 8.
- [ ] Sanitized fixture contains no OAuth material.
- [x] Human approver/date: **Approved by project owner, 2026-07-15**
