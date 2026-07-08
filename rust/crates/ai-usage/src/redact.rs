//! Secret redaction. Every output and error path routes potentially-sensitive
//! strings through `Redactor`. See `SPEC.md §10`.

use std::sync::OnceLock;

use regex::Regex;

pub struct Redactor;

impl Redactor {
    /// Redact any credential-looking substring in `s`. Safe to call on any string.
    pub fn redact_str(s: &str) -> String {
        let mut out = s.to_string();
        for re in patterns() {
            out = re.replace_all(&out, "[REDACTED]").into_owned();
        }
        out
    }
}

fn patterns() -> &'static [Regex] {
    static P: OnceLock<Vec<Regex>> = OnceLock::new();
    P.get_or_init(|| {
        vec![
            Regex::new(r"sk-or-v1-[A-Za-z0-9_-]+").unwrap(),
            Regex::new(r"sk-ant-[A-Za-z0-9_-]+").unwrap(),
            Regex::new(r"sk-cp-[A-Za-z0-9_-]+").unwrap(),
            Regex::new(r"sk-api-[A-Za-z0-9_-]+").unwrap(),
            // DeepSeek / generic hex api keys
            Regex::new(r"sk-[a-f0-9]{32,}").unwrap(),
            // JWT / bearer tokens: three base64url chunks separated by dots
            Regex::new(r"eyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}").unwrap(),
            // JSON credential fields
            Regex::new(r#""refresh_token"\s*:\s*"[^"]+""#).unwrap(),
            Regex::new(r#""access_token"\s*:\s*"[^"]+""#).unwrap(),
            Regex::new(r#""id_token"\s*:\s*"[^"]+""#).unwrap(),
            // Emails → masked
            Regex::new(r"\b[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}\b").unwrap(),
        ]
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_all_known_secret_shapes() {
        let cases = [
            "key sk-or-v1-abcdef0123456789 here",
            "claude sk-ant-sid1-oat01-xyz stuff",
            "minimax sk-cp-123abc and sk-api-99",
            "deepseek sk-1234567890abcdef1234567890abcdef",
            "bearer eyJhbGciOi.eyJzdWIiOi.c2lnbmVk",
            r#"{"access_token":"eyJxx.yy.zz","refresh_token":"rt"}"#,
            "contact trevor@example.com please",
        ];
        for c in cases {
            let r = Redactor::redact_str(c);
            assert!(
                !r.contains("sk-or-")
                    && !r.contains("sk-ant-")
                    && !r.contains("sk-cp-")
                    && !r.contains("sk-api-")
                    && !r.contains("sk-1234")
                    && !r.contains("eyJxx")
                    && !r.contains("access_token\":\"eyJ")
                    && !r.contains("refresh_token\":\"rt")
                    && !r.contains("trevor@"),
                "leak in: {c} -> {r}"
            );
        }
    }

    #[test]
    fn masked_email_survives_redaction() {
        // A masked email `t***@x.com` must NOT match the email pattern (the `***`
        // breaks it), so defense-in-depth redaction preserves it while a raw email
        // is scrubbed.
        let masked = crate::view::mask_email("trevor@example.com");
        assert_eq!(masked, "t***@example.com");
        let redacted_masked = Redactor::redact_str(&masked);
        assert_eq!(redacted_masked, masked, "masked email should survive");
        let redacted_raw = Redactor::redact_str("trevor@example.com");
        assert!(
            !redacted_raw.contains("trevor@"),
            "raw email must be redacted: {redacted_raw}"
        );
    }
}
