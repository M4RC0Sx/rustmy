# Security policy

## Supported versions

Security fixes go into the latest release. Please upgrade to it before
reporting, in case the problem is already fixed.

## Reporting a vulnerability

Please do not open a public issue for a vulnerability. Report it privately
through GitHub's [private vulnerability reporting](https://github.com/M4RC0Sx/rustmy/security/advisories/new),
or by email to contact@marcosalonso.dev.

Include what you can of: the affected version, the server (MySQL or MariaDB
and its version), the connection options involved (character set, TLS mode,
authentication method), and the steps or code that reproduce it.

You will get an acknowledgement as soon as possible. Once a fix is released,
the advisory is published with credit to the reporter, unless you prefer
otherwise.

## Scope

Especially relevant to a database driver: SQL injection through parameter
escaping or character set handling, TLS certificate verification, the
authentication methods, and memory safety in the Rust code that parses data
from the server.
