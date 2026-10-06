# Tern — Architecture Decisions

> **Tern** is the working name: a desktop mail client that only speaks standard
> protocols (IMAP, SMTP, later JMAP), built for KDE Plasma and other Wayland
> compositors (Hyprland).

This document records the architecture decisions made so far. Each decision has
a short rationale. When a decision changes, update it here and leave a short note
about what replaced what.

---

## 1. Scope

| In scope | Out of scope (for now) |
|---|---|
| IMAP4rev1/rev2 (receive and sync) | Exchange/EWS, Graph, proprietary APIs |
| SMTP submission (send) | Unified inbox |
| JMAP (postponed, see §12) | Calendar, contacts, tasks |
| Multiple accounts per profile | Built-in settings editor |
| Multiple profiles, chosen at startup | Plugin system |
| Full offline operation | |
| OpenPGP through the system `gpg` | |

**Desktop-agnostic:** Tern depends only on freedesktop specifications (XDG base
directories, Secret Service, desktop notifications, XDG portals), never on
applications or services from a specific desktop such as KDE or GNOME. It must
run equally well in Plasma and in Hyprland.

## 2. Language: Rust

**Decision:** Write all application logic in Rust.

**Rationale:**
- A mail client mostly parses hostile input (MIME, headers, charsets, HTML,
  attachments). Memory safety removes a whole class of security bugs.
- The mail ecosystem is mature: `mail-parser`, `mail-builder`, `lettre`,
  `imap-next`/`async-imap` and `jmap-client`.
- Go was rejected because of its weak GUI story. C++ was rejected for the core
  because of memory safety. The Qt frontend is written in C++, but it only
  displays data and never parses mail (§3, §4).

## 3. UI: C++/Qt frontend first, GTK4/libadwaita later

