# Security Policy

## Supported Versions

Only the latest release is actively supported. Security fixes are applied to
the current `main` branch and released as part of the next version.

| Version | Supported |
|---------|-----------|
| latest (v0.1.x) | ✅ |
| older | ❌ |

## Reporting a Vulnerability

Please do **not** open a public GitHub Issue for a security problem.

Instead, report it privately via GitHub's **Security** tab on this repository
(**Report a vulnerability**). If you prefer, you can use the GitHub
"Report a vulnerability" flow to disclose details confidentially.

You should receive an acknowledgement promptly. Because Sutura is a
single-developer open-source project, there is no guaranteed response SLA, but
we aim to respond as soon as possible. Please do not disclose the issue
publicly until it has been addressed and released.

## What we consider a security issue

Sutura processes untrusted mesh files (STL/3MF), so the primary concern is a
**malicious or malformed input file** that:

- causes a crash or hangs the process (denial of service), or
- leads to arbitrary code execution (RCE) through a parser/deserializer bug,
  or
- reads or writes unexpected files on the host.

Robustness against such inputs is a core goal of the project; if you find a
way to crash Sutura or corrupt its behaviour with a crafted file, please
report it even if you are unsure whether it is exploitable.
