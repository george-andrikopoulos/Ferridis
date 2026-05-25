# Ferridis intent vocabulary v0

> **Status:** Working draft. Proposed for AAIF governance once the
> foundation has working-group capacity for vocabulary work. Until
> then this document is the canonical reference for Ferridis adapter
> authors and consumers, maintained in this repository as a living
> draft.
>
> **Author:** George Andrikopoulos. Contributions welcome via PR; see
> [Governance](#governance) below.

## What this document is

The Ferridis architecture (see [`architecture.md`](./architecture.md))
separates **what a capability does** (the intent) from **who provides
it** (the capability ref). When a host asks Ferridis "send a message",
the runtime resolves that to a list of capabilities that declare the
`send-message` intent — Slack, Discord, Signal, Mastodon, whatever
the user has connected.

For that resolution to work, the intent name has to mean the same
thing across publishers. `send-message` on Slack and `send-message`
on Discord need to take the same request shape, return the same
response shape, and represent the same semantic operation. Otherwise
the host is back to per-publisher branching, which is what Ferridis
exists to eliminate.

The intent vocabulary is the shared agreement on those names and
shapes. **This document is v0 of that agreement.**

What this document is **not**:

- A schema specification. Each verb's request/response shape is
  declared in the capability's OpenAPI / AsyncAPI document, not here.
  This document fixes the *names* and gives each verb a one-line
  contract; the full shapes live with the manifests.
- An exhaustive list. Adapter authors will surface gaps. The
  vocabulary grows by addition; v0 is the seed.
- A binding standard. Until AAIF (or its successor working group)
  formalizes it, this is a working draft maintained by the Ferridis
  project. Adapter authors who follow it get the federation benefits
  Ferridis is designed for.

## Naming convention

Intent verbs are validated by
[`ferridis_core::IntentVerb`](./rust/crates/ferridis-core/src/intent.rs):

- Lowercase ASCII letters, digits, and dashes only: `[a-z0-9-]+`.
- No leading dash, no trailing dash, no consecutive dashes.
- Dots, underscores, mixed case are rejected at parse time.

In addition to the syntactic rules, vocabulary contributors should
follow these editorial rules so the vocabulary stays scannable:

1. **Verb-first.** `send-message`, not `message-send`. Start with the
   action, end with the noun.
2. **Generic, not capability-scoped.** `send-message` — not
   `slack-send-message`. The capability ref provides the scope; the
   intent provides the meaning.
3. **No marketing names.** `list-events` covers Google Calendar, Apple
   Calendar, Outlook, Fastmail, and anything else with the same
   concept. The vocabulary outlives any individual product.
4. **No prefixes for danger or write semantics.** Whether `delete-file`
   is destructive is the capability's responsibility to declare and
   the host's responsibility to gate. Marking it in the verb name
   couples vocabulary to enforcement.
5. **Singular nouns for single-object operations, plural for
   collections.** `read-event` (one), `list-events` (many).

## v0 vocabulary

Thirty-five verbs across eight categories. Each row gives the verb, a
one-line semantic contract, and the canonical input shape. Full
request and response schemas live in capabilities' OpenAPI documents.

### Messaging (5)

| Verb | Contract | Canonical input |
|---|---|---|
| `send-message` | Send a message to a conversation. | `{conversation_id, body, attachments?}` |
| `read-messages` | List messages in a conversation, newest-first by default. | `{conversation_id, limit?, since?, cursor?}` |
| `list-channels` | List broadcast channels the connection can post to. | `{}` |
| `list-conversations` | List conversations (DMs and channels) for the current user. | `{limit?, cursor?, archived?}` |
| `react-to-message` | Attach a reaction to a message. | `{conversation_id, message_id, reaction}` |

### Calendar (6)

| Verb | Contract | Canonical input |
|---|---|---|
| `list-events` | Events in a date range, optionally filtered by calendar. | `{calendar_id?, from, to, limit?, cursor?}` |
| `read-event` | One event by id. | `{calendar_id?, event_id}` |
| `create-event` | Create an event. | `{calendar_id?, title, start, end, attendees?, location?, description?}` |
| `update-event` | Update an event by id; fields are optional. | `{calendar_id?, event_id, ...fields}` |
| `delete-event` | Delete an event. | `{calendar_id?, event_id}` |
| `find-free-time` | Find availability windows across attendees. | `{attendees, from, to, duration_min}` |

### Files (6)

| Verb | Contract | Canonical input |
|---|---|---|
| `read-file` | Read a file's bytes (or UTF-8 text) by path or id. | `{path}` |
| `write-file` | Write a file. Creates or overwrites. | `{path, content, encoding?}` |
| `list-dir` | List entries in a directory. | `{path}` |
| `search-files` | Find files matching a pattern under a root. | `{path, name_contains, limit?}` |
| `move-file` | Move or rename. | `{from, to}` |
| `delete-file` | Delete a file. | `{path}` |

### Search (3)

| Verb | Contract | Canonical input |
|---|---|---|
| `search` | Generic search over the capability's scope (messages, mail, docs, whatever the capability indexes). | `{query, limit?, cursor?, filters?}` |
| `search-similar` | Find items semantically similar to a reference item. | `{reference_id, limit?}` |
| `fetch-document` | Retrieve a full document by id (the long-form companion to `search`). | `{document_id}` |

### Payments (3)

| Verb | Contract | Canonical input |
|---|---|---|
| `create-payment-intent` | Open a payment for the user to confirm out-of-band. | `{amount, currency, description?, metadata?}` |
| `list-transactions` | List transactions for the connected account. | `{from?, to?, limit?, cursor?}` |
| `refund-transaction` | Issue a refund. | `{transaction_id, amount?, reason?}` |

### Identity (3)

| Verb | Contract | Canonical input |
|---|---|---|
| `get-current-user` | Profile of the user this connection is authenticated as. | `{}` |
| `list-organizations` | Orgs / workspaces / teams the user belongs to. | `{}` |
| `list-members` | Members of an organization. | `{organization_id, limit?, cursor?}` |

### Scheduling (3)

| Verb | Contract | Canonical input |
|---|---|---|
| `list-tasks` | Tasks for the connected list / project. | `{list_id?, status?, limit?, cursor?}` |
| `create-task` | Create a task. | `{list_id?, title, due?, assignee?, description?}` |
| `complete-task` | Mark a task complete. | `{task_id}` |

### Common (6)

These apply to almost every capability; vocabulary contributors should
implement them where they make sense.

| Verb | Contract | Canonical input |
|---|---|---|
| `ping` | Liveness probe. Returns immediately. | `{}` |
| `health` | Richer health: connection status, rate-limit headroom, optional service version. | `{}` |
| `list-capabilities` | Enumerate the sub-capabilities this manifest exposes (for capabilities that are themselves brokers). | `{}` |
| `list-resources` | List the resource collections this capability operates on (calendars, channels, lists, etc.). | `{}` |
| `get-resource` | Read a resource by id. | `{resource_id}` |
| `list-versions` | Version compatibility info (which vocabulary versions and capability versions this implementation supports). | `{}` |

## Versioning

The vocabulary itself is versioned alongside Ferridis-core. v0 is the
seed; subsequent versions add verbs and may refine existing
contracts. **Existing verbs do not change semantics across vocabulary
versions** — that would break the federation guarantee. Instead:

- New verbs are added with the next-higher version (`v1`, `v2`, …).
- If a verb's semantics genuinely need to evolve, the new behavior
  gets a new verb (e.g., `list-events-v2`). The old verb continues to
  mean what it meant.
- Capabilities declare which vocabulary versions they support via
  `list-versions`.

The `IntentVerb` parser already accepts digits (since the dot-name
work in 2026-05) so `list-events-v2` is syntactically valid.

## Governance

> **Proposed pathway:** Submit v0 to AAIF as the basis for an
> AAIF-governed vocabulary working group. Until AAIF accepts the
> proposal (or refers it to a different home), the document is
> maintained in this repository by George Andrikopoulos with PRs
> welcome. The eventual disposition is intended to be:
>
> - **AAIF working group** owns the vocabulary spec.
> - Ferridis becomes a reference implementation, not the source of
>   truth.
> - Vocabulary versions are released on the AAIF cadence, not
>   Ferridis's release cycle.

### Contribution criteria (interim, pre-AAIF)

To add a verb to v0, a PR should establish:

1. **At least two independent existing or proposed adapters** that
   need the verb. (Avoids speculative verbs that nobody will
   implement.)
2. **A canonical input shape.** Doesn't have to be a full schema yet;
   one-line description of fields, with required vs. optional marked.
3. **A rationale for the name.** If a verb already nearly covers it,
   we extend the existing verb's contract instead of adding a new
   one.
4. **At least one publisher commits to implementing it** in a
   reference adapter.

To modify an existing verb's name or required input fields, the bar
is much higher (it breaks federation). The default answer is "add a
new verb."

### Out of scope

- **Capability-specific verbs.** If only Slack needs it, it lives in
  Slack's manifest as a custom intent, not in this vocabulary.
- **Tooling for human-in-the-loop confirmation.** That's a host /
  policy layer concern, not a vocabulary concern. See the
  [architecture](./architecture.md) discussion of fallback tiers.
- **Argument validation rules** beyond shape. Whether `amount` in
  `create-payment-intent` must be positive is the capability's
  problem to enforce, not the vocabulary's.

## Open questions

1. **Search scope.** `search` is intentionally generic — the
   capability provides the haystack. Should there be a way for a
   capability to declare which haystacks it indexes (mail, files,
   messages)?  This is more useful for federated search than for
   single-capability search.
2. **Streaming.** Several verbs (`search`, `read-messages`,
   `list-events`) naturally stream. The vocabulary stays
   shape-agnostic for now; the streaming response schema language
   (M9 in the roadmap) will declare per-verb whether the response is
   single-shot or streamed.
3. **Locale and i18n.** No verb in v0 takes a locale parameter;
   results are assumed to be in the connection's locale (set at
   OAuth time). Should we add a per-call locale override?
4. **Audit log.** Some capabilities expose audit / history of
   operations. Add a `list-audit-events` verb to Common, or keep that
   capability-specific?
5. **Cancellation.** Long-running operations (large file uploads,
   batch operations) might want a `cancel` verb. v0 does not
   include one; revisit when M9 (streaming) lands.

## What this enables

Once a vocabulary is fixed and adapters follow it, three things
become possible that aren't possible today:

- **Cross-capability routing.** "Send a message" works whether the
  host has Slack, Discord, or Signal connected.
- **Capability substitution.** A user can swap one calendar provider
  for another and their assistant keeps working.
- **Vocabulary-level testing.** A test harness can drive every
  adapter's `send-message` against a shared compliance suite.

The vocabulary is the smallest piece of structure that gets us
there. Keeping it small (35 verbs in v0) and stable is the work; the
rest of Ferridis is plumbing.