**Decision:**
- **Primary frontend (now):** C++23 with **Qt 6 Widgets** (Qt 6.8 LTS or
  newer) and **QtWebEngine** for HTML mail. It is built with CMake and calls the
  Rust core through a [`cxx`](https://cxx.rs/) bridge (§4).
- **Second frontend (later):** GTK4 + libadwaita in Rust with gtk-rs, Relm4 and
  WebKitGTK 6, for when the main desktop moves to Hyprland. It uses the same
  `tern-app` API (§4).

*Changed:* this section first chose Qt through cxx-qt, then GTK4 first with Qt
later. The order is now reversed: Qt first, because Plasma is the current
desktop.

**Why Qt first:**
- Plasma is the current desktop. A Qt app gets the Breeze style, color scheme,
  fonts and portal file dialogs automatically.
- Qt 6 runs natively on Wayland. Under Hyprland it can be themed with `qt6ct`
  (`QT_QPA_PLATFORMTHEME=qt6ct`), so it stays usable after the move.

**Why Qt Widgets rather than QML (Qt Quick):**
- A three-pane desktop mail client is the classic case for Widgets. `QTreeView`
  and `QTableView` are mature, fast with large virtualized models, and handle
  keyboard navigation, column resizing and sorting out of the box.
- `QTextEdit` is a mature compose editor.
- Widgets pick up the Breeze *style plugin* in Plasma when it is installed and
  fall back to Fusion elsewhere, without depending on any KDE library. A native
  look in QML needs `qqc2-desktop-style` from KDE Frameworks, and Kirigami is
  itself a KDE Framework. Both would break the "freedesktop only" rule (§1).
- Prior art: Trojitá, a Qt Widgets IMAP client.

**Why C++23:**
- Useful features: `std::expected` for error handling at the Rust boundary
  without exceptions, `std::ranges::to`, deducing `this`, `std::print`/`std::format`
  for non-Qt code paths, and `std::flat_map`.
- No conflicts: Qt 6 itself only requires C++17 and builds fine in C++23 mode,
  and the C++ code that `cxx` generates works with any newer standard.
- Toolchain floor: **GCC 14+ or Clang 18+** (the development host has GCC 16).
  CMake sets `CMAKE_CXX_STANDARD 23`, `CMAKE_CXX_STANDARD_REQUIRED ON` and
  `CMAKE_CXX_EXTENSIONS OFF`.
- **Not used:** C++ modules (`import std`). CMake support for them is still
  experimental, and they don't work together with moc and Qt's headers yet.
  We'll revisit once that settles.

**Why `cxx` and not cxx-qt:** `cxx` only bridges plain C++ and Rust types and
knows nothing about Qt. The Qt side is an ordinary C++ project with no Rust
build magic. All Qt-specific glue (models, signals) is hand-written C++ on top
of a small, stable interface. That keeps the fragile part small and testable.

**Rejected:**
- **cxx-qt:** too fragile. Its API still changes between versions, exposing
  Rust data to QML takes a lot of boilerplate, and the cargo + Qt + C++ build
  is complex.
- **Slint, iced:** no web view for HTML mail.
- **Kirigami/QML:** depends on KDE Frameworks (see above).

**Consequences:**
- C++ code is limited to presentation. **It never parses mail**: MIME parsing,
  HTML sanitizing, charset decoding and PGP all happen in Rust (§2).
- Runtime needs: Qt 6 Base, Widgets and WebEngine. Breeze is used if present
  and is never required.

## 4. Crate layout and frontend boundary

```
tern/
├─ CMakeLists.txt     # top-level build: Qt app + Rust via Corrosion
├─ Cargo.toml         # Rust workspace
├─ crates/
│  ├─ tern-core/      # domain model, storage, sync engine
│  ├─ tern-imap/      # IMAP backend (implements MailBackend)
│  ├─ tern-smtp/      # SMTP submission + outbox
│  ├─ tern-pgp/       # gpg subprocess wrapper
│  ├─ tern-config/    # config file parsing and validation
│  ├─ tern-app/       # UI-agnostic facade: commands, events, view models
│  ├─ tern-ffi/       # cxx bridge over tern-app (staticlib for C++)
│  └─ tern-gtk/       # (later) GTK4/libadwaita/Relm4 frontend (binary: `tern-gtk`)
├─ frontends/
│  └─ qt/             # C++/Qt Widgets app (binary: `tern`)
└─ ARCHITECTURE.md
```

**Frontend rule:** frontends talk **only to `tern-app`** (the Qt frontend
through `tern-ffi`), never to `tern-core` or the backends directly. With two
frontends planned, this keeps the second one cheap: it wraps the same API.

`tern-app` API design. These rules keep it easy to bridge to C++:
- **Commands in, events out.** The frontend sends commands (`OpenProfile`,
  `SelectFolder`, `SetFlags`, `MoveMessages`, `Send`, and so on) and receives a
  stream of events (`FolderTreeChanged`, `MessagesChanged`, `SyncProgress`,
  `Error`, and so on). The frontend never sees futures, async traits or tokio.
- **Plain data types.** Commands, events and view models use simple owned
  structs and enums: strings, integers, vectors, IDs. No lifetimes, generics or
  trait objects cross the boundary. `cxx` maps these directly to C++ structs.
- **Windowed list models.** Message lists are read by index range
  (`rows(folder, offset, count)`) and announced with change events (inserted,
  removed, updated ranges). This maps directly onto Qt's
  `QAbstractItemModel`/`QAbstractTableModel` (with `canFetchMore`/`fetchMore`)
  and later onto GTK's `gio::ListModel`.
- **Shared rendering policy.** `tern-app` decides what is shown: sanitized HTML,
  whether remote content is allowed, the plain-text alternative and
  decryption/signature status. Each frontend's web view only enforces it (§10),
  so both frontends behave the same.
- **Threading:** `tern-app` owns a tokio runtime on its own threads. Events are
  delivered through a callback that the frontend registers.
  - **Qt:** the callback only posts the event to the GUI thread with
    `QMetaObject::invokeMethod(..., Qt::QueuedConnection)`. Qt objects are never
    touched from Rust threads.
  - **GTK (later):** the frontend drains a channel on the `glib` main loop.

**Build:**
- CMake is the top-level build. [Corrosion](https://github.com/corrosion-rs/corrosion)
  builds `tern-ffi` as a static library and links it into the Qt binary.
- `cargo build`/`cargo test` still work on their own for all Rust crates, so the
  core can be developed and tested without Qt.
- Both frontends read the same config and data. The per-profile lock (§6) keeps
  them from opening the same profile at the same time.

**Other core decisions:**
- **Async runtime:** `tokio`, inside `tern-app` (see above).
- **TLS:** `rustls` with the system certificate store (`rustls-native-certs`). No OpenSSL.
- **Logging:** `tracing`. Logs go to files under `$XDG_STATE_HOME` (see §7).
- **Backend abstraction:** a `MailBackend` trait (list folders, sync folder,
  fetch, store flags, move, append). IMAP implements it first and JMAP later.

## 5. Configuration: files only

**Decision:** Tern is configured only through TOML files and **never writes to
them**. The UI may show the configuration but has no settings editor.

**Rationale:** Configuration can be versioned and kept in dotfiles, and it can be
reproduced. The app never rewrites a file behind the user's back.

**Details:**
- **Location:** `$XDG_CONFIG_HOME/tern/` (defaults to `~/.config/tern/`).
- **Hot reload:** the app watches the files with the `notify` crate. Validation
  errors appear in the UI and the last valid config stays active.
- **Secrets:** plaintext passwords are **not allowed** in config files. Two
  sources are supported:
  - `password.command = "pass show mail/work"`, the first line of stdout
  - `password.keyring = "<entry>"`, through the freedesktop Secret Service
    D-Bus API. Any provider works: KeePassXC, gnome-keyring, oo7 or KWallet.
    Tern depends on the spec, never on a specific provider.

Layout:
```
~/.config/tern/
├─ tern.toml                     # global: default_profile, ask_on_startup, ui prefs
└─ profiles/
   ├─ personal/
   │  ├─ profile.toml            # profile-level settings (identity defaults, gpg key)
   │  └─ accounts/
   │     ├─ fastmail.toml
   │     └─ posteo.toml
   └─ work/
      ├─ profile.toml
      └─ accounts/
         └─ corp.toml
```

Example account file:
```toml
# ~/.config/tern/profiles/personal/accounts/posteo.toml
name = "Posteo"
email = "me@posteo.de"
display_name = "Sebastian"

[imap]
host = "posteo.de"
port = 993
tls = "implicit"          # implicit | starttls
username = "me@posteo.de"
password.command = "pass show mail/posteo"

[smtp]
host = "posteo.de"
port = 465
tls = "implicit"
username = "me@posteo.de"
password.command = "pass show mail/posteo"

[pgp]
key = "0xDEADBEEFCAFEBABE"
sign_by_default = false
encrypt_when_possible = true
```

## 6. Profiles

**Decision:** A profile is a fully isolated set of accounts, data, cache and
state. When more than one profile exists, a profile picker appears at startup.

- `tern --profile <name>` skips the picker.
- `ask_on_startup = false` together with `default_profile` in `tern.toml` also
  skips it.
- **Single instance per profile:** a lock file at
  `$XDG_RUNTIME_DIR/tern/<profile>.lock`. Starting `tern` a second time on the
  same profile raises the existing window (over D-Bus) instead of opening a new one.
- Different profiles can run at the same time as separate processes.

## 7. Storage: XDG layout

**Decision:** Each kind of data goes to the XDG base directory that matches its
nature.

| Data | Location | Why |
|---|---|---|
| Config | `$XDG_CONFIG_HOME/tern/` | User-authored settings |
| **Mail store** (messages, metadata, drafts, outbox) | `$XDG_DATA_HOME/tern/profiles/<p>/` | Irreplaceable user data: unsent mail and offline changes |
| UI state, logs, last-used profile | `$XDG_STATE_HOME/tern/` | Persistent but unimportant, as the spec defines it |
| Search index, rendered HTML, remote-image cache | `$XDG_CACHE_HOME/tern/profiles/<p>/` | Can be regenerated at any time |
| Locks, IPC sockets | `$XDG_RUNTIME_DIR/tern/` | Lives only for the session |

> Why not `$XDG_STATE_HOME` for mail? The spec reserves STATE for data that is
> "not important or portable enough" for DATA, such as history and logs. A full
> offline mailbox with pending drafts and an outbox is important user data, so
> it belongs in DATA.

Mail store layout:
```
~/.local/share/tern/profiles/<profile>/
└─ <account-id>/
   ├─ store.sqlite              # folders, message index, flags, sync state
   ├─ blobs/
   │  └─ 3f/
   │     └─ 3fa9…c21.eml.zst    # raw RFC 5322 bytes, zstd-compressed
   └─ outbox/                   # queued outgoing messages (raw .eml)
```

## 8. Message storage format

**Decision:** Store each message **exactly as received** (raw RFC 5322 bytes) in
an immutable blob file compressed with **zstd**. Keep all mutable metadata in
SQLite.

- **Blob naming:** the BLAKE3 hash of the *uncompressed* bytes. When the same
  message appears in several folders (for example Gmail labels or copies), it
  is stored once.
- **Compression:** zstd level 3 by default. It can be turned off per profile
  (`store.compress = false`), in which case files are written as plain `.eml`.
  The reader handles both.
- **Immutability:** IMAP messages never change. Only flags and folder
  membership do, and those live in SQLite. Blobs are written once to a temp file
  and atomically renamed into place, then never modified.
- **SQLite (`rusqlite`, WAL mode)** holds:
  - folders: hierarchy, delimiter, SPECIAL-USE role, UIDVALIDITY,
    HIGHESTMODSEQ, UIDNEXT
  - messages: `(folder_id, uid) → blob_hash`, flags, modseq, size, and parsed
    envelope fields (date, from, to, subject, message-id, references) for fast
    lists
  - pending offline operations (the queue described in §9)
- **Garbage collection:** a blob that no folder references any more is deleted
  after a grace period.
- **Not Maildir:** compression and deduplication don't fit Maildir. A
  `tern export --maildir` or `--mbox` command can come later for interoperability.

### Filesystem considerations

- **Inodes:** one file per message is not a practical risk for a single user.
  Btrfs (the development host's `/home`) allocates inodes dynamically. ext4 sets
  them when the filesystem is created, by default one per 16 KiB: a 256 GB disk
  gets about 15 million, so a 500,000-message store uses about 3%.
- **Small-file overhead:** Btrfs stores files under about 2 KiB inside its
  metadata. On ext4, each file uses at least one 4 KiB block. Backups (rsync,
  restic) slow down with very large file counts.
- **Write cost:** during the initial full sync, blobs are written in batches and
  forced to disk (fsync) once per batch, not once per message.
- **Double compression:** Fedora mounts Btrfs with `compress=zstd:1`. Tern's own
  zstd (level 3) still stays on by default because it compresses better, works
  on any filesystem and makes backups smaller. Btrfs detects data that is
  already compressed and stores it as is, so nothing is wasted.
- **Possible change later (hybrid store):** messages under about 64 KiB could be
  stored as raw bytes in SQLite, which reads small blobs faster than separate
  files, with only larger messages kept as blob files. All storage access goes
  through one storage layer in `tern-core`, so this could be added without
  changing the rest of the app. It is not planned for now.

## 9. Sync engine: full offline

**Decision:** Tern is local-first. The UI reads only from the local store. Every
user action is applied locally first, put into a queue and replayed against the
server.

- **IMAP extensions** we use when the server offers them: `IDLE`, `CONDSTORE`,
  `QRESYNC`, `UIDPLUS`, `MOVE`, `SPECIAL-USE`, `LIST-STATUS`, `ESEARCH`,
  `COMPRESS=DEFLATE`. We fall back gracefully on servers without them.
- **Sync strategy per folder:**
  1. Check UIDVALIDITY. If it changed, drop the folder's index and resync.
  2. Use QRESYNC/CONDSTORE for flag changes and expunges. Without them, fall
     back to UID diffing.
  3. Fetch headers and envelopes first, then bodies in the background.
     Everything is downloaded (full offline).
- **IDLE** runs on the INBOX of each account, plus optional extra folders. The
  other folders are polled on an interval set in the config.
- **Offline operation queue:** flag changes, moves, deletes and appends are
  stored in SQLite and replayed when the account is online again. Conflicts are
  resolved with server-wins for flags and local-wins for moves the user
  explicitly made.
- **Outbox:** outgoing mail goes to `outbox/` first and is sent through SMTP
  when online. After a successful send it is appended to the Sent folder,
  unless the server already saves sent mail there (configurable).

## 10. UI structure

- Three panes: **folder tree** (one root node per account, with real IMAP
  subfolders as children) → **message list** (threaded or flat, toggle) →
  **message view**.
- There is no unified inbox. Accounts stay separate in the tree.
- **Threading** uses the JWZ algorithm (References/In-Reply-To), computed per folder.
- **HTML mail**, the common policy decided in `tern-app` for both frontends:
  - HTML sanitized with `ammonia` in Rust before any web view sees it
  - JavaScript disabled
  - remote content (`http(s)`) blocked by default. It can be allowed per
    message, or permanently per sender through an allow-list in the config.
  - content served through a custom URL scheme (`tern-msg:`), never from
    `file://`
  - clicked links open in the default browser through the OpenURI portal,
    never inside the web view
  - plain-text view always available, with an option to prefer it
- **Qt (QtWebEngine)** enforces it with:
  - an off-the-record `QWebEngineProfile`, so cookies, cache and storage are
    never written to disk
  - `JavascriptEnabled = false`, `LocalContentCanAccessRemoteUrls = false`
  - a `QWebEngineUrlRequestInterceptor` that blocks every `http(s)` request not
    allowed by the policy
  - a `QWebEngineUrlSchemeHandler` for `tern-msg:`
  - an overridden `QWebEnginePage::acceptNavigationRequest` that passes links to
    `QDesktopServices::openUrl`
  - Chromium's sandbox left enabled
- **GTK (WebKitGTK 6, later)** enforces it with `enable-javascript = false`, a
  `UserContentFilterStore` rule blocking `http(s)`, a registered URI scheme,
  and the `decide-policy` signal for navigation. WebKit's bubblewrap sandbox
  stays on.
- **Compose:** three editor modes, switchable while writing, default set in
  the config: plain text (`format=flowed`), Markdown (sent as
  multipart/alternative: the Markdown source as text/plain plus the rendered
  HTML) and HTML (a rich-text editor; a text version is generated). Message
  building, format conversion and HTML cleaning happen in Rust.
  *Changed:* this first said "plain text first, HTML compose postponed".

## 11. OpenPGP: native gpg

**Decision:** Drive the **system `gpg` binary** directly as a subprocess. Don't
link GPGME or Sequoia.

**Rationale:** It reuses the user's existing keyring, `gpg-agent`,
smartcards/YubiKeys and whichever pinentry the user configured, without
duplicating any of that.

- **Invocation:** always `--batch --no-tty --status-fd 2 --with-colons`, and
  parse the machine-readable status lines (`GOODSIG`, `VALIDSIG`,
  `DECRYPTION_OKAY`, and so on). Never parse human-readable output.
- **gpg binary:** configurable as `gpg.program`, default `gpg`.
- **Formats:**
  - Send: PGP/MIME (RFC 3156), signed and/or encrypted.
  - Read: PGP/MIME, plus legacy inline PGP (read only).
- **Data at rest:** encrypted messages are stored **encrypted**, as received.
  Decrypted content stays in memory and is never written to disk or to the
  search index.
- **Key lookup:** recipient keys come from the local keyring. WKD lookup
  (`gpg --locate-keys`) is optional. Autocrypt is postponed.
- **S/MIME:** postponed. If we add it later, it would go through `gpgsm` in the
  same way.

## 12. JMAP: postponed

**Decision:** Postpone JMAP until we have a test server. Keep the
`MailBackend` trait shaped so JMAP fits in later.

- The local store must not assume IMAP semantics everywhere. For example, a
  message can belong to several mailboxes in JMAP, which the content-hash blob
  design (§8) already supports.
- **Test server:** a local Stalwart container (Docker) when we get to it.

## 13. Search

- **Fast path:** SQLite FTS5 over envelope fields (from, to, subject).
- **Full-text bodies:** `tantivy` index in `$XDG_CACHE_HOME`, built
  incrementally. Since it can be regenerated, deleting the cache is always safe.
- Server-side IMAP `SEARCH` as a fallback for folders not yet fully synced.

## 14. Packaging

- **Primary:** a native CMake build (which also builds the Rust crates) and an RPM for Fedora (the main
  development host is Fedora Asahi on aarch64).
- **Flatpak:** postponed. Running the host `gpg` from inside the sandbox
  conflicts with §11 and needs `flatpak-spawn --host` or a portal-based solution.

---

## Open questions

- [ ] Final project name, then check crates.io, Flathub and GitHub for conflicts.
- [x] License: MIT OR Apache-2.0 (Rust convention; lets the crates be reused). All
      dependencies are permissive, and Qt is LGPLv3 (dynamically linked).
- [ ] OAuth2 (Gmail, Microsoft 365): when, and how to configure it in files only.
- [x] Notifications: the freedesktop notification spec over D-Bus, sent from
      `tern-app` (shared by both frontends). Configured per key in tern.toml,
      profile.toml or an account file (`[notifications]`), with a folder list
      per account (default INBOX). The sound comes from the freedesktop sound
      theme (`message-new-email`) and is played by the frontend (libcanberra
      in Qt), because many notification servers don't play sounds.
- [ ] Sieve (ManageSieve) for server-side filters?
