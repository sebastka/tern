//! cxx bridge over `tern-app` for the C++/Qt frontend (ARCHITECTURE.md §3, §4).
//!
//! Only plain data crosses: shared structs, strings, vectors, integers. The
//! C++ side implements `EventSink`; Rust calls it from runtime threads.

use tern_app as app;

#[cxx::bridge(namespace = "tern::ffi")]
mod ffi {
    #[derive(Debug, Clone, PartialEq, Eq)]
    struct MessageKey {
        account: String,
        id: i64,
    }

    struct StartupInfo {
        profiles: Vec<String>,
        /// Empty when the picker must be shown.
        auto_profile: String,
        issues: Vec<String>,
    }

    struct OpenResult {
        ok: bool,
        already_running: bool,
        message: String,
    }

    struct AccountInfo {
        id: String,
        name: String,
        email: String,
        sign_by_default: bool,
        encrypt_when_possible: bool,
        has_pgp_key: bool,
        can_archive: bool,
        compose_format: BodyFormat,
    }

    struct FolderNode {
        account: String,
        folder: i64,
        depth: u32,
        parent: i32,
        name: String,
        path: String,
        role: String,
        selectable: bool,
        unread: u32,
        total: u32,
    }

    #[derive(Clone)]
    struct MessageRow {
        key: MessageKey,
        depth: u32,
        thread_size: u32,
        date: i64,
        from: String,
        to: String,
        subject: String,
        unread: bool,
        flagged: bool,
        answered: bool,
        has_attachments: bool,
        encrypted: bool,
        size: u32,
    }

    #[derive(Clone)]
    struct AttachmentInfo {
        index: u32,
        filename: String,
        content_type: String,
        size: u64,
    }

    #[repr(u8)]
    enum SignatureState {
        None,
        Good,
        Warning,
        Bad,
        Unknown,
    }

    #[repr(u8)]
    enum AccountState {
        Connecting,
        Syncing,
        Online,
        Offline,
        Error,
    }

    #[repr(u8)]
    enum BodyFormat {
        Plain,
        Markdown,
        Html,
    }

    #[repr(u8)]
    enum ListColumn {
        Flag,
        Subject,
        From,
        To,
        Correspondent,
        Date,
        Attachment,
        Size,
    }

    struct ListLayout {
        columns: Vec<ListColumn>,
        sort_by: ListColumn,
        descending: bool,
    }

    #[repr(u8)]
    enum ReplyMode {
        Reply,
        ReplyAll,
        Forward,
    }

    #[derive(Clone)]
    struct HeaderField {
        name: String,
        value: String,
    }

    #[repr(u8)]
    enum SourceLine {
        Body,
        HeaderField,
        HeaderContinuation,
        Boundary,
        Encoded,
        Quote,
        Armor,
    }

    /// `found == false`: not downloaded yet (the download was requested).
    struct MessageSource {
        found: bool,
        file_name: String,
        raw: Vec<u8>,
        text: String,
        lines: Vec<SourceLine>,
    }

    struct MessageView {
        key: MessageKey,
        subject: String,
        from: String,
        to: String,
        cc: String,
        date: i64,
        url: String,
        text_url: String,
        text: String,
        has_html: bool,
        has_remote_content: bool,
        remote_allowed: bool,
        attachments: Vec<AttachmentInfo>,
        encrypted: bool,
        decryption_failed: bool,
        signature: SignatureState,
        signature_text: String,
        headers: Vec<HeaderField>,
        body_missing: bool,
    }

    #[derive(Clone)]
    struct Draft {
        account: String,
        to: String,
        cc: String,
        bcc: String,
        subject: String,
        body: String,
        format: BodyFormat,
        in_reply_to: String,
        references: String,
        attachments: Vec<String>,
        sign: bool,
        encrypt: bool,
        /// `account` empty: not a reply.
        reply_to_message: MessageKey,
        /// `account` empty: not a forward.
        forward_message: MessageKey,
    }

    /// A served resource or attachment; `found == false` if unavailable.
    struct Resource {
        found: bool,
        /// MIME type for resources, file name for attachments.
        name: String,
        data: Vec<u8>,
    }

