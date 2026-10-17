//! Names: one word given on the command line (`posts`, `Post`, `blog_post`, `BlogPosts`) becomes
//! every spelling a generator needs, the way Rails' inflector does it (`ActiveSupport::Inflector`).

use crate::util::{Fail, Result};

/// Irregular singular → plural pairs (Rails' defaults, plus a few common ones).
const IRREGULAR: &[(&str, &str)] = &[
    ("person", "people"),
    ("man", "men"),
    ("woman", "women"),
    ("child", "children"),
    ("sex", "sexes"),
    ("move", "moves"),
    ("zombie", "zombies"),
    ("ox", "oxen"),
    ("mouse", "mice"),
    ("goose", "geese"),
    ("tooth", "teeth"),
    ("foot", "feet"),
    ("leaf", "leaves"),
    ("knife", "knives"),
    ("life", "lives"),
    ("wife", "wives"),
    ("half", "halves"),
    ("wolf", "wolves"),
    ("shelf", "shelves"),
    ("calf", "calves"),
    ("thief", "thieves"),
    ("quiz", "quizzes"),
    ("index", "indices"),
    ("matrix", "matrices"),
    ("vertex", "vertices"),
    ("status", "statuses"),
    ("campus", "campuses"),
    ("alias", "aliases"),
    ("bus", "buses"),
    ("virus", "viruses"),
    ("hero", "heroes"),
    ("potato", "potatoes"),
    ("tomato", "tomatoes"),
    ("echo", "echoes"),
    ("analysis", "analyses"),
    ("crisis", "crises"),
    ("thesis", "theses"),
    ("criterion", "criteria"),
    ("datum", "data"),
    ("medium", "media"),
];

/// Words with one form (Rails' uncountables).
const UNCOUNTABLE: &[&str] = &[
    "equipment",
    "information",
    "rice",
    "money",
    "species",
    "series",
    "fish",
    "sheep",
    "jeans",
    "police",
    "news",
    "feedback",
    "metadata",
    "software",
    "hardware",
    "staff",
    "content",
];

fn is_vowel(c: char) -> bool {
    matches!(c, 'a' | 'e' | 'i' | 'o' | 'u')
}

/// The plural of one lowercase word.
pub fn pluralize_word(w: &str) -> String {
    if w.is_empty() || UNCOUNTABLE.contains(&w) {
        return w.to_owned();
    }
    if let Some((_, p)) = IRREGULAR.iter().find(|(s, _)| *s == w) {
        return (*p).to_owned();
    }
    if IRREGULAR.iter().any(|(_, p)| *p == w) {
        return w.to_owned();
    }
    let chars: Vec<char> = w.chars().collect();
    let last = chars[chars.len() - 1];
    let before = chars.len().checked_sub(2).map(|i| chars[i]);
    if w.ends_with("ss")
        || w.ends_with("sh")
        || w.ends_with("ch")
        || last == 'x'
        || last == 'z'
        || (last == 's' && !w.ends_with("us"))
    {
        if last == 's' && !w.ends_with("ss") {
            // Already plural ("posts").
            return w.to_owned();
        }
        return format!("{w}es");
    }
    if w.ends_with("us") {
        return format!("{w}es");
    }
    if last == 'y' && before.is_some_and(|b| !is_vowel(b)) {
        return format!("{}ies", &w[..w.len() - 1]);
    }
    format!("{w}s")
}

/// The singular of one lowercase word.
pub fn singularize_word(w: &str) -> String {
    if w.is_empty() || UNCOUNTABLE.contains(&w) {
        return w.to_owned();
    }
    if let Some((s, _)) = IRREGULAR.iter().find(|(_, p)| *p == w) {
        return (*s).to_owned();
    }
    if IRREGULAR.iter().any(|(s, _)| *s == w) {
        return w.to_owned();
    }
    if let Some(stem) = w.strip_suffix("ies")
        && !stem.is_empty()
    {
        return format!("{stem}y");
    }
    for suffix in ["sses", "shes", "ches", "xes"] {
        if let Some(stem) = w.strip_suffix(suffix) {
            return format!("{stem}{}", &suffix[..suffix.len() - 2]);
        }
    }
    if w.ends_with("ss") || w.ends_with("us") || w.ends_with("is") {
        return w.to_owned();
    }
    if let Some(stem) = w.strip_suffix('s')
        && !stem.is_empty()
    {
        return stem.to_owned();
    }
    w.to_owned()
}

/// `BlogPost`, `blogPost`, `blog-post`, `blog post` → `blog_post`.
pub fn snake(raw: &str) -> String {
    let mut out = String::new();
    let mut prev_lower = false;
    for c in raw.trim().chars() {
        if c.is_ascii_uppercase() {
            if prev_lower && !out.ends_with('_') {
                out.push('_');
            }
            out.push(c.to_ascii_lowercase());
            prev_lower = false;
        } else if c.is_ascii_alphanumeric() {
            out.push(c);
            prev_lower = c.is_ascii_lowercase() || c.is_ascii_digit();
        } else if !out.is_empty() && !out.ends_with('_') {
            out.push('_');
            prev_lower = false;
        }
    }
    out.trim_matches('_').to_owned()
}

