//! What counts as an acceptable password, and how strong one looks.
//!
//! GUIDE: .offrails/guides/auth.md#passwords-password-variants
//!
//! NIST SP 800-63B rules, not composition rules: a minimum length, no maximum worth hitting, any
//! characters (spaces and emoji welcome, paste allowed), and a check against known-bad passwords.
//! The check is offline: a bundled list (`common_passwords.txt`) plus patterns (sequences,
//! repeats, the email's own name, the app's name).

use std::collections::HashSet;
use std::sync::LazyLock;

use crate::models::Errors;

// APP: the length bounds. 12 is the framework minimum; don't go lower. The maximum only stops
// absurd inputs (argon2 handles long ones fine).
pub const MIN_CHARS: usize = 12;
pub const MAX_CHARS: usize = 72;

static COMMON: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    include_str!("common_passwords.txt")
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .collect()
});

/// Keyboard rows and runs a person types without thinking.
const SEQUENCES: &[&str] = &[
    "abcdefghijklmnopqrstuvwxyz",
    "01234567890123456789",
    "qwertyuiopasdfghjklzxcvbnm",
    "qwertzuiopasdfghjklyxcvbnm",
    "azertyuiopqsdfghjklmwxcvbn",
    "1qaz2wsx3edc4rfv5tgb6yhn7ujm8ik9ol0p",
    "zaq1xsw2cde3vfr4bgt5nhy6mju7",
];

/// Validation errors on the `password` field (empty when acceptable).
// SECURITY: rejecting the passwords attackers try first is what makes online guessing (with
// the throttle) hopeless.
// INVARIANT: common and pattern passwords are refused (test: weak_passwords_are_refused_with_a_reason)
pub fn validate(password: &str, email: &str) -> Errors {
    let mut errors = Errors::default();
    if let Some(problem) = problem(password, email) {
        errors.add("password", problem);
    }
    errors
}

/// The first problem with `password`, as the message after "Password …".
pub fn problem(password: &str, email: &str) -> Option<&'static str> {
    let chars = password.chars().count();
    if password.trim().is_empty() {
        return Some("can't be blank");
    }
    if chars < MIN_CHARS {
        return Some("is too short (minimum is 12 characters)");
    }
    if chars > MAX_CHARS {
        return Some("is too long (maximum is 72 characters)");
    }
    let lower = password.to_lowercase();
    if is_common(&lower) || is_common(&unleet(&lower)) {
        return Some("is too common: it appears in lists of passwords attackers try first");
    }
    if is_pattern(&lower) {
        return Some("is too predictable: avoid repeats and keyboard or number sequences");
    }
    let name = email.split('@').next().unwrap_or_default().to_lowercase();
    if name.chars().count() >= 4 && lower.contains(&name) {
        return Some("can't contain your email address");
    }
    let app = crate::config::APP_NAME.to_lowercase().replace(' ', "");
    if app.chars().count() >= 4 && lower.replace(' ', "").contains(&app) {
        return Some("can't contain the name of this site");
    }
    None
}

/// A listed password, or a listed word with only digits and symbols around it
/// ("Password123!", "!!dragon2024").
fn is_common(lower: &str) -> bool {
    if COMMON.contains(lower) {
        return true;
    }
    let core =
        lower.trim_matches(|c: char| c.is_ascii_digit() || c.is_ascii_punctuation() || c == ' ');
    core.chars().count() >= 3 && COMMON.contains(core)
}

/// Common character swaps undone: "p@ssw0rd" → "password".
fn unleet(lower: &str) -> String {
    lower
        .chars()
        .map(|c| match c {
            '@' | '4' => 'a',
            '0' => 'o',
            '1' | '!' => 'i',
            '3' => 'e',
            '$' | '5' => 's',
            '7' => 't',
            c => c,
        })
        .collect()
}

/// One short unit repeated ("abcabcabcabc"), or a run along a keyboard row or the alphabet,
/// forwards or backwards.
fn is_pattern(lower: &str) -> bool {
    let chars: Vec<char> = lower.chars().collect();
    for unit in 1..=4 {
        if chars.len() >= unit * 3 && chars.iter().enumerate().all(|(i, c)| *c == chars[i % unit]) {
            return true;
        }
    }
    let compact: String = lower.chars().filter(|c| !c.is_whitespace()).collect();
    SEQUENCES.iter().any(|seq| {
        let reversed: String = seq.chars().rev().collect();
        seq.contains(compact.as_str()) || reversed.contains(compact.as_str())
    })
}