    /// Read before the toolkit starts (see `tern_app::startup_settings`).
    struct StartupSettings {
        spare_renderer: bool,
    }

    extern "Rust" {
        type App;

        fn startup_settings() -> StartupSettings;

        fn app_new(sink: UniquePtr<EventSink>) -> Box<App>;

        fn startup_info(self: &App, requested: &str) -> StartupInfo;
        fn last_profile(self: &App) -> String;
        fn open_profile(self: &App, name: &str) -> OpenResult;
        fn raise_existing(self: &App, profile: &str, activation_token: &str) -> bool;
        fn close_profile(self: &App);
        fn profile_name(self: &App) -> String;
        fn config_issues(self: &App) -> Vec<String>;
        fn accounts(self: &App) -> Vec<AccountInfo>;
        fn prefer_plain_text(self: &App) -> bool;
        fn threaded_by_default(self: &App) -> bool;
        fn list_layout(self: &App) -> ListLayout;

        fn folder_tree(self: &App) -> Vec<FolderNode>;
        fn open_list(self: &App, account: &str, folder: i64, threaded: bool, query: &str) -> u32;
        fn close_list(self: &App);
        fn list_count(self: &App) -> u32;
        fn list_rows(self: &App, offset: u32, count: u32) -> Vec<MessageRow>;
        /// -1 if not in the current list.
        fn list_index_of(self: &App, key: &MessageKey) -> i64;

        fn open_message(self: &App, key: &MessageKey, allow_remote: bool);
        fn resource(self: &App, url: &str) -> Resource;
        fn attachment(self: &App, key: &MessageKey, index: u32) -> Resource;
        fn message_source(self: &App, key: &MessageKey) -> MessageSource;
        fn mark_read(self: &App, keys: &[MessageKey], read: bool);
        fn mark_flagged(self: &App, keys: &[MessageKey], flagged: bool);
        fn move_messages(self: &App, keys: &[MessageKey], account: &str, folder: i64);
        fn delete_messages(self: &App, keys: &[MessageKey]);
        fn archive_messages(self: &App, keys: &[MessageKey]);
        fn sync_now(self: &App);

        fn prepare_reply(self: &App, key: &MessageKey, mode: ReplyMode);
        fn new_draft(self: &App, account: &str) -> Draft;
        fn send(self: &App, draft: Draft) -> u64;
        fn convert_body(self: &App, body: &str, from: BodyFormat, to: BodyFormat) -> String;
        fn markdown_preview(self: &App, markdown: &str) -> String;
    }

    unsafe extern "C++" {
        include!("tern-ffi/include/sink.h");

        type EventSink;

        fn folder_tree_changed(self: &EventSink);
        fn list_changed(self: &EventSink, count: u32);
        fn message_loaded(self: &EventSink, view: MessageView);
        fn compose_ready(self: &EventSink, draft: Draft);
        fn account_status(self: &EventSink, account: String, state: u8, text: String);
        fn progress(self: &EventSink, account: String, folder: String, done: u32, total: u32);
        fn config_changed(self: &EventSink, issues: Vec<String>);
        fn draft_queued(self: &EventSink, request: u64, error: String);
        fn send_result(self: &EventSink, ok: bool, text: String);
        fn error(self: &EventSink, text: String);
        fn raise_window(self: &EventSink, activation_token: String);
    }

    // C++ builds key lists to pass as slices.
    impl Vec<MessageKey> {}
}

use ffi::*;

/// The C++ sink only posts to the GUI thread (documented contract in
/// `sink.h`), so calling it from any thread is fine.
struct SharedSink(cxx::UniquePtr<EventSink>);
unsafe impl Send for SharedSink {}
unsafe impl Sync for SharedSink {}

impl SharedSink {
    fn get(&self) -> Option<&EventSink> {
        self.0.as_ref()
    }
}

pub struct App(app::App);

fn key_in(k: &MessageKey) -> app::MessageKey {
    app::MessageKey { account: k.account.clone(), id: k.id }
}

fn key_out(k: app::MessageKey) -> MessageKey {
    MessageKey { account: k.account, id: k.id }
}

