# Tern — Implementation Decisions

Decisions taken while implementing [ARCHITECTURE.md](ARCHITECTURE.md). The
architecture document stays the source of truth for the big picture; this
file records the smaller choices made along the way, and the places where
the implementation deliberately does less than the architecture describes
(yet). Newest decisions are added at the end of each section.

---

## Build and toolchain

- **Rust edition 2024, MSRV 1.89.** 1.89 is needed for `std::fs::File::try_lock`
  (used for the profile lock instead of the `fs4` crate) and let-chains.
- **Workspace dependencies** are declared once in the root `Cargo.toml`.
- **TLS crypto provider: `ring`** (for both rustls and lettre), not
  `aws-lc-rs`: it builds without CMake/NASM on both x86_64 and aarch64.
- **Test machine** (zeus, Linux Mint 22.1 / Ubuntu 24.04): Qt 6.8.3 is
  installed with `aqtinstall` into `~/Qt` because the distribution ships only
  Qt 6.4. GCC 14 from the distribution. CMake and Ninja come from `uv tool`.
  The Fedora build uses the system Qt.
- **Test servers**: `testenv/compose.yaml` runs Dovecot 2.4 (IMAP, with
  every extension we use) and Mailpit (SMTP sink) on loopback. Integration
  tests are skipped unless `TERN_TEST_IMAP` / `TERN_TEST_SMTP` /
  `TERN_TEST_MAILPIT_API` are set, so `cargo test` works anywhere.

- **GitHub (public repo, `sebastka/tern`):**
  - **CI** (`ci.yaml`) on every push to main and every PR:
    - fmt, clippy `-D warnings` and all tests, including the live
      Dovecot/Mailpit and gpg tests, natively on x86_64 and arm64 runners
    - an MSRV check (`cargo check` with `rust-version`)
    - the Qt build with warnings as errors on Fedora 44 (newest Qt) and
      Debian 13 (Qt 6.8, the oldest supported)
    - `nix build`
    - cargo-deny for advisories and licenses
  - **Releases** (`release.yaml`) on `v*` tags: RPM (Fedora 44) and .deb
    (Debian 13) built natively for both architectures, plus a Nix build check,
    published as a GitHub release with SHA256SUMS. The tag must match the
    version in Cargo.toml and the spec.
- **Workflow hygiene:** every action is pinned to a full commit SHA with the
  version in a comment (`@<sha> # v7.0.1`), which Dependabot updates. x86
  jobs run on `ubuntu-latest`. arm64 jobs use `ubuntu-24.04-arm`, because
  GitHub has no "latest" label for arm64 runners. `dtolnay/rust-toolchain`
  has no releases, so it is pinned to a commit of `master` with an explicit
  `toolchain:` input.
- **Dependabot** (not Renovate): weekly grouped Cargo and Actions updates.
  It is built in and needs no third-party app. Nix isn't covered, so
  `update-flake-lock.yaml` refreshes `flake.lock` monthly via a PR (needs
  "Allow GitHub Actions to create pull requests" in the repo settings).
- **apt target is Debian 13:** Ubuntu 24.04 / Mint 22 ship Qt 6.4, below
  Tern's 6.8 floor. The .deb is assembled with `dpkg-deb` from the CMake
  install, with dependencies from `dpkg-shlibdeps`, rather than a full
  `debian/` source package. Debian 13's Rust (1.85) is below the MSRV, so the
  build uses rustup (build time only).
