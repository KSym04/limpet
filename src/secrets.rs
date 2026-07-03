//! Secret detection for the write path.
//!
//! `remember` refuses to persist a memory whose body or evidence looks like a
//! credential, so a secret can never enter the local store and therefore can
//! never leak later through `admin export` -> `.limpet/memory.jsonl` -> git.
//!
//! Detection is deliberately high-precision (provider-specific prefixes with
//! length and charset checks) to avoid false positives on ordinary prose like
//! "the endpoint returns a bearer token". No regex dependency: the whole scan
//! is a token walk plus a couple of substring checks.

/// The first credential detected in `text`, as a human-readable label, or
/// `None` if nothing matched.
pub fn detect(text: &str) -> Option<&'static str> {
    // Multi-line block markers, checked on the whole text.
    if text.contains("-----BEGIN") && text.contains("PRIVATE KEY-----") {
        return Some("private key block");
    }

    // Everything else is a self-contained token: split on characters that
    // never appear inside these credentials and inspect each candidate.
    for tok in text.split(|c: char| {
        c.is_whitespace() || matches!(c, '"' | '\'' | '`' | ',' | ';' | '(' | ')' | '<' | '>')
    }) {
        if let Some(label) = classify_token(tok) {
            return Some(label);
        }
    }
    None
}

fn classify_token(tok: &str) -> Option<&'static str> {
    let n = tok.len();

    // AWS access key id: AKIA/ASIA + 16 uppercase alnum.
    if (tok.starts_with("AKIA") || tok.starts_with("ASIA"))
        && n == 20
        && tok[4..].bytes().all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
    {
        return Some("AWS access key id");
    }

    // GitHub tokens: ghp_/gho_/ghu_/ghs_/ghr_ + >=36, or github_pat_.
    if let Some(rest) = tok
        .strip_prefix("ghp_")
        .or_else(|| tok.strip_prefix("gho_"))
        .or_else(|| tok.strip_prefix("ghu_"))
        .or_else(|| tok.strip_prefix("ghs_"))
        .or_else(|| tok.strip_prefix("ghr_"))
    {
        if rest.len() >= 36 && rest.bytes().all(|b| b.is_ascii_alphanumeric()) {
            return Some("GitHub token");
        }
    }
    if tok.starts_with("github_pat_") && n >= 40 {
        return Some("GitHub token");
    }

    // Slack tokens: xoxb-/xoxp-/xoxa-/xoxr-/xoxs- + digits/hyphens.
    if tok.starts_with("xox") && n >= 5 && tok.as_bytes().get(4) == Some(&b'-') && n >= 20 {
        return Some("Slack token");
    }

    // OpenAI-style keys: sk- (and sk-proj-) + long base62-ish body.
    if let Some(rest) = tok.strip_prefix("sk-") {
        let body = rest.strip_prefix("proj-").unwrap_or(rest);
        if body.len() >= 20 && body.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_') {
            return Some("API secret key");
        }
    }

    // Stripe live/test secret keys.
    if (tok.starts_with("sk_live_") || tok.starts_with("sk_test_") || tok.starts_with("rk_live_"))
        && n >= 24
    {
        return Some("Stripe secret key");
    }

    // Google API key: AIza + 35 url-safe chars.
    if tok.starts_with("AIza")
        && n == 39
        && tok[4..].bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Some("Google API key");
    }

    // JSON Web Token: three base64url segments separated by dots. Require real
    // length so short "a.b.c" style prose does not trip it.
    if tok.starts_with("eyJ") {
        let parts: Vec<&str> = tok.split('.').collect();
        if parts.len() == 3
            && n >= 40
            && parts.iter().all(|p| {
                !p.is_empty()
                    && p.bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
            })
        {
            return Some("JWT");
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flags_real_credentials() {
        assert_eq!(detect("key is AKIAIOSFODNN7EXAMPLE here"), Some("AWS access key id"));
        assert_eq!(
            detect("token ghp_1234567890abcdefghijklmnopqrstuvwxyz"),
            Some("GitHub token")
        );
        assert_eq!(detect("xoxb-123456789012-abcdefghijkl"), Some("Slack token"));
        assert_eq!(
            detect("sk-proj-abcdefghijklmnopqrstuvwxyz0123456789"),
            Some("API secret key")
        );
        assert_eq!(
            detect("AIzaSyD1234567890abcdefghijklmnopqrstuv"),
            Some("Google API key")
        );
        assert_eq!(
            detect("-----BEGIN OPENSSH PRIVATE KEY-----\nabc\n-----END OPENSSH PRIVATE KEY-----"),
            Some("private key block")
        );
        assert!(detect("Bearer eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.abcDEFghiJKL").is_some());
    }

    #[test]
    fn ignores_ordinary_prose_and_code_names() {
        assert_eq!(detect("the endpoint returns a bearer token on login"), None);
        assert_eq!(detect("recall applies a 0.35 relative score cutoff"), None);
        assert_eq!(detect("call sk_from_context() then skip the akia branch"), None);
        assert_eq!(detect("main.rs uses a hand-rolled arg parser, not clap"), None);
        assert_eq!(detect("a.b.c is a dotted path, not a JWT"), None);
    }
}
