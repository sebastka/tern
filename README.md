# Tern

[![CI](https://github.com/sebastka/tern/actions/workflows/ci.yaml/badge.svg)](https://github.com/sebastka/tern/actions/workflows/ci.yaml)

> [!NOTE]
> This project was created with [Anthropic Claude Opus 5.5](https://www.anthropic.com/claude).

A desktop mail client that only speaks standard protocols (IMAP, SMTP), keeps
a full offline copy of your mail, is configured through TOML files only, and
uses the system `gpg` for OpenPGP. The core is Rust; the first frontend is
C++/Qt 6 Widgets.

- Design: [ARCHITECTURE.md](ARCHITECTURE.md)
- Implementation decisions and known limitations: [DECISIONS.md](DECISIONS.md)

Licensed under either of [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE),
at your option.

## Installing

Packages for x86_64 and aarch64 are attached to each
[release](https://github.com/sebastka/tern/releases). Tern needs Qt 6.8 or
newer.

- **Fedora 44+:** `sudo dnf install ./tern-<version>-<release>.fc44.<arch>.rpm`
- **Debian 13 "trixie"+, Ubuntu 25.04+:** `sudo apt install ./tern_<version>-1_<arch>.deb`
  (not Ubuntu 24.04 / Linux Mint 22: their Qt 6.4 is too old)
- **Nix:** `nix profile install github:sebastka/tern`, or run it once with
  `nix run github:sebastka/tern`

Each release has a `SHA256SUMS` file to check the downloads.

## Building

Requirements: Rust ≥ 1.89, CMake ≥ 3.24, Ninja, GCC ≥ 14 or Clang ≥ 18,
Qt ≥ 6.8 with QtWebEngine, and `gpg` at runtime.
[Corrosion](https://github.com/corrosion-rs/corrosion) is used if installed,
otherwise it is fetched at configure time.

```sh
cmake -B build -G Ninja -DCMAKE_BUILD_TYPE=RelWithDebInfo
cmake --build build
./build/frontends/qt/tern            # or: tern --profile work
```

On Fedora:

```sh
sudo dnf install cmake ninja-build gcc-c++ cargo rust corrosion \
    qt6-qtbase-devel qt6-qtwebengine-devel
```

The Rust crates also build and test on their own: `cargo test --workspace`.

The packages are built by [.github/workflows/release.yaml](.github/workflows/release.yaml)
when a `v<version>` tag is pushed. Locally:

- `.deb`: `packaging/build-deb.sh` on Debian 13 (or in a `debian:trixie` container)
- Nix: `nix build .#tern`; `nix develop` gives a shell with all build tools

An RPM spec is in [packaging/tern.spec](packaging/tern.spec).
`packaging/build-rpm.sh [x86_64|aarch64]` builds it in a Fedora 44 container
(aarch64 is emulated on x86_64 hosts and needs `qemu-user-static`); the RPMs
land in `packaging/out/`. To build directly, create the sources with
`packaging/make-sources.sh` and run `rpmbuild -ba packaging/tern.spec` with
the tarballs in `~/rpmbuild/SOURCES`.

## Configuration

Tern never writes its configuration. Everything is in
`$XDG_CONFIG_HOME/tern/` (normally `~/.config/tern/`), and changes are picked
up while Tern runs. If a file has an error, Tern shows it and keeps the last
valid configuration.

```
~/.config/tern/
├─ tern.toml                       # optional, global settings
└─ profiles/
   └─ personal/                    # one directory per profile
      ├─ profile.toml              # optional, profile settings
      └─ accounts/
         └─ posteo.toml            # one file per account; "posteo" is its id
```

### Account (`profiles/<profile>/accounts/<id>.toml`)

```toml
name = "Posteo"
email = "me@posteo.de"
display_name = "Sebastian"          # optional, defaults to profile.toml

[imap]
host = "posteo.de"
port = 993                          # optional: 993 implicit / 143 starttls
tls = "implicit"                    # implicit | starttls | insecure-plaintext (see below)
username = "me@posteo.de"
password.command = "pass show mail/posteo"

[smtp]
host = "posteo.de"
port = 465                          # optional: 465 implicit / 587 starttls
tls = "implicit"
username = "me@posteo.de"
password.keyring = "service=mail account=posteo"
save_to_sent = true                 # false if the server files sent mail itself (Gmail)

[pgp]                               # optional, overrides profile.toml
key = "0xDEADBEEFCAFEBABE"            # 16-digit long key id or 40-digit fingerprint, no spaces
sign_by_default = false
encrypt_when_possible = true

[sync]                              # optional
poll_interval_secs = 300            # folders without IDLE; at least 30
idle_folders = ["Lists/work"]       # IDLE on these as well as INBOX
exclude_folders = ["Spam"]          # never synced

[compose]                           # optional, overrides profile.toml / tern.toml
format = "markdown"                 # plain | markdown | html
signature = "signatures/posteo.md"  # .txt, .md or .html, relative to ~/.config/tern/; max 64 KiB
```

`tls = "insecure-plaintext"` turns TLS off completely. It is meant for local
test servers only and is rejected unless `host` is `localhost` or a loopback
address.

`archive` is a top-level key (put it above `[imap]`): the folder the Archive
action moves messages to. `{year}` and `{month}` are taken from each message's
date, and `/` separates levels (Tern uses the server's own separator).
Missing folders are created. Without it, the Archive action is disabled for
that account:

```toml
archive = "Archive/{year}"
```

`sent_folder` (also top-level) is where copies of sent mail go, with `/`
between levels. Missing folders are created. Without it, Tern uses the folder
the server marks as Sent. It can also be set in profile.toml or tern.toml; the
most specific setting wins:

```toml
sent_folder = "INBOX/Sent Items"
```

Plaintext passwords are not accepted. Use one of these:

- `password.command`: a shell command; Tern uses the first line it prints.
- `password.keyring`: a Secret Service lookup (KeePassXC, GNOME Keyring,
  KWallet, oo7…). Give `key=value` attributes as for `secret-tool lookup`,
  or one word `x`, which means `service=tern entry=x`.

`pgp.key` is the account's own key, as a 16-digit long key id
(`gpg --list-keys --keyid-format long`) or the 40-digit fingerprint, with or
without `0x`. Short 8-digit ids are refused because they are easy to forge.
When a profile opens and whenever the configuration changes, Tern checks each
key in the gpg keyring and lists problems under *Configuration problems*: key
missing or ambiguous, expired or revoked, no user id for the account's
address (a key copied from another account), no encryption subkey, no
reachable secret key for decrypting (keys on a smartcard count as reachable),
and no secret signing key when `sign_by_default = true`. Encrypted mail also
goes to your own key, so you can read your sent copies.

### Profile (`profiles/<profile>/profile.toml`, optional)

```toml
display_name = "Sebastian"
account_order = ["work", "posteo"]  # account file names; unlisted accounts follow, A–Z

[pgp]                               # default for all accounts of the profile
key = "0xDEADBEEFCAFEBABE"
sign_by_default = true

[store]
compress = true                     # zstd-compress stored messages
compression_level = 3               # 1–19

[remote_content]
allow_senders = ["news@lwn.net", "@example.org"]   # load remote images for these

[compose]                           # defaults for all accounts of the profile
format = "plain"
signature = "signatures/me.txt"
```

`archive = "Archive/{year}"` and `sent_folder` can also be set here
(top-level) as defaults for all accounts.

`account_order` sets the order of accounts in the folder tree. The first
account is also the default for New Message when no folder is selected.

### Global (`tern.toml`, optional)

```toml
default_profile = "personal"
ask_on_startup = false              # with default_profile: skip the profile picker

sent_folder = "Sent"                # default for all profiles (top-level, above any [table])

[ui]
threaded = true
prefer_plain_text = false

[ui.message_list]
# Left to right: flag, subject, from, to, correspondent, date, attachment, size.
# "correspondent" is From, or To in Sent and Drafts folders.
columns = ["flag", "subject", "correspondent", "date", "attachment"]   # not empty, no duplicates
sort_by = "date"                    # any of the above; it doesn't have to be shown
sort_order = "desc"                 # asc | desc

[gpg]
program = "gpg"
wkd_lookup = false                  # look up missing recipient keys via WKD

[compose]
format = "plain"                    # default editor: plain | markdown | html (no signature here)

[memory]
message_cache_mb = 64               # rendered messages kept in memory, attachments included; 1–4096
spare_renderer = true               # keep a spare web renderer ready (newer Qt only; restart to apply)

[notifications]                     # also in profile.toml or an account file; most specific wins, per key
enabled = true                      # desktop notification for new unread mail
sound = true                        # play the sound theme's "message-new-email" sound
folders = ["INBOX"]                 # folders that notify; patterns, e.g. ["INBOX", "Lists/*"] or ["*"]
exclude_folders = []                # folders that never notify (same patterns); they win over `folders`
```

`folders` and `exclude_folders` take folder patterns. Write `/` between
levels whatever the server uses (Tern converts it to the server's separator,
e.g. `.`):

| Pattern | Matches | Doesn't match |
|---|---|---|
| `"*"` | every folder | |
| `"Lists/*"` | `Lists`, `Lists/rust`, `Lists/rust/dev` | `Listserv`, `Archive/Lists` |
| `"Lists/rust"` | exactly `Lists/rust` | `Lists`, `Lists/rust-dev`, `Lists/rust/dev` |
| `"INBOX"`, `"inbox"` | the inbox, in any case | |
| `"INBOX/*"` | the inbox and folders nested under it (`INBOX.Archive` on servers that nest) | |

`*` only works on its own or as a final `/*`. Patterns like `"Lists*"`,
`"*/rust"` or `"Lists/*/old"` are reported as configuration errors. Except
for `INBOX`, names are case-sensitive, as on the server.

To be notified everywhere except in a few folders (opt-out):

```toml
[notifications]
folders = ["*"]
exclude_folders = ["Junk", "Trash", "Lists/*"]
```

Each key is taken from the most specific file that sets it, and lists are
not merged: an account's `exclude_folders` replaces the one in tern.toml.

New mail is announced through the freedesktop notification service, so it
works on any desktop (Plasma, GNOME, or Hyprland with mako, dunst, swaync…).
Clicking a notification opens the message. Only unread messages that arrive
after a folder's first sync count, so the initial download stays quiet. The
sound is played by Tern itself (through libcanberra), from the freedesktop
sound theme: `message-new-email`, falling back to the theme's general
`message` sound. `enabled` and `sound` are independent: either can be off.

In threaded view, `sort_by` orders whole threads. Numeric fields (date, size,
flag, attachment) use the thread's highest value, so `date` means the newest
message and `flag` lifts a thread with any flagged or unread message. Text
fields use the thread's first message. Messages inside a thread stay in date
order. Subjects sort without their `Re:`/`Fwd:`/`SV:` prefixes.

### Composing

The composer has three modes, switchable at any time (the text is converted):

- **Plain text**, sent as `format=flowed` text.
- **Markdown**: you write CommonMark; the message carries your Markdown as
  its text version and the rendered HTML next to it. Line breaks are kept
  and bare URLs become links, as in GitHub comments. *Preview* shows the result.
- **HTML**: a rich-text editor (bold, italic, underline, strikethrough, lists,
  links). A plain-text version is generated alongside.

Signatures are inserted below your text (above the quote in replies),
converted to the editor's mode. Changing the From account of an untouched
new message switches to that account's signature and mode.

## Where data goes

| What | Where |
|---|---|
| Mail, index, outbox | `~/.local/share/tern/profiles/<profile>/<account>/` |
| Logs, window layout, last profile | `~/.local/state/tern/` |
| Lock files | `$XDG_RUNTIME_DIR/tern/` |

Set `TERN_LOG=debug` for verbose logs, and `TERN_LOG_STDERR=1` to also log to
the terminal.

## Keyboard

| Key | Action |
|---|---|
| F5 | Get mail |
| Ctrl+N | New message |
| Ctrl+R / Ctrl+Shift+R / Ctrl+L | Reply / Reply all / Forward |
| Ctrl+Return | Send (in the composer) |
| Ctrl+Shift+P | Markdown preview (in the composer) |
| Ctrl+B / Ctrl+I / Ctrl+U / Ctrl+K | Bold / italic / underline / link (HTML mode) |
| Delete | Move to Trash (deletes for good inside Trash) |
| A | Archive (needs `archive` in the config) |
| M / Shift+M | Mark read / unread |
| S | Toggle flag |
| T | Threaded / flat list |
| Ctrl+U | View the message source (Ctrl+S in it: save as `.eml`) |
| Ctrl+F | Search |

The single-letter keys work while the message list has focus. Messages can
be dragged onto folders of the same account. Clicking an attachment opens it
with its default application; the arrow next to it offers *Save As*.

## Development test environment

`testenv/` has local mail servers in Docker:

```sh
docker compose -f testenv/compose.yaml up -d   # Dovecot (IMAP) + Mailpit (SMTP)
testenv/seed.py demo                           # fill a test mailbox
testenv/demo.sh                                # run Tern against it, isolated XDG dirs
```

The integration tests use the same servers:

```sh
TERN_TEST_IMAP=127.0.0.1:31143 TERN_TEST_SMTP=127.0.0.1:1025 \
TERN_TEST_MAILPIT_API=127.0.0.1:8025 cargo test --workspace
```

Without these variables, the server tests skip themselves.