/// How strong a password looks, for the meter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Strength {
    /// 0 (refused) to 4 (strong).
    pub score: u8,
    pub label: &'static str,
    pub hint: String,
}

impl Strength {
    /// Width of the meter bar, as a Tailwind class (static, so the CSS build sees it).
    pub fn bar_class(&self) -> &'static str {
        match self.score {
            0 => "w-1/12 bg-red-600 dark:bg-red-500",
            1 => "w-1/4 bg-orange-500 dark:bg-orange-400",
            2 => "w-1/2 bg-amber-500 dark:bg-amber-400",
            3 => "w-3/4 bg-lime-600 dark:bg-lime-400",
            _ => "w-full bg-green-600 dark:bg-green-400",
        }
    }
}

/// A server-side estimate (no JS needed): refused passwords score 0; otherwise the score grows
/// with length and variety. Deliberately conservative.
// APP: thresholds and wording of the meter.
pub fn strength(password: &str, email: &str) -> Strength {
    if let Some(problem) = problem(password, email) {
        return Strength {
            score: 0,
            label: "Too weak",
            hint: format!("This password {problem}."),
        };
    }
    let len = password.chars().count();
    let classes = [
        password.chars().any(|c| c.is_ascii_lowercase()),
        password.chars().any(|c| c.is_ascii_uppercase()),
        password.chars().any(|c| c.is_ascii_digit()),
        password.chars().any(|c| !c.is_ascii_alphanumeric()),
    ]
    .iter()
    .filter(|x| **x)
    .count();
    let distinct = password.chars().collect::<HashSet<_>>().len();
    let words = password.split_whitespace().count();
    let score = match (len, classes, distinct, words) {
        (l, _, d, w) if l >= 20 && d >= 10 && w >= 3 => 4,
        (l, c, d, _) if l >= 16 && c >= 3 && d >= 10 => 4,
        (l, _, d, _) if l >= 16 && d >= 8 => 3,
        (l, c, d, _) if l >= 13 && c >= 3 && d >= 9 => 3,
        (_, _, d, _) if d >= 7 => 2,
        _ => 1,
    };
    let (label, hint) = match score {
        1 => (
            "Weak",
            "Accepted, but easy to guess. A few unrelated words make a stronger password.",
        ),
        2 => ("Fair", "Not bad. Longer is better: add another word."),
        3 => ("Good", "Good password."),
        _ => ("Strong", "Strong password."),
    };
    Strength {
        score,
        label,
        hint: hint.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EMAIL: &str = "ada.lovelace@example.com";

    #[test]
    fn weak_passwords_are_refused_with_a_reason() {
        for bad in [
            "",
            "short",
            "password1234",
            "Password1234!",
            "P@ssw0rd1234",
            "!!Dragon2024!!",
            "123456789012",
            "qwertyuiopas",
            "abcdefghijkl",
            "abcabcabcabc",
            "aaaaaaaaaaaaaaa",
            "lkjhgfdsapoiu",
            "my ada.lovelace password",
            "correcthorsebatterystaple",
        ] {
            assert!(problem(bad, EMAIL).is_some(), "{bad:?} was accepted");
        }
        assert!(problem(&"x".repeat(73), EMAIL).is_some());
    }

    #[test]
    fn good_passwords_pass_and_the_meter_ranks_them() {
        for good in [
            "violet tractor sandwich",
            "Kq7#mZ2!vR9@xL",
            "the kettle sings at dawn",
            "émigré façade naïve",
        ] {
            assert_eq!(problem(good, EMAIL), None, "{good:?} was refused");
        }
        assert_eq!(strength("password1234", EMAIL).score, 0);
        assert!(strength("the kettle sings at dawn", EMAIL).score >= 3);
        assert!(strength("Kq7#mZ2!vR9@xL", EMAIL).score >= 3);
        assert!(strength("violet tractors", EMAIL).score <= 3);
    }
}
