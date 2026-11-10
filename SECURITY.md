# Security

## Supported version

WagglePost 0.2.x on Windows 10/11 x64 is the tested platform. Unix builds are
present in the source tree but are not supported or equated with the Windows
acceptance.

## Reporting

Please report a vulnerability privately through this repository's GitHub
Security Advisory page when available. Do not put credentials, tokens, private
repository contents or exploit details in a public issue.

Include the affected version/commit, platform, reproduction steps, and whether
the issue requires a local user, a browser session, or a malicious runtime.

## Security model

- The server binds `127.0.0.1` only and validates Host/Origin.
- The browser uses a one-time startup code, an HttpOnly cookie and CSRF.
- The CLI uses a separate bearer token in the managed-root session file. It can
  optionally be limited to repositories with `--cli-repo DIR` (repeatable) or
  `WAGGLEPOST_CLI_ALLOWED_REPOS` (path-list).
- Agent output is bounded and redacted before persistence/display; user code,
  patches and diffs are not rewritten.
- Credentials for model runtimes stay with those native tools. WagglePost does
  not read, print or persist model API keys. The single documented exception is
  the optional provider-key environment variable (e.g. `DEEPSEEK_API_KEY`), which
  is read from the user's environment and forwarded only to the isolated runtime
  process via its process environment — never to logs, receipts, the UI or Git.
- External runtimes launch as argv arrays inside a Windows Job Object (assigned
  before the first instruction). OpenCode reviews use an isolated home and
  a filtered snapshot. Project configuration is disabled. The per-attempt
  policy denies tools by default, permits read, glob and grep, and blocks
  external directory access. A credential-free preflight checks the exact
  policy and its sources before each launch. Cancel/timeout require whole-tree
  exit confirmation before any slot handover.
- No telemetry.

WagglePost is a local single-user tool, not a multi-tenant hosted service. Do
not expose the loopback port through a proxy or bind it to a public interface.
