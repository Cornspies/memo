//! Code highlighting with syntect's bundled grammars. Scopes are reduced to a few token kinds, so
//! the web CSS and the Android theme each colour them in their own palette.

use std::sync::OnceLock;

use serde::Serialize;
use syntect::{
    parsing::{ParseState, Scope, ScopeStack, SyntaxSet},
    util::LinesWithEndings,
};

/// Longer code is shown plain.
const MAX_LEN: usize = 64 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Token {
    Comment,
    String,
    Number,
    Constant,
    Keyword,
    Function,
    Type,
}

impl Token {
    /// The class [`to_html`](super::to_html) gives the token's `<span>`.
    pub fn class(self) -> &'static str {
        match self {
            Token::Comment => "hl-comment",
            Token::String => "hl-string",
            Token::Number => "hl-number",
            Token::Constant => "hl-constant",
            Token::Keyword => "hl-keyword",
            Token::Function => "hl-function",
            Token::Type => "hl-type",
        }
    }
}

/// Names people write after the fence that the bundled grammars do not know, mapped to the
/// closest one they do.
const ALIASES: [(&str, &str); 14] = [
    ("javascript", "js"),
    ("typescript", "js"),
    ("ts", "js"),
    ("tsx", "js"),
    ("jsx", "js"),
    ("kotlin", "java"),
    ("kt", "java"),
    ("csharp", "cs"),
    ("c#", "cs"),
    ("shell", "sh"),
    ("bash", "sh"),
    ("zsh", "sh"),
    ("console", "sh"),
    ("golang", "go"),
];

/// Runs of `code` with their token kind, or `None` if the language is unknown.
pub fn highlight<'a>(code: &'a str, language: &str) -> Option<Vec<(Option<Token>, &'a str)>> {
    if code.len() > MAX_LEN {
        return None;
    }
    let syntaxes = syntaxes();
    let language = language.to_ascii_lowercase();
    let name = ALIASES.iter().find(|(alias, _)| *alias == language).map_or(language.as_str(), |(_, to)| to);
    let syntax = syntaxes.find_syntax_by_token(name)?;
    let rules = rules();
    let mut state = ParseState::new(syntax);
    let mut stack = ScopeStack::new();
    // Byte ranges of `code`; adjacent runs of one kind become one.
    let mut runs: Vec<(Option<Token>, usize, usize)> = Vec::new();
    let mut push = |token: Option<Token>, start: usize, end: usize| {
        if start >= end {
            return;
        }
        match runs.last_mut() {
            Some((last, _, last_end)) if *last == token && *last_end == start => *last_end = end,
            _ => runs.push((token, start, end)),
        }
    };
    let mut offset = 0;
    for line in LinesWithEndings::from(code) {
        let ops = state.parse_line(line, syntaxes).ok()?;
        let mut pos = 0;
        for (at, op) in ops {
            let at = at.clamp(pos, line.len());
            push(classify(&stack, rules), offset + pos, offset + at);
            pos = at;
            stack.apply(&op).ok()?;
        }
        push(classify(&stack, rules), offset + pos, offset + line.len());
        offset += line.len();
    }
    Some(runs.into_iter().map(|(token, start, end)| (token, &code[start..end])).collect())
}

fn syntaxes() -> &'static SyntaxSet {
    static SYNTAXES: OnceLock<SyntaxSet> = OnceLock::new();
    SYNTAXES.get_or_init(SyntaxSet::load_defaults_newlines)
}

/// Scope prefixes and what they mean, checked from the innermost scope outwards. `None` stops
/// there (plain text); a scope matching no rule passes on to its parent.
fn rules() -> &'static [(Scope, Option<Token>)] {
    static RULES: OnceLock<Vec<(Scope, Option<Token>)>> = OnceLock::new();
    RULES.get_or_init(|| {
        [
            ("comment", Some(Token::Comment)),
            ("string", Some(Token::String)),
            ("constant.numeric", Some(Token::Number)),
            ("constant.character.escape", Some(Token::String)),
            ("constant", Some(Token::Constant)),
            ("support.constant", Some(Token::Constant)),
            ("variable.language", Some(Token::Constant)),
            ("keyword.operator", None),
            ("keyword", Some(Token::Keyword)),
            ("storage", Some(Token::Keyword)),
            ("entity.name.tag", Some(Token::Keyword)),
            ("entity.name.function", Some(Token::Function)),
            ("support.function", Some(Token::Function)),
            ("variable.function", Some(Token::Function)),
            ("entity.other.attribute-name", Some(Token::Function)),
            ("entity.name.type", Some(Token::Type)),
            ("entity.name.class", Some(Token::Type)),
            ("entity.name.struct", Some(Token::Type)),
            ("entity.name.enum", Some(Token::Type)),
            ("entity.name.trait", Some(Token::Type)),
            ("entity.other.inherited-class", Some(Token::Type)),
            ("support.type", Some(Token::Type)),
            ("support.class", Some(Token::Type)),
        ]
        .into_iter()
        .map(|(scope, token)| (Scope::new(scope).expect("valid scope"), token))
        .collect()
    })
}

fn classify(stack: &ScopeStack, rules: &[(Scope, Option<Token>)]) -> Option<Token> {
    for &scope in stack.as_slice().iter().rev() {
        if let Some((_, token)) = rules.iter().find(|(prefix, _)| prefix.is_prefix_of(scope)) {
            return *token;
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tokens(code: &str, language: &str) -> Vec<(Option<Token>, String)> {
        highlight(code, language).unwrap().into_iter().map(|(t, s)| (t, s.to_owned())).collect()
    }

    #[test]
    fn rust_keywords_strings_comments() {
        let code = "fn main() {\n    let x = \"hi\"; // note\n    42\n}";
        let runs = tokens(code, "rust");
        assert_eq!(runs.iter().map(|(_, s)| s.as_str()).collect::<String>(), code);
        let kind = |text: &str| runs.iter().find(|(_, s)| s.contains(text)).unwrap().0;
        assert_eq!(kind("fn"), Some(Token::Keyword));
        assert_eq!(kind("main"), Some(Token::Function));
        assert_eq!(kind("\"hi\""), Some(Token::String));
        assert_eq!(kind("// note"), Some(Token::Comment));
        assert_eq!(kind("42"), Some(Token::Number));
    }

    #[test]
    fn aliases_and_unknown_languages() {
        assert!(highlight("const x = 1", "TypeScript").is_some());
        assert!(highlight("echo hi", "bash").is_some());
        assert!(highlight("x", "no-such-language").is_none());
        assert!(highlight("x", "").is_none());
    }
}