fn opt_key_out(k: Option<app::MessageKey>) -> MessageKey {
    k.map(key_out).unwrap_or(MessageKey { account: String::new(), id: 0 })
}

fn opt_key_in(k: &MessageKey) -> Option<app::MessageKey> {
    (!k.account.is_empty()).then(|| key_in(k))
}

fn keys_in(keys: &[MessageKey]) -> Vec<app::MessageKey> {
    keys.iter().map(key_in).collect()
}

fn format_out(f: app::BodyFormat) -> BodyFormat {
    match f {
        app::BodyFormat::Plain => BodyFormat::Plain,
        app::BodyFormat::Markdown => BodyFormat::Markdown,
        app::BodyFormat::Html => BodyFormat::Html,
    }
}

fn format_in(f: BodyFormat) -> app::BodyFormat {
    match f {
        BodyFormat::Markdown => app::BodyFormat::Markdown,
        BodyFormat::Html => app::BodyFormat::Html,
        _ => app::BodyFormat::Plain,
    }
}

fn draft_out(d: app::Draft) -> Draft {
    Draft {
        account: d.account,
        to: d.to,
        cc: d.cc,
        bcc: d.bcc,
        subject: d.subject,
        body: d.body,
        format: format_out(d.format),
        in_reply_to: d.in_reply_to,
        references: d.references,
        attachments: d.attachments,
        sign: d.sign,
        encrypt: d.encrypt,
        reply_to_message: opt_key_out(d.reply_to_message),
        forward_message: opt_key_out(d.forward_message),
    }
}

fn draft_in(d: Draft) -> app::Draft {
    app::Draft {
        reply_to_message: opt_key_in(&d.reply_to_message),
        forward_message: opt_key_in(&d.forward_message),
        account: d.account,
        to: d.to,
        cc: d.cc,
        bcc: d.bcc,
        subject: d.subject,
        body: d.body,
        format: format_in(d.format),
        in_reply_to: d.in_reply_to,
        references: d.references,
        attachments: d.attachments,
        sign: d.sign,
        encrypt: d.encrypt,
    }
}

fn view_out(v: app::MessageView) -> MessageView {
    MessageView {
        key: key_out(v.key),
        subject: v.subject,
        from: v.from,
        to: v.to,
        cc: v.cc,
        date: v.date,
        url: v.url,
        text_url: v.text_url,
        text: v.text,
        has_html: v.has_html,
        has_remote_content: v.has_remote_content,
        remote_allowed: v.remote_allowed,
        attachments: v
            .attachments
            .into_iter()
            .map(|a| AttachmentInfo {
                index: a.index,
                filename: a.filename,
                content_type: a.content_type,
                size: a.size,
            })
            .collect(),
        encrypted: v.encrypted,
        decryption_failed: v.decryption_failed,
        signature: match v.signature {
            app::SignatureState::None => SignatureState::None,
            app::SignatureState::Good => SignatureState::Good,
            app::SignatureState::Warning => SignatureState::Warning,
            app::SignatureState::Bad => SignatureState::Bad,
            app::SignatureState::Unknown => SignatureState::Unknown,
        },
        signature_text: v.signature_text,
        headers: v.headers.into_iter().map(|h| HeaderField { name: h.name, value: h.value }).collect(),
        body_missing: v.body_missing,
    }
}

fn deliver(sink: &EventSink, e: app::Event) {
    match e {
        app::Event::FolderTreeChanged => sink.folder_tree_changed(),
        app::Event::ListChanged { count } => sink.list_changed(count),
        app::Event::MessageLoaded(v) => sink.message_loaded(view_out(v)),
        app::Event::ComposeReady(d) => sink.compose_ready(draft_out(d)),
        app::Event::AccountStatus { account, state, text } => {
            let s = match state {
                app::AccountState::Connecting => AccountState::Connecting,
                app::AccountState::Syncing => AccountState::Syncing,
                app::AccountState::Online => AccountState::Online,
                app::AccountState::Offline => AccountState::Offline,
                app::AccountState::Error => AccountState::Error,
            };
            sink.account_status(account, s.repr, text)
        }
        app::Event::Progress { account, folder, done, total } => sink.progress(account, folder, done, total),
        app::Event::ConfigChanged { issues } => sink.config_changed(issues),
        app::Event::DraftQueued { request, error } => sink.draft_queued(request, error),
        app::Event::SendResult { ok, text } => sink.send_result(ok, text),
        app::Event::Error { text } => sink.error(text),
        app::Event::RaiseWindow { activation_token } => sink.raise_window(activation_token),
    }
}

