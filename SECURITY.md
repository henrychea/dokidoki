# Security policy

## Reporting a vulnerability

Do not open a public issue for a security problem.

Use GitHub's private vulnerability reporting on this repository:
`https://github.com/henrychea/dokidoki/security/advisories/new`.

If you cannot use GitHub, email the maintainer at
`security@example.com` (replace with the project security contact before the
public release). Include:

- the affected version or commit,
- the command or UI path that triggers it,
- a minimal reproduction, with secrets and private repository names removed,
- the impact you believe it has.

You will get an acknowledgement as soon as the report is read, and a status
update when the issue is confirmed or ruled out.

## Scope

In scope:

- Force-push, history rewrite, or bypass of a safety invariant.
- A secret, token, or credential leaving the machine, being written to a
  journal, log, notification, or prompt where it should not go.
- A repository file (`.doki.toml`) that can execute a binary or widen the
  configured policy.
- Command injection through repository names, branch names, model names, agent
  output, or configuration values.
- A path traversal or symlink escape in discovery, snapshots, or state files.
- Any way for an untrusted repository to make `doki` mutate data outside that
  repository.

Never force-pushing and never leaking a secret are the two properties this
project cares about most. A report that demonstrates either failure is treated
as high severity even when exploitation requires unusual conditions.

Out of scope:

- Bugs that require the local user to deliberately run `doki` with a malicious
  configuration they wrote themselves, unless the configuration is documented
  as safe.
- Denial of service against your own machine that only stops `doki` from
  syncing.
- Issues in third-party CLIs (`omp`, `codex`, `claude`, `opencode`, `pi`) or in
  `git` itself. Report those upstream.

## Supported versions

The latest 0.1.x release receives security fixes. Older commits are not
maintained.
