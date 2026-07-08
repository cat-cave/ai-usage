//! Pure formatting helpers. No IO. The CLI crate owns terminal rendering.

//! Pure formatting helpers. No IO. The CLI crate owns terminal rendering.

use rust_decimal::Decimal;

use crate::model::{Currency, Money};

/// Mask an email for safe display: `trevor@example.com` → `t***@example.com`.
/// The `***` makes the result not match the redactor's email pattern, so a
/// masked value survives defense-in-depth redaction.
pub fn mask_email(email: &str) -> String {
    match email.split_once('@') {
        Some((local, domain)) => {
            let first = local
                .chars()
                .next()
                .map(|c| c.to_string())
                .unwrap_or_default();
            format!("{first}***@{domain}")
        }
        None => "[redacted]".into(),
    }
}

pub fn format_money(m: &Money) -> String {
    let sym = match m.currency {
        Currency::Usd => "$",
        Currency::Cny => "¥",
        Currency::Eur => "€",
        Currency::Unknown => "",
    };
    format!("{}{}", sym, trim_decimal(m.amount))
}

fn trim_decimal(d: Decimal) -> String {
    let s = d.to_string();
    // Keep two decimals for money, drop trailing zeros otherwise.
    if s.contains('.') {
        let parts: Vec<&str> = s.split('.').collect();
        let frac = if parts.len() == 2 {
            let mut f = parts[1].to_string();
            if f.len() > 2 {
                f.truncate(2);
            }
            f
        } else {
            String::new()
        };
        if frac.is_empty() {
            parts[0].to_string()
        } else {
            format!("{}.{}", parts[0], frac)
        }
    } else {
        s
    }
}