fn startup_settings() -> StartupSettings {
    let s = app::startup_settings();
    StartupSettings { spare_renderer: s.spare_renderer }
}

fn app_new(sink: cxx::UniquePtr<EventSink>) -> Box<App> {
    let sink = SharedSink(sink);
    Box::new(App(app::App::new(move |e| {
        if let Some(s) = sink.get() {
            deliver(s, e);
        }
    })))
}

impl App {
    fn startup_info(&self, requested: &str) -> StartupInfo {
        let i = self.0.startup_info(Some(requested).filter(|r| !r.is_empty()));
        StartupInfo { profiles: i.profiles, auto_profile: i.auto_profile.unwrap_or_default(), issues: i.issues }
    }

    fn last_profile(&self) -> String {
        self.0.last_profile().unwrap_or_default()
    }

    fn open_profile(&self, name: &str) -> OpenResult {
        match self.0.open_profile(name) {
            Ok(()) => OpenResult { ok: true, already_running: false, message: String::new() },
            Err(e) => OpenResult {
                ok: false,
                already_running: matches!(e, app::OpenError::AlreadyRunning(_)),
                message: e.to_string(),
            },
        }
    }

    fn raise_existing(&self, profile: &str, activation_token: &str) -> bool {
        self.0.raise_existing(profile, activation_token)
    }

    fn close_profile(&self) {
        self.0.close_profile();
    }

    fn profile_name(&self) -> String {
        self.0.profile_name().unwrap_or_default()
    }

    fn config_issues(&self) -> Vec<String> {
        self.0.config_issues()
    }

    fn accounts(&self) -> Vec<AccountInfo> {
        self.0
            .accounts()
            .into_iter()
            .map(|a| AccountInfo {
                id: a.id,
                name: a.name,
                email: a.email,
                sign_by_default: a.sign_by_default,
                encrypt_when_possible: a.encrypt_when_possible,
                has_pgp_key: a.has_pgp_key,
                can_archive: a.can_archive,
                compose_format: format_out(a.compose_format),
            })
            .collect()
    }

    fn prefer_plain_text(&self) -> bool {
        self.0.prefer_plain_text()
    }

    fn threaded_by_default(&self) -> bool {
        self.0.threaded_by_default()
    }

    fn list_layout(&self) -> ListLayout {
        let column = |c: app::ListColumn| match c {
            app::ListColumn::Flag => ListColumn::Flag,
            app::ListColumn::Subject => ListColumn::Subject,
            app::ListColumn::From => ListColumn::From,
            app::ListColumn::To => ListColumn::To,
            app::ListColumn::Correspondent => ListColumn::Correspondent,
            app::ListColumn::Date => ListColumn::Date,
            app::ListColumn::Attachment => ListColumn::Attachment,
            app::ListColumn::Size => ListColumn::Size,
        };
        let l = self.0.list_layout();
        ListLayout {
            columns: l.columns.into_iter().map(column).collect(),
            sort_by: column(l.sort_by),
            descending: l.descending,
        }
    }

    fn folder_tree(&self) -> Vec<FolderNode> {
        self.0
            .folder_tree()
            .into_iter()
            .map(|n| FolderNode {
                account: n.account,
                folder: n.folder,
                depth: n.depth,
                parent: n.parent,
                name: n.name,
                path: n.path,
                role: n.role,
                selectable: n.selectable,
                unread: n.unread,
                total: n.total,
            })
            .collect()
    }

    fn open_list(&self, account: &str, folder: i64, threaded: bool, query: &str) -> u32 {
        self.0.open_list(app::FolderKey { account: account.to_owned(), folder }, threaded, query)
    }

    fn close_list(&self) {
        self.0.close_list();
    }