/// `blog_post` → `BlogPost`.
pub fn camel(snake: &str) -> String {
    snake
        .split('_')
        .filter(|p| !p.is_empty())
        .map(|p| {
            let mut c = p.chars();
            match c.next() {
                Some(f) => f.to_ascii_uppercase().to_string() + c.as_str(),
                None => String::new(),
            }
        })
        .collect()
}

/// `blog_post` → `Blog post` (Rails' `humanize`).
pub fn human(snake: &str) -> String {
    let spaced = snake.replace('_', " ");
    let mut c = spaced.chars();
    match c.next() {
        Some(f) => f.to_ascii_uppercase().to_string() + c.as_str(),
        None => String::new(),
    }
}

/// Changes the last `_`-separated word of a snake name.
fn map_last(snake: &str, f: fn(&str) -> String) -> String {
    match snake.rsplit_once('_') {
        Some((head, last)) => format!("{head}_{}", f(last)),
        None => f(snake),
    }
}

pub fn pluralize(snake: &str) -> String {
    map_last(snake, pluralize_word)
}

pub fn singularize(snake: &str) -> String {
    map_last(snake, singularize_word)
}

/// Rust keywords (2024 edition, strict and reserved): never a module, field or function name.
pub const RUST_KEYWORDS: &[&str] = &[
    "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum", "extern",
    "false", "fn", "for", "gen", "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut",
    "pub", "ref", "return", "self", "static", "struct", "super", "trait", "true", "type", "unsafe",
    "use", "where", "while", "abstract", "become", "box", "do", "final", "macro", "override",
    "priv", "try", "typeof", "unsized", "virtual", "yield",
];

/// Every spelling of a resource name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Names {
    /// `blog_post`: the model module, factory, path helper (`paths::blog_post(id)`).
    pub singular: String,
    /// `blog_posts`: the table, controller, views and templates directory.
    pub plural: String,
    /// `BlogPost`: the model alias, `BlogPostParams`, `NewBlogPost`.
    pub model: String,
    /// `BlogPosts`: page structs (`BlogPostsIndex`).
    pub plural_camel: String,
    /// `blog-posts`: the URL segment.
    pub kebab: String,
    /// `Blog post`.
    pub human: String,
    /// `Blog posts`.
    pub human_plural: String,
    /// `blog post`.
    pub lower: String,
    /// `blog posts`.
    pub lower_plural: String,
}

impl Names {
    /// From whatever form was typed: `posts`, `post`, `Post`, `BlogPosts`, `blog-posts`.
    pub fn new(raw: &str) -> Result<Self> {
        let base = snake(raw);
        check_ident(raw, &base)?;
        let singular = singularize(&base);
        let plural = pluralize(&singular);
        if singular == plural {
            return Err(Fail::usage(format!(
                "'{raw}' has the same singular and plural form ({singular}); pick a countable name (e.g. {singular}_item)"
            )));
        }
        Ok(Self::from_parts(singular, plural))
    }

    /// For names that are not resources (a job, a mailer, a page): no plural check.
    pub fn plain(raw: &str) -> Result<Self> {
        let base = snake(raw);
        check_ident(raw, &base)?;
        Ok(Self::from_parts(base.clone(), base))
    }

    fn from_parts(singular: String, plural: String) -> Self {
        Self {
            model: camel(&singular),
            plural_camel: camel(&plural),
            kebab: plural.replace('_', "-"),
            human: human(&singular),
            human_plural: human(&plural),
            lower: singular.replace('_', " "),
            lower_plural: plural.replace('_', " "),
            singular,
            plural,
        }
    }
}

fn check_ident(raw: &str, base: &str) -> Result<()> {
    if base.is_empty() || base.starts_with(|c: char| c.is_ascii_digit()) {
        return Err(Fail::usage(format!(
            "'{raw}' is not a usable name: start with a letter, then letters, digits or _"
        )));
    }
    if RUST_KEYWORDS.contains(&base) {
        return Err(Fail::usage(format!(
            "'{raw}' is a Rust keyword; pick another name"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plurals_follow_rails() {
        for (s, p) in [
            ("post", "posts"),
            ("category", "categories"),
            ("day", "days"),
            ("box", "boxes"),
            ("class", "classes"),
            ("church", "churches"),
            ("person", "people"),
            ("status", "statuses"),
            ("quiz", "quizzes"),
            ("hero", "heroes"),
            ("photo", "photos"),
            ("leaf", "leaves"),
            ("address", "addresses"),
        ] {
            assert_eq!(pluralize_word(s), p, "{s}");
            assert_eq!(singularize_word(p), s, "{p}");
        }
        assert_eq!(pluralize_word("posts"), "posts");
        assert_eq!(singularize_word("news"), "news");
    }

    #[test]
    fn every_spelling_from_any_form() {
        for raw in [
            "blog_posts",
            "BlogPost",
            "blog-posts",
            "blogPosts",
            "BlogPosts",
            "blog_post",
        ] {
            let n = Names::new(raw).unwrap();
            assert_eq!(n.singular, "blog_post", "{raw}");
            assert_eq!(n.plural, "blog_posts");
            assert_eq!(n.model, "BlogPost");
            assert_eq!(n.plural_camel, "BlogPosts");
            assert_eq!(n.kebab, "blog-posts");
            assert_eq!(n.human, "Blog post");
            assert_eq!(n.human_plural, "Blog posts");
        }
        assert!(Names::new("type").is_err());
        assert!(Names::new("9lives").is_err());
        assert!(Names::new("news").is_err());
    }
}
