<!-- markdownlint-disable MD013 -->

# Telegram Forum-Topic Feasibility Spike

**Status:** Test protocol approved; controlled test pending
**Task:** Phase 0, Task 1
**Environment:** Development bot and sanitized forum supergroup only

## Purpose

Prove what Telegram actually delivers to Novus in a forum supergroup and which
identifiers remain stable enough to enforce workflow isolation. This document is
a test protocol, not evidence. Do not mark the spike complete until every
required scenario has an observed result linked to a sanitized update in
`tests/fixtures/telegram/forum-spike.json`.

The test must not use a production token, production group, real customer data,
or employee personal data. Record the Bot API version, test date, bot username,
and configuration state with pseudonymous values only.

## Official behavior to verify

The following are hypotheses from Telegram's documentation, not substitutes for
the controlled test:

- Telegram says privacy-enabled bots receive commands explicitly addressed to
  them, some general commands, messages sent via the bot, and replies to bot
  messages.
- Telegram says bot administrators and bots with privacy mode disabled receive
  all group messages except messages from other bots.
- `Update.update_id` is the update identifier used to confirm processed updates.
- Forum messages may carry `message_thread_id`, which must remain associated
  with the originating chat and topic.
- `getChatMember` is guaranteed to work for other users only when the bot is an
  administrator in the chat.

Sources:

- <https://core.telegram.org/bots/faq#what-messages-will-my-bot-get>
- <https://core.telegram.org/bots/api#update>
- <https://core.telegram.org/bots/api#message>
- <https://core.telegram.org/bots/api#callbackquery>
- <https://core.telegram.org/bots/api#getchatmember>
- <https://core.telegram.org/bots/api#setwebhook>

## Configuration matrix

Run the delivery scenarios in both configurations. Using only the administrator
configuration would not prove privacy-mode filtering because Telegram documents
that bot administrators receive all group messages.

| Configuration | BotFather privacy | Bot chat role | Purpose | Observed |
| --- | --- | --- | --- | --- |
| A | Enabled | Member, not administrator | Prove privacy-mode delivery | Pending |
| B | Enabled | Administrator with the minimum test permissions | Prove membership/service-update behavior and identify delivery differences caused by admin status | Pending |

For each configuration, capture:

- BotFather privacy setting as shown by `/mybots` -> **Bot Settings** ->
  **Group Privacy**.
- Whether the bot is an administrator and its exact granted permissions.
- Webhook `allowed_updates`, especially `message`, `callback_query`,
  `my_chat_member`, and `chat_member`.
- Whether the forum has Topics enabled.
- Whether the bot was removed and re-added after changing privacy mode; Telegram
  notes in BotFather that group privacy changes may require re-adding the bot.

Do not grant broad administrator permissions merely to make the test pass. If
administrator status is required for reliable live membership checks, record
that its broader update visibility is an application filtering concern rather
than claiming privacy mode prevents delivery.

## Stable-identifier assertions

For every observed update, record and compare:

- `update_id`.
- `message.message_id` or `callback_query.message.message_id`.
- `message.chat.id` and `message.chat.type`.
- `message.is_topic_message`.
- `message.message_thread_id`.
- `from.id` and `sender_chat.id`, when present.
- `reply_to_message.message_id` and its `message_thread_id`, when present.
- `callback_query.id`, `callback_query.from.id`, callback `data`, and the
  callback message's chat/thread identifiers.
- Text or caption entities, including the bot mention or command entity offsets.
- Attachment `file_id` and `file_unique_id`.
- Membership update `old_chat_member.status`, `new_chat_member.status`, actor,
  subject user, and chat ID.

The routing candidate is the pair `(chat.id, message_thread_id)`, never
`message_thread_id` alone. The test must use at least two topics in the same
forum and, if practical, identically titled topics to show that display names
are not routing identifiers.

## Scenario matrix

Use pseudonymous participants **Owner A** and **Participant B**, topics **Topic
Alpha** and **Topic Beta**, and a harmless generated image/document. Keep each
instruction free of customer or company data.

| ID | Configuration | Action | Required evidence | Expected before test | Observed |
| --- | --- | --- | --- | --- | --- |
| F01 | A | Owner A posts `@<bot> start ping` in Topic Alpha | Message, mention entity, chat ID, topic ID | Delivery must be tested; the FAQ does not make plain mentions sufficiently explicit for this design | Pending |
| F02 | A | Owner A posts an image whose caption mentions the bot in Topic Alpha | Caption entity, photo/document fields, topic ID | Delivery must be tested | Pending |
| F03 | A | Owner A posts the same kind of image without a mention or reply | Presence or proven absence during a bounded observation window | Likely not delivered under privacy mode, but must be observed | Pending |
| F04 | A | Owner A replies to a Novus message with an untagged text follow-up | Reply metadata and topic ID | Telegram documents replies to the bot as delivered | Pending |
| F05 | A | Owner A replies to a Novus message with an untagged attachment | Reply metadata, attachment fields, topic ID | Delivery must be tested | Pending |
| F06 | A | Owner A sends `/done` | Command entity and topic ID, or proven absence | Delivery depends on Telegram's general-command routing state; must not be assumed | Pending |
| F07 | A | Owner A sends `/done@<bot_username>` | Command entity and topic ID | Expected to be delivered as an explicitly addressed command | Pending |
| F08 | A | Owner A sends `/status`, `/correct test`, `/confirm`, and `/stop`, each as a reply to Novus | One update per command with reply/thread metadata | Expected to be delivered as replies | Pending |
| F09 | A | Participant B presses an inline callback button on a Novus message | Callback ID, actor, data, message chat/thread IDs | Expected callback query; thread location must be verified | Pending |
| F10 | A | Repeat F01 and F09 in Topic Beta | Distinct topic IDs and no cross-topic identifiers | Same chat ID, different stable topic ID | Pending |
| F11 | A | Send concurrent messages in Alpha and Beta | Ordered capture with update IDs and route pairs | No assumption that update arrival order equals send order | Pending |
| F12 | A | Return a non-2xx webhook response once, then accept the retry | Raw deliveries and repeated identifiers | Determine Telegram redelivery behavior; do not synthesize evidence | Pending |
| F13 | Local replay | Submit one sanitized captured payload twice to the fixture replay path | Byte-identical input and deduplication result | Same `update_id` must map to one accepted application event later | Pending |
| F14 | B | Repeat F01-F08 as bot administrator | Compare delivered updates with Configuration A | Telegram documents administrators as receiving all non-bot messages | Pending |
| F15 | B | Remove Participant B, then have B attempt a late command if Telegram permits | Membership update, `getChatMember`, attempted update or proven inability | Membership status must deny authorization regardless of message delivery | Pending |
| F16 | B | Remove Novus from the forum | `my_chat_member` update and any service message | Bot-removal evidence must be distinct from participant removal | Pending |
| F17 | B | Re-add Novus, recreate webhook subscription, and retry one topic action | Membership update and topic-scoped message | Required recovery steps must be documented | Pending |
| F18 | A and B | Call `getChatMember` for Owner A and removed Participant B | API result in each bot-role configuration | Reliability difference between member/admin roles must be recorded | Pending |