    fn list_count(&self) -> u32 {
        self.0.list_count()
    }

    fn list_rows(&self, offset: u32, count: u32) -> Vec<MessageRow> {
        self.0
            .list_rows(offset, count)
            .into_iter()
            .map(|r| MessageRow {
                key: key_out(r.key),
                depth: r.depth,
                thread_size: r.thread_size,
                date: r.date,
                from: r.from,
                to: r.to,
                subject: r.subject,
                unread: r.unread,
                flagged: r.flagged,
                answered: r.answered,
                has_attachments: r.has_attachments,
                encrypted: r.encrypted,
                size: r.size,
            })
            .collect()
    }

    fn list_index_of(&self, key: &MessageKey) -> i64 {
        self.0.list_index_of(&key_in(key)).map(i64::from).unwrap_or(-1)
    }

    fn open_message(&self, key: &MessageKey, allow_remote: bool) {
        self.0.open_message(key_in(key), allow_remote);
    }

    fn resource(&self, url: &str) -> Resource {
        match self.0.resource(url) {
            Some((name, data)) => Resource { found: true, name, data },
            None => Resource { found: false, name: String::new(), data: Vec::new() },
        }
    }

    fn attachment(&self, key: &MessageKey, index: u32) -> Resource {
        match self.0.attachment(&key_in(key), index) {
            Some((name, data)) => Resource { found: true, name, data },
            None => Resource { found: false, name: String::new(), data: Vec::new() },
        }
    }

    fn message_source(&self, key: &MessageKey) -> MessageSource {
        let line = |l: app::SourceLine| match l {
            app::SourceLine::Body => SourceLine::Body,
            app::SourceLine::HeaderField => SourceLine::HeaderField,
            app::SourceLine::HeaderContinuation => SourceLine::HeaderContinuation,
            app::SourceLine::Boundary => SourceLine::Boundary,
            app::SourceLine::Encoded => SourceLine::Encoded,
            app::SourceLine::Quote => SourceLine::Quote,
            app::SourceLine::Armor => SourceLine::Armor,
        };
        match self.0.message_source(&key_in(key)) {
            Some(s) => MessageSource {
                found: true,
                file_name: s.file_name,
                raw: s.raw,
                text: s.text,
                lines: s.lines.into_iter().map(line).collect(),
            },
            None => MessageSource {
                found: false,
                file_name: String::new(),
                raw: Vec::new(),
                text: String::new(),
                lines: Vec::new(),
            },
        }
    }

    fn mark_read(&self, keys: &[MessageKey], read: bool) {
        self.0.mark_read(&keys_in(keys), read);
    }

    fn mark_flagged(&self, keys: &[MessageKey], flagged: bool) {
        self.0.mark_flagged(&keys_in(keys), flagged);
    }

    fn move_messages(&self, keys: &[MessageKey], account: &str, folder: i64) {
        self.0.move_messages(&keys_in(keys), &app::FolderKey { account: account.to_owned(), folder });
    }

    fn delete_messages(&self, keys: &[MessageKey]) {
        self.0.delete_messages(&keys_in(keys));
    }

    fn archive_messages(&self, keys: &[MessageKey]) {
        self.0.archive_messages(&keys_in(keys));
    }

    fn sync_now(&self) {
        self.0.sync_now();
    }

    fn prepare_reply(&self, key: &MessageKey, mode: ReplyMode) {
        let mode = match mode {
            ReplyMode::ReplyAll => app::ReplyMode::ReplyAll,
            ReplyMode::Forward => app::ReplyMode::Forward,
            _ => app::ReplyMode::Reply,
        };
        self.0.prepare_reply(key_in(key), mode);
    }

    fn new_draft(&self, account: &str) -> Draft {
        draft_out(self.0.new_draft(account))
    }

    fn send(&self, draft: Draft) -> u64 {
        self.0.send(draft_in(draft))
    }

    fn convert_body(&self, body: &str, from: BodyFormat, to: BodyFormat) -> String {
        self.0.convert_body(body, format_in(from), format_in(to))
    }

    fn markdown_preview(&self, markdown: &str) -> String {
        self.0.markdown_preview(markdown)
    }
}
