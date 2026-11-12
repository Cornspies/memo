# Threat model

## Assets

1. Memo content: Markdown bodies, attachments, and attachment metadata (file type, dimensions).
2. Account secrets: passwords, recovery keys, account keys, private keys, memo keys.
3. Access integrity: only members can read a memo, and only intended members ever receive its key.

## Adversaries and guarantees

| Adversary | Can | Cannot (by design) |
|---|---|---|
| Someone with a copy of the database, blob directory or backups | See metadata (below). Try to brute-force a user's password offline against `auth_hash` / the KDF | Read content without guessing the password or having a recovery key. Argon2id parameters set the cost of guessing |
| Malicious or compromised server, with users on **native clients** (desktop, Android) | Withhold or delay changes, refuse service, delete data, lie about memberships, serve wrong public keys to users who have not pinned them yet | Read content. Forge or modify memo content or attachments. Replay an old memo version as newer. Get itself added to a memo |
| Malicious or compromised server, with users on the **web client** | Everything above, **plus** serve modified JavaScript/WASM that captures passwords or keys | — (no protection: see limitations) |
| Another user on the same server | Look up usernames and public keys. Share memos with you | Read memos that were not shared with them. Access attachments of those memos |
| Former member (after revocation) | Keep whatever they downloaded while they had access | Read any version written after revocation (the memo key is rotated) |
| Network attacker | Nothing beyond what TLS at the reverse proxy allows | — |

## Metadata visible to the server

The server sees and stores in plaintext:

- usernames, account creation time, device names and last-seen times, IP addresses (in logs)
- how many memos each user has, their (padded) sizes, and creation/update/deletion times
- who shares which memo with whom, with which permission, and who made each edit
- how many attachments each memo has, their exact ciphertext sizes and upload times
- per-user `pinned` / `archived` flags

Memo bodies, tags (written as `#hashtags` in the body), titles, attachment file types and dimensions,
and contact verification state are encrypted.

## Out of scope

- **Compromised or unlocked devices.** Native clients keep decrypted content in their local database
  so they can search it, and rely on OS disk encryption (Android file-based encryption, LUKS on
  Linux). Malware on a device sees what the user sees.
- **Denial of service by the server operator.** The operator can always delete data. Keep backups.
- **Traffic analysis** beyond the metadata above.

## Known limitations

1. **The web client trusts the server that delivers its code.** Web E2E apps all share this. For
   protection against the server operator, use the native clients.
2. **Permissions are enforced by the server, not by cryptography.** Every member holds the memo key.
   A `read` member and a colluding server could write new versions, which the other members' clients
   would accept. Future work: per-edit signatures.
3. **The editor shown on a version (`updated_by`) is asserted by the server, not authenticated.** Same
   future work as above.
4. **Forgotten password + lost recovery key = permanent data loss.** No administrator can reset it.
5. **Trust on first use for contact keys.** The first public key a client sees for a user is pinned.
   A server that lies *at first contact* is only detected if the users compare fingerprints out of band.
6. **The protocol is not audited.** It composes standard primitives (Argon2id, HKDF, XChaCha20-Poly1305,
   HPKE) in a conventional way, but nobody has formally reviewed this construction.
7. **Rollback across all of a user's devices.** A client rejects memo and vault versions lower than the
   highest it has seen. A brand-new device, though, has no history to compare against: a server could
   hand it an old vault, without the newer contact pins and share records, or old memo versions.
8. **A member who left keeps the memo key** until the owner removes them, which rotates it. The server
   stops sending them the memo, but the encryption alone does not keep them out.