### Bounded absence evidence

An absent update cannot be represented by inventing a payload. For F03, F06,
and any other non-delivery result:

1. Record the Telegram message link or a sanitized local test sequence number.
2. Record the webhook collector's start and end update IDs.
3. Wait at least 60 seconds while the collector is healthy.
4. Confirm webhook status has no pending update or delivery error attributable
   to the scenario.
5. Record the observation interval and result in this report only.

## Duplicate-update procedure

Telegram redelivery and application replay are separate claims:

1. **Provider redelivery:** intentionally return a non-2xx response for one
   development webhook delivery, then capture the subsequent delivery. Record
   whether `update_id` and the embedded message/callback identifiers remain the
   same. Restore successful responses immediately.
2. **Local deterministic replay:** after sanitization, feed the exact saved JSON
   object twice through the future normalization/deduplication test. This proves
   application behavior but is not evidence that Telegram itself redelivered
   the update.

Do not leave the webhook in a failing state or repeatedly trigger Telegram
retries.

## Sanitization and fixture contract

Store captured evidence at `tests/fixtures/telegram/forum-spike.json` only after
sanitization. The fixture should be one JSON object with metadata and a list of
cases:

```json
{
  "schema_version": 1,
  "captured_at": "YYYY-MM-DD",
  "bot_api_version": "record-at-test-time",
  "cases": [
    {
      "case_id": "F01",
      "configuration": "A",
      "delivery": "delivered",
      "update": {}
    }
  ]
}
```

Use deterministic replacements so equality and cross-reference checks remain
possible:

- Map the development forum chat ID to one fixed non-production integer.
- Map each user ID, message ID, topic ID, callback ID, file ID, and update ID
  consistently across all cases while preserving equality/inequality
  relationships and update ordering.
- Replace names, usernames, URLs, file names, and text with the pseudonyms and
  harmless instructions in this protocol.
- Remove bot tokens, webhook secret tokens, authorization headers, IP addresses,
  signed URLs, OAuth material, and unrelated headers entirely.
- Preserve JSON field presence, nullability, types, entity offsets, array
  lengths, MIME types, file sizes, and identifier relationships needed by the
  assertions.
- Validate that no replacement accidentally contains a real Telegram ID or
  secret fragment before committing.

Keep the unsanitized capture outside the repository and delete it after the
human verifies the sanitized fixture, unless an approved secure evidence policy
requires temporary retention.

## Replay checklist

For each delivered fixture case, manually confirm:

- The update parses as JSON without modification.
- The documented field path exists with the documented JSON type.
- `(chat.id, message_thread_id)` matches the scenario's topic.
- Mention, command, caption, reply, attachment, and callback fields match the
  visible test action.
- Replayed duplicate `update_id` values remain equal.
- Alpha and Beta have different topic IDs and cannot produce the same route
  pair.
- Removed-member evidence identifies whether it concerns Novus or a participant.

## Decision output required from this spike

After the controlled test, replace the pending cells and state the minimum
approved production configuration:

- BotFather group privacy mode.
- Whether Novus must be a forum administrator.
- Exact minimum administrator permissions, if any.
- Webhook `allowed_updates` values.
- The user interaction rule for mentions, replies, addressed commands, bare
  commands, and attachments.
- How live membership checks work and what is unavailable when Novus is not an
  administrator.
- Whether administrator delivery broadens visibility beyond intended workflow
  turns, and which updates the application must ignore.

Do not resolve these points from documentation alone; Configuration A and B
must provide observed evidence.

## Evidence log

| Date | Tester | Bot API version | Configuration | Cases | Fixture revision | Result |
| --- | --- | --- | --- | --- | --- | --- |
| Pending | Pending | Pending | Pending | Pending | Pending | Pending |

## Human approval

- [ ] Every F01-F18 scenario has an observed result or a documented, approved
  reason it could not be run.
- [ ] `tests/fixtures/telegram/forum-spike.json` contains sanitized captured
  updates only.
- [ ] Manual replay confirms all documented fields.
- [ ] Required BotFather, webhook, and administrator configuration is explicit.
- [ ] The final interaction contract has been reconciled with
  `docs/prd/telegram-operations-worker.md`.
- [x] Human approver name/date: **Approved by project owner, 2026-07-15**
