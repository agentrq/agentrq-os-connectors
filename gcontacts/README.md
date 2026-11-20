# gcontacts — Google Contacts connector

An [AgentRQ](https://agentrq.com) connector (MCP server compiled to a `wasm32-wasip2`
component) for the Google People API v1 — the API behind Google Contacts.

Unlike a 1:1 mapping of the API, the tools are **contact-shaped**: each one
finishes an outcome in a single call, and every contact read is answered as a
flat, agent-friendly object instead of raw `Person` JSON:

```json
{
  "resourceName": "people/c123",
  "displayName": "Ada Lovelace",
  "givenName": "Ada",
  "familyName": "Lovelace",
  "emails": ["ada@example.com", "ada@work.example"],
  "phones": ["+15551234567"],
  "organization": {"name": "Analytical Engines", "title": "Mathematician"},
  "addresses": ["12 St James Square, London"],
  "notes": "First programmer"
}
```

Primary email/phone entries come first; the API's metadata noise never leaves
the connector. Writes do their own choreography internally: `updateContact`
resolves the API's etag with a read before patching, and the group tools
accept a label **name or** resource name, resolving it with one extra lookup
when needed.

![icon](icon.svg)

## Tools

| Tool | What it does in one call |
|---|---|
| `searchContacts` | Find contacts by name, email, phone, or organization — one search across all fields |
| `getContactByEmail` | Get one contact's full details by email address: searches internally, then fetches the exact match (fails clearly on zero or several matches) |
| `getContact` | Get one contact's full details by resource name |
| `listContacts` | List contacts, newest first by default, with paging |
| `createContact` | Create a contact from plain fields: name parts, emails, phones, organization, addresses, notes |
| `updateContact` | Change a contact's details by resource name; the etag is resolved internally |
| `deleteContact` | Delete a contact (answers `{"deleted": true}`) |
| `listContactGroups` | List your contact labels with member counts |
| `getContactGroup` | Get one label's details by name or resource name, including member resource names |
| `createContactGroup` | Create a new contact label |
| `renameContactGroup` | Rename a label, by name or resource name |
| `deleteContactGroup` | Delete a label, by name or resource name; optionally delete its contacts too |
| `modifyGroupMembers` | Add and/or remove contacts from a label; answers the label's new summary |
| `listOtherContacts` | List "other contacts" — addresses Google collected automatically that aren't in your contacts |
| `copyOtherContact` | Promote an "other contact" into your real contacts |

`updateContact` replaces the fields it is given wholesale: to change emails or
phones, send the complete list you want to keep. `displayName` is written as
the contact's full name (`unstructuredName` — the API treats `displayName`
itself as output-only).

## Auth

OAuth 2.0 (authorization code + PKCE), scope
`https://www.googleapis.com/auth/contacts`. The host runs the sign-in and
injects the Bearer token; the connector code never sees it. Configure the
`client_id` / `client_secret` inputs and the `api_base` input
(default `https://people.googleapis.com/v1`).

## Exclusions

- The Workspace directory (`people.listDirectoryPeople`) is not covered: it
  needs the extra `directory.readonly` scope, and mixing it into this
  connector's single-scope OAuth flow would complicate sign-in for everyone.
- "Other contacts" are read-only by design of the API: only `names`,
  `emailAddresses`, and `phoneNumbers` are readable there, and the only
  write is promoting one into your contacts (`copyOtherContact`).

## Build & test

Built with the AgentRQ `wasmrs` connector toolchain (`oauth2` template):

```sh
cargo test          # unit tests (100% line and region coverage required)
```

`sdk/connectors-sdk` is a host-side test double of the private AgentRQ
`connectors-sdk`, providing the same API surface so tests run anywhere. The
real SDK validates `manifest.json` at compile time and performs HTTP inside
the `wasm32-wasip2` guest.

The multi-call tools stay unit-testable through a small `Transport` trait:
the guest implementation sends requests over HTTP, while tests answer from
canned responses and assert on the requests the tools planned.
