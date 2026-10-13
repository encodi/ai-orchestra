//! A conservative, best-effort check for text that should not be remembered.
//!
//! *Credentials* (API keys, tokens, passwords, private keys) are never stored: not as memory, not
//! in a summary. *Personal data* that looks like contact details or payment numbers is stored only
//! when the user explicitly allows it for a persistent memory, and is never written to session
//! memory automatically. This is a heuristic, not a data-loss-prevention system: it will miss
//! things, and it says so in the documentation.

use crate::store::redact_secrets;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sensitivity {
    None,
    /// Looks like contact details or payment data.
    Personal,
    /// Looks like a secret.
    Credential,
}

fn looks_like_email(word: &str) -> bool {
    let w = word.trim_matches(|c: char| {
        !c.is_alphanumeric() && c != '@' && c != '.' && c != '_' && c != '-' && c != '+'
    });
    match w.split_once('@') {
        Some((local, domain)) => {
            !local.is_empty()
                && domain.contains('.')
                && !domain.starts_with('.')
                && !domain.ends_with('.')
                && domain
                    .chars()
                    .all(|c| c.is_alphanumeric() || c == '.' || c == '-')
        }
        None => false,
    }
}

/// The most digits in one phone-number- or card-shaped span: a standalone run of digits, or a few
/// groups of 2-4 digits joined by single spaces, dashes or dots. A number inside a longer token (a
/// hash in a URL, an id in a file name) is not one, and neither is a long list of unrelated numbers.
fn longest_digit_run(text: &str) -> usize {
    let chars: Vec<char> = text.chars().collect();
    let boundary = |c: Option<&char>| {
        c.is_none_or(|c| !(c.is_alphanumeric() || matches!(c, '/' | '_' | '=' | '#')))
    };
    let mut best = 0;
    let mut i = 0;
    while i < chars.len() {
        if !chars[i].is_ascii_digit() {
            i += 1;
            continue;
        }
        let start = i;
        let mut groups: Vec<usize> = Vec::new();
        let mut separators_ok = true;
        let mut end = i;
        while end < chars.len() {
            let g_start = end;
            while end < chars.len() && chars[end].is_ascii_digit() {
                end += 1;
            }
            groups.push(end - g_start);
            // A single separator followed by another digit continues the span.
            if end + 1 < chars.len()
                && matches!(chars[end], ' ' | '-' | '.')
                && chars[end + 1].is_ascii_digit()
            {
                end += 1;
            } else {
                break;
            }
        }
        let before_ok =
            start == 0 || boundary(chars.get(start - 1)) || matches!(chars[start - 1], '+' | '(');
        let after_ok = boundary(chars.get(end));
        let digits: usize = groups.iter().sum();
        if groups.len() > 1
            && (groups.len() > 5 || groups.iter().any(|g| !(2..=4).contains(g) && *g != digits))
        {
            separators_ok = false;
        }
        if before_ok && after_ok && separators_ok {
            best = best.max(digits);
        }
        i = end.max(start + 1);
    }
    best
}

fn has_credential_shape(text: &str) -> bool {
    let lower = text.to_lowercase();
    if lower.contains("-----begin") && lower.contains("private key") {
        return true;
    }
    for key in [
        "password",
        "passwd",
        "contraseña",
        "secret",
        "api key",
        "api_key",
        "apikey",
        "token",
    ] {
        let mut from = 0;
        while let Some(i) = lower[from..].find(key) {
            let after = &lower[from + i + key.len()..];
            let rest = after.trim_start_matches([' ', '"', '\'']);
            if (rest.starts_with(':') || rest.starts_with('=') || rest.starts_with("is "))
                && rest
                    .trim_start_matches([':', '=', ' ', '"', '\''])
                    .trim_start_matches("is ")
                    .chars()
                    .take_while(|c| !c.is_whitespace())
                    .count()
                    >= 6
            {
                return true;
            }
            from += i + key.len();
        }
    }
    // A long unbroken token with letters and digits, like a key.
    text.split_whitespace().any(|w| {
        let w = w.trim_matches(|c: char| !c.is_alphanumeric());
        w.len() >= 32
            && w.chars().any(|c| c.is_ascii_digit())
            && w.chars().any(|c| c.is_ascii_alphabetic())
            && w.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    })
}

pub fn classify(text: &str) -> Sensitivity {
    if redact_secrets(text) != text || has_credential_shape(text) {
        return Sensitivity::Credential;
    }
    if text.split_whitespace().any(looks_like_email) || longest_digit_run(text) >= 10 {
        return Sensitivity::Personal;
    }
    let lower = text.to_lowercase();
    if [
        "my address is",
        "mi dirección es",
        "my phone",
        "mi teléfono",
        "my ssn",
        "social security",
        "my passport",
        "mi curp",
        "my home address",
    ]
    .iter()
    .any(|p| lower.contains(p))
    {
        return Sensitivity::Personal;
    }
    Sensitivity::None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credentials_are_recognised_in_their_usual_shapes() {
        for text in [
            "my key is sk-abcdefghijklmnopqrstuvwxyz0123456789",
            "password: hunter2hunter2",
            "contraseña = abcdef123",
            "api_key=AAAAbbbbCCCC1111222233334444",
            "the token is 9f8e7d6c5b4a39281706f5e4d3c2b1a09f8e7d6c",
            "-----BEGIN RSA PRIVATE KEY----- MIIE",
            "export OPENAI_API_KEY=abcdef123456",
        ] {
            assert_eq!(classify(text), Sensitivity::Credential, "{text}");
        }
    }

    #[test]
    fn personal_data_is_recognised_but_ordinary_numbers_are_not() {
        for text in [
            "write to ana.perez@example.com",
            "call me at +52 55 1234 5678",
            "card 4111 1111 1111 1111",
            "my home address is Calle 5 #20",
        ] {
            assert_eq!(classify(text), Sensitivity::Personal, "{text}");
        }
        for text in [
            "budget is 8,000 MXN",
            "the oscilloscope has 100 MHz of bandwidth and 1 GSa/s",
            "I prefer 2 channels and a 7 inch screen",
            "the password field must be at least 8 characters",
        ] {
            assert_eq!(classify(text), Sensitivity::None, "{text}");
        }
    }

    #[test]
    fn hashes_in_urls_and_lists_of_specifications_are_not_personal_data() {
        for text in [
            "Fuente: https://example.com/b16a09d65d0bbd249c/67747de1e0b6243125033c2f_gofikulofafijapixix.pdf",
            "HDS242 40 MHz, 250 MSa/s, 25,300 yuan; HDS2102 100 MHz, 500 MSa/s, 38,500 yuan",
            "version 20261012 build 123456789 of the firmware",
        ] {
            assert_eq!(classify(text), Sensitivity::None, "{text}");
        }
        for text in [
            "call +52 55 1234 5678",
            "card 4111 1111 1111 1111",
            "tel 555-123-4567 please",
            "id 1234567890 end",
        ] {
            assert_eq!(classify(text), Sensitivity::Personal, "{text}");
        }
    }
}
