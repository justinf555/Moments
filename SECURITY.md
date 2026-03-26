# Security Policy

## Reporting a Vulnerability

If you discover a security vulnerability in Moments, please report it responsibly using [GitHub Security Advisories](https://github.com/justinf555/Moments/security/advisories/new).

**Please do not open a public issue for security vulnerabilities.**

## What to Include

- Description of the vulnerability
- Steps to reproduce
- Potential impact
- Suggested fix (if you have one)

## Response Timeline

- **Acknowledgement**: within 7 days of your report
- **Assessment**: we will evaluate severity and impact within 14 days
- **Fix**: critical issues will be prioritised for the next release

## Security Considerations

Moments handles the following sensitive data:

- **Immich API tokens** are stored in the GNOME Keyring (via `libsecret`), not in plaintext files or the database
- **Photo metadata** (GPS coordinates, timestamps) is stored in the local SQLite database
- **Network communication** with Immich servers uses HTTPS via `reqwest` with `rustls-tls`

## Supported Versions

As Moments is in early development (pre-1.0), security fixes are applied to the `main` branch only.
