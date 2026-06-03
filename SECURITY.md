# Security Policy

## Supported Versions

Currently, only the latest version of Always is supported with security updates.

## Reporting a Vulnerability

If you discover a security vulnerability, please report it responsibly.

### Private Disclosure (Preferred)

For sensitive security issues, please send an email to:
- [security contact email to be added]

Please include:
- Description of the vulnerability
- Steps to reproduce (if applicable)
- Potential impact
- Suggested fix (if known)

We will respond within 48 hours and work with you to address the issue.

### GitHub Security Advisories

You can also report vulnerabilities through GitHub's built-in security advisory feature:
1. Go to the Security tab
2. Click "Report a vulnerability"
3. Follow the prompts to submit a private report

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