- **Nix:** `flake.nix` with `packaging/nix/package.nix` (Cargo deps via
  `importCargoLock`, Corrosion from nixpkgs, `wrapQtAppsHook`; gpg added to
  the wrapper's PATH).
  - It follows a **stable NixOS release** (`nixos-26.05`), not unstable:
    there are only fixes between lock updates, and it shares Qt/WebEngine
    with most NixOS systems. 26.05 has Rust 1.95 and Qt 6.11, well above
    the minimums.
  - The monthly lock update stays on that release branch; switching to the
    next release (26.11) is a manual one-line change.
- **Licenses of dependencies:** all permissive, except `cssparser` and
  `dtoa-short` (MPL-2.0, file-level copyleft, from ammonia). These are
  compatible with distributing Tern under MIT OR Apache-2.0.

## Configuration (`tern-config`)

- **Strict parsing:** every config struct uses `deny_unknown_fields`, so a typo
  is an error instead of a silently ignored setting. All problems in all
  files are reported together.
- **Account id = account file stem** (`accounts/posteo.toml` → `posteo`). It
  names the data directory, so ids are limited to `[A-Za-z0-9._-]`.
- **Password sources** are `password.command` or `password.keyring` (exactly
  one). A plaintext `password = "..."` fails to parse.
- **Keyring lookup syntax:** `password.keyring` is either `key=value` pairs
  separated by whitespace, the same as `secret-tool lookup`
  (`"service=mail account=work"`), or one bare word `x`, which is shorthand
  for `service=tern entry=x`. Locked items are unlocked through the Secret
  Service prompt.
- **Extra settings not in the architecture example:**
  - `tern.toml`: `[ui] prefer_plain_text`, `[ui] threaded`,
    `[gpg] program`, `[gpg] wkd_lookup`.
  - `profile.toml`: `display_name`, `[pgp]` (defaults for all accounts),
    `[store] compress` / `compression_level`,
    `[remote_content] allow_senders` (exact addresses or `@domain`).
  - account: `[smtp] save_to_sent` (default true), `[sync] idle_folders`,
    `[sync] poll_interval_secs` (default 300, minimum 30),
    `[sync] exclude_folders`.
  - Ports default to 993/143 (IMAP) and 465/587 (SMTP) depending on `tls`.
- **`tls = "insecure-plaintext"`** exists for local test servers only, and
  validation rejects it for any host other than localhost or a loopback IP.
- **Hot reload** watches the whole config directory with `notify`, debounced
  by 400 ms. Accounts whose configuration did not change keep running; changed
  accounts are restarted. If the new config is invalid, the old one stays
  active and the issues are sent to the UI.

## Storage (`tern-core`)

- **One SQLite file per account** (`store.sqlite`), WAL mode, one connection
  behind a mutex. Transactions are short (≤ 250 headers), so UI reads are
  never blocked for long. Schema versioning uses `PRAGMA user_version`.
- **Blob durability:** blobs are written without fsync, then one `syncfs(2)`
  per batch (25 bodies) flushes them, and only after that is the SQLite
  transaction that references them committed. This is the "fsync once per
  batch" of §8.
- **Local copies without a UID** (offline move, sent copy waiting for
  upload) have `uid = NULL`. When the server later reports a message with the
  same Message-ID in that folder, the row is *adopted* (it gets the UID)
  instead of being duplicated. COPYUID/APPENDUID set the UID directly when
  the server supports UIDPLUS.
- **`local_change` flag** on message rows: while a queued op touches a
  message, flag updates from the server are not applied to it. Server-wins
  takes over again after the op has been replayed or dropped.
- **FTS5** external-content table over subject / from / to, kept in sync by
  triggers. User input is turned into quoted prefix terms, so FTS syntax
  can't be injected.
- **Blob GC** runs 2 minutes after the profile opens and deletes unreferenced
  blobs older than 7 days.

## Sync engine and offline queue

- **Membership check:** if UIDVALIDITY, EXISTS and UIDNEXT are all
  unchanged, nothing was added or expunged and the UID listing is skipped.
  Otherwise `UID SEARCH ALL` is diffed against the local UIDs.
- **QRESYNC is not used yet:** flags use CONDSTORE `CHANGEDSINCE`, and
  expunges use the UID diff above. QRESYNC (`VANISHED`) would save the
  diff on very large folders; `async-imap` has no support for it.
- **Not used yet:** `COMPRESS=DEFLATE`, `LIST-STATUS`, `ESEARCH`.
  async-imap's deflate stream changes the session type; we will add it once
  the sync has been profiled on real mailboxes.
- **Headers first, newest first:** header batches go from the highest UID
  down, so the newest mail appears first. Bodies are downloaded after all
  folders are indexed.
- **Ops reference folders by name**, not by local id, so they survive
  folder-table changes.
- **Replay policy:** stop at the first network error so that ordering is kept.
  Any other failure is retried up to 5 times. A NO response drops the op
  immediately, and the UI is told what was given up.
- **Delete** = move to the Trash role folder. Inside Trash (or when there is
  none) it expunges with UID EXPUNGE.
- **Without MOVE or UIDPLUS** a move is UID COPY + `\Deleted`. A plain
  EXPUNGE would also remove messages that other clients have marked
  `\Deleted`, so it is not sent; the originals stay marked until something
  else expunges them. Messages flagged `\Deleted` are hidden from lists,
  counts and search. An explicit permanent delete (from Trash) on such
  a server does send a plain EXPUNGE.
- **Ops carry the folder's UIDVALIDITY** they were queued under. If it changed
  on the server before replay, the op is dropped instead of applied to the
  wrong messages (UIDs are reassigned on a reset).
- **Dropped ops are undone locally:** flag ops clear the folder's
  HIGHESTMODSEQ so the next sync fetches all flags (server wins). Moves and
  appends delete their local placeholder; the next sync brings back the
  server's copy.
- **`local_change` is a counter** of queued ops per message, so two pending
  ops on one message can't release each other's protection.
- **Known limitations:** flags changed on a message that was moved offline and
  has no UID yet are lost (server wins) once the moved copy is adopted. The
  local change and its queued op are two SQLite transactions, so a crash
  between them can leave a local-only change.
- **Threading limits:** only the last 50 References of a message are used, and
  the tree is walked iteratively, so hostile mail can neither slow nor crash
  the list.

## IMAP backend (`tern-imap`)

- **async-imap 0.12** (tokio) rather than imap-next: mature enough, and IDLE
  and CONDSTORE are supported. MOVE and APPEND are sent as raw commands so
  that the COPYUID/APPENDUID response codes can be read, because the
  library's helpers discard them.
- **Lazy connection with automatic reconnect:** any connection or protocol
  error drops the session, and the next call reconnects. Every command has a
  120 s timeout.
- **Modified UTF-7** mailbox names are converted only at the wire boundary;
  everything above uses UTF-8.
- **Folder-taking methods select the folder themselves** when needed
  (`ensure_selected`) instead of relying on the caller.
- **One IDLE connection per watched folder** (INBOX plus
  `sync.idle_folders`), separate from the "work" connection. IDLE only wakes
  the worker and is restarted every 25 minutes.

## Sending (`tern-smtp`)

- **Hand-written MIME writer** instead of `mail-builder`: PGP/MIME needs
  the exact bytes of the signed part, and all generated parts are 7-bit
  (quoted-printable text, base64 attachments) with CRLF line endings, so
  signatures survive any transport. Plain text is `format=flowed`. Headers
  use RFC 2047 and filenames RFC 2231.
- **Outbox:** `outbox/<id>.eml` plus a `<id>.json` sidecar holding the SMTP
  envelope (Bcc is only in the envelope) and the retry state. Both files are
  written atomically with fsync. Permanent failures (5xx, bad address) stay
  in the outbox, marked `failed`, and are not retried.
- **After sending:** the copy goes to the Sent role folder through the normal
  offline queue (an Append op), and the original of a reply gets
  `\Answered`.
- **Forward** attaches the original as `message/rfc822` and quotes its text
  inline.

## OpenPGP (`tern-pgp`)

- The signature files for `--verify` are written to a temp file (they aren't
  secret). Plaintext only ever goes through pipes.
- **Encrypting adds the sender's own key** as a hidden recipient, so the
  Sent copy stays readable. **Bcc recipients are hidden recipients**
  (`--hidden-recipient`), so To/Cc can't see their key ids.
- **`--trust-model always` when encrypting:** the fingerprints come from
  `resolve_recipients` (exact address match in the user's own keyring, key
  usable for encryption). Without it, gpg in batch mode refuses every key the
  user hasn't certified, which is most of them. Signature *display* still
  uses the keyring's trust (see below).
- **Recipient keys** must be usable for encryption (`E` capability, not
  expired or revoked). With `gpg.wkd_lookup = true`, `--locate-keys` is used
  instead of `--list-keys`.
- **Trust:** a signature only shows as "good" with full or ultimate trust;
  otherwise it shows as a warning that names the fingerprint.
- **Replying to an encrypted message** pre-selects encryption, even for an
  account without `[pgp]` settings (the quoted text was decrypted).
- **Inline PGP** (read only) covers only the text part, so when a message
  uses it, the HTML alternative is not shown. Otherwise a "good signature"
  badge could appear over HTML that nobody signed.
- **Known limitation:** the Subject of encrypted mail is not protected
  (no "protected headers" yet).

## Rendering policy (`tern-app::render`)

- **ammonia keeps `<style>` and `style=`:** most HTML mail is unreadable
  without them, and CSS can't run code. Remote loads from CSS are still
  stopped by the CSP and by the web view's request interceptor.
- **Every document has a CSP** (`default-src 'none'`, images/styles only from
  `tern-msg:` and `data:`, plus `http:`/`https:` when remote content is
  allowed). This is defense in depth on top of the frontend's interceptor.
- **URLs:** `tern-msg:/<account>/<message-id>/html`, `/text` and `/cid/<cid>`.
  `cid:` references are rewritten to the last form while sanitizing.
- **Plain text is also rendered as an HTML document** (escaped, links made
  clickable, quote levels colored, follows light/dark), so both views use the
  same web view and scheme.
- **Signed parts are split by hand** (RFC 2046 boundary scanning) to get the
  exact signed bytes, canonicalized to CRLF before verification.
- **Rendered messages are cached in memory** (the last 8). Attachments and
  `cid:` parts are served from this cache, so decrypted content is never
  written anywhere.

## Application facade (`tern-app`)

- **One current message list per `App`** (`open_list` + `list_rows(offset,
  count)`). The threaded view is JWZ flattened into rows with a depth, so it
  is still a windowed list. Search results use the same list (flat,
  newest first).
- **Coarse list updates for now:** changes produce `ListChanged { count }`
  (a model reset), coalesced by a 250 ms ticker. The frontend restores the
  selection by key (`list_index_of`). Fine-grained inserted/removed ranges
  are a later optimization.
- **JWZ without subject grouping:** unrelated messages that share a subject
  are not merged.
- **Account status** is reported as Connecting / Syncing / Online / Offline /
  Error. "Online" is only reported after a full sync completes. Offline
  retries back off from 30 s to 10 min. Authentication and password-command
  failures wait for a manual sync (or one hour), so `pass`/pinentry prompts
  don't repeat endlessly.
- **Passwords are resolved once per account run** and kept in memory. SMTP
  reuses the IMAP password when both use the same source.
- **Single instance:** the profile lock file plus the D-Bus name
  `fr.karlsen.Tern.p_<profile>` with a `Raise(activation_token)` method on
  `/fr/karlsen/Tern` (interface `fr.karlsen.Tern1`). The XDG activation
  token is passed along so the Wayland compositor lets the window take focus.
  The `fr.karlsen` prefix is a placeholder until the final name is chosen.
- **Logs:** daily rotated files in `$XDG_STATE_HOME/tern/logs/`, 7 kept.
  `TERN_LOG` sets the filter and `TERN_LOG_STDERR=1` mirrors to stderr. The
  last used profile is stored in `$XDG_STATE_HOME/tern/last_profile`.
- **Outbox safety:** a message is removed from the outbox as soon as SMTP has
  accepted it. Filing the Sent copy and setting `\Answered` is best effort,
  so a failure there can never cause the message to be sent twice. SMTP
  problems are reported but don't stop IMAP sync.
- **Sending is a two-step handshake:** `send()` returns a request id, and
  `DraftQueued { request, error }` tells the composer whether the message
  could be built (addresses, keys, attachments) and queued. The composer
  hides while it waits, and comes back with the error so no text is lost.
  `SendResult` later reports the SMTP outcome.
- **Postponed:** tantivy body index (§13), server-side SEARCH fallback,
  desktop notifications (open question), drafts folder sync, HTML compose,
  `mailto:` handling from other applications.

## Composing formats, signatures, archiving

- **Three editor modes** (ARCHITECTURE.md §10 updated): plain, Markdown, HTML.
  `[compose] format` in tern.toml, profile.toml or the account file; the
  most specific wins. Conversions between modes run in Rust
  (`tern-app::format`):
  - Markdown → HTML: pulldown-cmark, then ammonia.
  - HTML → Markdown: htmd.
  - HTML → text: html2text.
  - Plain → Markdown: escaping plus hard breaks.
  - Plain → HTML: escaping, links, quote levels as nested blockquotes.
  Going from HTML to plain text asks first, since formatting is lost.
- **Markdown is sent as multipart/alternative** with the Markdown source as
  the text/plain part (readable as is) and the rendered HTML.
- **Departures from strict CommonMark**, because this is email:
  - A line break is a line break (soft breaks become `<br>`), as in GitHub
    comments.
  - Bare URLs become links (not inside links or code).
  - A `-- ` line stays a signature separator; CommonMark would turn the line
    above it into a heading.
- **HTML mode** uses `QTextEdit`. Its HTML is cleaned in Rust before sending:
  inline styles are kept; scripts, `<style>` blocks and non-http(s)/data
  URLs are removed. The HTML part is a complete document with a small
  stylesheet.
- **Signatures:** `[compose] signature = "<file>"`, a path relative to
  `~/.config/tern/` (`~/` is allowed) and at most 64 KiB, in profile.toml or
  the account file. The extension decides the type (.txt, .md, .html), and
  the content is converted to the editor's mode. The file is read each time a
  message is started, so edits apply immediately.
  - Placement: below the space for your text and above the quote in replies,
    with the `-- ` separator. In HTML it is a `<div class="tern-signature">`.
  - Changing the From account of an untouched draft replaces body, mode and
    PGP defaults with that account's.
- **Archive** (`archive = "Archive/{year}"`, account or profile):
  - `{year}`/`{month}` come from each message's date (local time).
  - `/` is replaced with the server's delimiter.
  - Missing folders are created offline-first: a local folder row plus a
    queued `CreateFolder` op that runs before the queued moves.
  - CREATE on an existing folder counts as success, and new folders are
    subscribed (best effort).
  - Without a pattern the Archive action is disabled. The SPECIAL-USE
    `\Archive` folder is no longer used for this.
- **Memory limits (`[memory]` in tern.toml):**
  - `message_cache_mb` (default 64, 1–4096) bounds the rendered-message cache
    by approximate size: documents, text, inline parts and attachments. It
    used to hold the last 8 messages regardless of size. The message on
    screen and the one just rendered are never evicted. The limit is read on
    each insert, so changes apply without a restart.
  - `spare_renderer` (default true) maps to Chromium's
    `SpareRendererForSitePerProcess` feature (`--disable-features` when
    false), read through `startup_settings()` before Qt starts. It's on/off
    because Chromium keeps at most one spare renderer.
  - Measured with Qt 6.8 (zeus): QtWebEngine starts no spare renderer, so the
    setting makes no difference there. Boreas (Qt 6.11) showed 2 renderers;
    the effect there is still to be measured.
- **Opening attachments** writes them read-only to
  `$XDG_RUNTIME_DIR/tern/opened-<pid>/<unique>/<name>`, which is
  per-user, mode 0700 and tmpfs (memory), and opens them with the OpenURI
  portal's `OpenFile`, falling back to `QDesktopServices`. The directory is
  removed when Tern exits. This is the one place where decrypted content can
  reach a file system, and only a memory-backed one, at the user's request.

## Qt frontend

- **Bridge:** `tern-ffi` declares an abstract C++ `EventSink`. Its `const`
  methods run on Rust threads and only queue a signal emission on the GUI
  thread (`QMetaObject::invokeMethod`, `Qt::QueuedConnection`). Large
  payloads (`MessageView`, `Draft`) travel as `std::shared_ptr`. The event
  bridge object outlives the core by construction (`main.cpp`).
- **Generated headers:** `build.rs` copies the cxx headers to
  `${CMAKE_BINARY_DIR}/cxxbridge` (`TERN_FFI_INCLUDE_DIR`, set through
  Corrosion). A custom target that runs after `cargo-build_tern_ffi` declares
  them as BYPRODUCTS, so Ninja knows they change during the build and
  recompiles every source that includes them in the same run.
  - Why: without this, unchanged C++ files kept old struct layouts. The
    linker then picked a stale inline copy constructor of `Draft`, which
    crashed the composer. This was found and fixed while adding the
    `format` field.
- **C++23 without modules**, built with `-Wall -Wextra -Wpedantic`,
  `QT_NO_CAST_FROM_ASCII` and `QT_NO_KEYWORDS`.
- **Message list:** `QTreeView` over a flat `QAbstractTableModel` that reads
  pages of 128 rows on demand (LRU of 32 pages). Threads are shown as
  indentation with ↳. After a refresh the current message stays selected
  (by key); if it is gone, the row that took its place is selected.
- **`tern-msg:` is not registered as a local scheme:** local pages could never
  load remote content, but remote content must be allowed per message. The
  CSP in each document and the request interceptor do the blocking, and the
  interceptor only lets `http(s)` through for subresources of the current,
  allowed message.
- **Links** open through the OpenURI portal (asynchronous D-Bus call),
  falling back to `QDesktopServices`. `mailto:` links open the composer.
- **UI state** (geometry, splitters, columns, threaded, last folder) is stored
  in `$XDG_STATE_HOME/tern/qt-<profile>.ini`, never in the config directory.
- **Window title** is only the profile name: desktops append the application
  name themselves ("private — Tern").
- **Raising from another instance** sets `XDG_ACTIVATION_TOKEN` from the D-Bus
  call before activating the window. How much focus a Wayland compositor
  then grants is up to it.
- **No Widevine probe:** `main()` adds `--widevine-path=/nonexistent/…` to
  `QTWEBENGINE_CHROMIUM_FLAGS`, keeping any flags the user set. Fedora ships a
  placeholder Widevine file that QtWebEngine otherwise tries, and fails, to
  load at every start. Tern never plays DRM content. Confirmed on boreas.
- **Known harmless message:** "This plugin supports grabbing the mouse only for
  popup windows" comes from Qt's Wayland plugin when QtWebEngine grabs the
  mouse on a click. It is left alone: silencing it would hide that whole
  category of Qt Wayland warnings.
- **Missing icon themes** (bare Hyprland, Xvfb): the message list falls back
  to Unicode glyphs (★ ● ↩ 📎 🔒) when the theme has no mail icons.
- **Single-letter shortcuts** (A, M, S, T) only work while the message list
  has focus, so typing in the search field isn't hijacked.
- **No mailto: handler in the desktop file yet:** forwarding a `mailto:` to an
  already running instance needs a D-Bus method; postponed.

## Verification so far

- Unit and integration tests: 60 tests. They include a live Dovecot 2.4 (IMAP
  with CONDSTORE/QRESYNC/MOVE/UIDPLUS/IDLE), Mailpit (SMTP), real gpg
  (sign/verify/encrypt/decrypt, PGP/MIME round trip, tamper detection), and
  an end-to-end run through `tern-app`.
- The same test suite passes for **aarch64** (cross-compiled with
  `aarch64-linux-gnu-g++-14`, run under qemu-user), on zeus.
- The Qt app was driven under Xvfb with xdotool: sync, threaded list, HTML
  rendering with blocked remote content and `cid:` images, attachments,
  reply/send, and single-instance raising over D-Bus.
- **Not verified yet:** the Qt app on aarch64/Asahi (16 KiB pages) and on a
  real Plasma/Hyprland session; a TLS connection that *succeeds* against a
  server with a publicly trusted certificate (implicit TLS and STARTTLS are
  tested to reject Dovecot's self-signed one); Secret Service lookups.
