#!/usr/bin/env python3
"""Fill a test mailbox on the local Dovecot (testenv/compose.yaml) with varied
mail: threads, HTML with remote images, attachments, non-ASCII text, many
messages for scrolling, and a subfolder.

    testenv/seed.py [user]        (default user: demo, password: pass)
"""

import imaplib
import sys
import time
from email.message import EmailMessage
from email.utils import format_datetime, make_msgid
from datetime import datetime, timedelta, timezone

USER = sys.argv[1] if len(sys.argv) > 1 else "demo"
ME = f"{USER}@example.org"
NOW = datetime.now(timezone.utc)


def msg(subject, frm, body, *, when, to=ME, html=None, reply_to=None, attach=None, msgid=None):
    m = EmailMessage()
    m["From"] = frm
    m["To"] = to
    m["Subject"] = subject
    m["Date"] = format_datetime(when)
    m["Message-ID"] = msgid or make_msgid(domain="example.org")
    if reply_to is not None:
        m["In-Reply-To"] = reply_to["Message-ID"]
        refs = (reply_to.get("References", "") + " " + reply_to["Message-ID"]).strip()
        m["References"] = refs
    m.set_content(body)
    if html:
        m.add_alternative(html, subtype="html")
    for name, ctype, data in attach or []:
        maintype, subtype = ctype.split("/")
        m.add_attachment(data, maintype=maintype, subtype=subtype, filename=name)
    return m


def main():
    imap = imaplib.IMAP4("127.0.0.1", 31143)
    imap.login(USER, "pass")
    imap.create('"Lists"')
    imap.create('"Lists/rust-users"')
    imap.subscribe('"Lists/rust-users"')

    def put(folder, m, seen=False):
        flags = "(\\Seen)" if seen else "()"
        imap.append(f'"{folder}"', flags, imaplib.Time2Internaldate(time.time()), m.as_bytes())

    t = NOW - timedelta(days=3)
    root = msg("Plans for the weekend", "Ann Example <ann@example.org>",
               "Hi!\n\nShall we go hiking on Saturday? The weather looks good.\n\nAnn", when=t)
    r1 = msg("Re: Plans for the weekend", "Bob Builder <bob@example.org>",
             "Count me in.\n\n> Shall we go hiking on Saturday?\n", when=t + timedelta(hours=2), reply_to=root)
    r2 = msg("Re: Plans for the weekend", f"Demo User <{ME}>",
             "Great, I'll bring sandwiches.\n\n> Count me in.\n", when=t + timedelta(hours=3), reply_to=r1)
    r3 = msg("Re: Plans for the weekend", "Ann Example <ann@example.org>",
             "Perfect. Meet at the station at 9:00.\n", when=t + timedelta(hours=5), reply_to=r1)
    for m, seen in ((root, True), (r1, True), (r2, True), (r3, False)):
        put("INBOX", m, seen)

    html = """<html><head><style>.box{border:1px solid #ccc;padding:12px;border-radius:6px}
h1{color:#2a6fb0}</style></head><body>
<h1>Newsletter #42</h1><div class="box"><p>This newsletter has a <b>tracking pixel</b>
and a remote image:</p><img src="https://example.com/tracker.gif" width="1" height="1">
<p><img src="https://www.rust-lang.org/static/images/rust-logo-blk.svg" width="64"></p>
<p>And an inline image: <img src="cid:dot@example.org" alt="inline"></p>
<p><a href="https://www.rust-lang.org/">A link</a> opens in your browser.</p>
<script>alert('scripts never run')</script></div></body></html>"""
    news = msg("Newsletter #42 — tracking pixels inside", "News <news@lwn.example>",
               "Newsletter #42 (plain version)\n\nVisit https://www.rust-lang.org/", when=NOW - timedelta(hours=20),
               html=html)
    # Attach the cid image to the HTML part's related set.
    png = bytes.fromhex("89504e470d0a1a0a0000000d4948445200000010000000100802000000909168360000001c4944415478"
                        "9c63fccf801f30e1924bd4c00848c0c8303a0a8c02f00084d1037f6fa5e2a10000000049454e44ae426082")
    news.get_body(("html",)).add_related(png, "image", "png", cid="<dot@example.org>")
    put("INBOX", news)

    put("INBOX", msg("Rechnung für Oktober — Grüße aus Köln", "Müller & Söhne <rechnung@example.de>",
                     "Sehr geehrte Damen und Herren,\n\nanbei die Rechnung. Größe: 2 KiB.\n\nMit freundlichen Grüßen",
                     when=NOW - timedelta(hours=6),
                     attach=[("Rechnung Oktober.pdf", "application/pdf", b"%PDF-1.4\n% fake pdf\n"),
                             ("notes.txt", "text/plain", "line one\nline two\n".encode())]))

    long_lines = ("This is a long paragraph written by someone whose mail client does not wrap lines at all, "
                  "so the whole paragraph arrives as one single very long line that the viewer must wrap. ") * 4
    put("INBOX", msg("Long lines and quotes", "Carl <carl@example.net>",
                     long_lines + "\n\n> quoted once\n>> quoted twice\n>>> quoted three times\n\n-- \nCarl",
                     when=NOW - timedelta(hours=1)))

    for i in range(150):
        put("Lists/rust-users", msg(f"[rust-users] Question {i}: borrowing in loops",
                                    f"User {i} <user{i}@example.net>",
                                    f"Message number {i} on the list.\n",
                                    when=NOW - timedelta(days=30) + timedelta(hours=4 * i)), seen=i < 140)
    imap.logout()
    print(f"seeded mailbox of {USER}")


if __name__ == "__main__":
    main()
