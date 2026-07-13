# Security Policy

## Supported Versions

| Version | Supported |
|---------|:---------:|
| 0.13.x  | ✅        |
| < 0.13  | ❌        |

Only the latest minor release receives security updates. Patch versions are
released for the latest minor only.

## Reporting a Vulnerability

If you discover a security vulnerability, please report it responsibly.
**Do not open a public GitHub issue.**

### Private Disclosure (Preferred)

Use GitHub's private security advisory feature (preferred — encrypted in
transit and tracked alongside the fix):

1. Go to the **Security** tab of the [`rtk-ai/always`](https://github.com/rtk-ai/always) repository
2. Click **Report a vulnerability**
3. Follow the prompts to submit a private report

If GitHub Security Advisories are unavailable to you, send email to:

- **security@always.devliv.io**

GPG fingerprint for encrypted reports: `<TBD — published in v1.0.0 release notes>`

Please include:
- Description of the vulnerability
- Steps to reproduce (if applicable)
- Potential impact (CVSS optional)
- Suggested fix (if known)
- Whether you want public credit

## Disclosure Timeline (Embargo Policy)

| Stage | Target |
|-------|--------|
| Acknowledgement of report | within **48 hours** |
| Initial triage + severity assessment | within **5 business days** |
| Coordinated patch + advisory drafted | within **30 days** for High/Critical, **60 days** for Medium, **90 days** for Low |
| Public disclosure (CVE + advisory + fix) | at end of embargo, or sooner with reporter consent |

Critical vulnerabilities (RCE, credential exfiltration, microphone hijack)
are prioritized above all other work and may be patched out-of-band.

We will keep the reporter updated weekly during the embargo. If we miss a
milestone, the reporter is free to disclose publicly with 7 days' notice.

## Credit

Reporters are credited in the GitHub Security Advisory and CHANGELOG by
default. Opt out by stating so in the report. We do not currently operate a
paid bounty program.

## Security Best Practices

### API Keys
- Never commit API keys to the repository
- Use environment variables or secure storage (Keychain on macOS)
- API keys are masked in configuration output
- Logs do not contain full API key values

### Logs
- Logs are stored in platform-standard locations with appropriate permissions
- Transcript content is not logged by default (requires `ALWAYS_LOG_TRANSCRIPTS=1`)
- API key prefixes are never logged
- Log files are rotated daily, keeping 7 days of history

### Inter-Process Communication
- UDS socket uses restricted permissions (0600)
- Socket location is in user-specific directories, not world-writable paths
- Commands are validated for length and rate-limited

### Dependencies
- Dependencies are regularly updated
- CI runs `cargo audit` to check for known vulnerabilities
- Third-party dependencies are reviewed before adding

## Security Features

- **API Key Protection**: Keys stored in Keychain (macOS) or secret-service (Linux)
- **Secure Logging**: Structured logging with privacy controls
- **Process Isolation**: Daemon runs as user process, not root
- **Permission Model**: Requires microphone and accessibility permissions
- **Code Signing**: Application signed and notarized (when distributed)

## Transparency

Security advisories will be published on GitHub when fixed versions are released. We aim to:
- Acknowledge reports within 48 hours
- Provide regular updates on remediation progress
- Release fixes within a reasonable timeframe based on severity
- Credit reporters (if desired)
