//! Hand written Oberon-07 lexer.
//!
//! Why not LALRPOP's built in (regex) lexer: Oberon comments nest, i.e.
//! `(* a (* b *) c *)` is one comment.  That is not a regular language, so the
//! built in lexer (whose `match { ... }` ignore patterns are regular
//! expressions) cannot express it.  Everything else in Oberon's vocabulary is
//! regular; the language is small enough that a by-hand scanner is cheap and
//! gives exact control over the two lexical near-misses:
//!
//!   * `22X`  is the character with ordinal 0x22, not the number 22 followed by
//!     an identifier `X`;
//!   * `1..5` is the integer 1, the range operator and the integer 5, not the
//!     real `1.` followed by `.5`;
//!   * `100H` / `0FFH` are hexadecimal integers.
//!
//! The lexer emits `Result<(usize, Token, usize), LexicalError>` items, which is
//! the stream shape LALRPOP expects from a custom lexer (`Spanned`).

use std::fmt;

pub type Spanned<Tok, Loc, Error> = Result<(Loc, Tok, Loc), Error>;

#[derive(Debug, Clone, PartialEq)]
pub enum LexicalError {
    IllegalCharacter(char, usize),
    UnterminatedComment(usize),
    UnterminatedString(usize),
    /// A numeral the target's INTEGER cannot hold: substituting zero would
    /// silently change a constant, a bound or an index.
    NumberOutOfRange(String, usize),
    /// A real numeral that is not representable as a 32-bit `REAL`.
    BadReal(String, usize),
}

impl LexicalError {
    /// The byte span this error covers, so it can be reported like any other
    /// diagnostic.
    pub fn span(&self) -> (usize, usize) {
        let at = match self {
            LexicalError::IllegalCharacter(_, off)
            | LexicalError::UnterminatedComment(off)
            | LexicalError::UnterminatedString(off)
            | LexicalError::NumberOutOfRange(_, off)
            | LexicalError::BadReal(_, off) => *off,
        };
        let width = match self {
            LexicalError::IllegalCharacter(c, _) => c.len_utf8(),
            LexicalError::NumberOutOfRange(text, _) | LexicalError::BadReal(text, _) => text.len(),
            LexicalError::UnterminatedComment(_) | LexicalError::UnterminatedString(_) => 1,
        };
        (at, at + width.max(1))
    }
}

impl fmt::Display for LexicalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LexicalError::IllegalCharacter(c, _) => {
                write!(f, "illegal character {c:?}")
            }
            LexicalError::UnterminatedComment(_) => write!(f, "unterminated comment"),
            LexicalError::UnterminatedString(_) => write!(f, "unterminated string"),
            LexicalError::NumberOutOfRange(text, _) => {
                write!(f, "`{text}` is outside the range of INTEGER (report 3, 6.1)")
            }
            LexicalError::BadReal(text, _) => {
                write!(f, "`{text}` is not a representable REAL (report 6.1)")
            }
        }
    }
}

impl LexicalError {
    /// What the reader should do about it, when that is not obvious.
    pub fn note(&self) -> &'static str {
        match self {
            LexicalError::IllegalCharacter(..) => "not part of Oberon's vocabulary (report 3)",
            LexicalError::UnterminatedComment(_) => "comments nest and must be closed (report 3)",
            LexicalError::UnterminatedString(_) => {
                "a string cannot contain its closing quote (report 3)"
            }
            LexicalError::NumberOutOfRange(..) => "INTEGER holds 32 bits (report 6.1)",
            LexicalError::BadReal(..) => "REAL is a 32-bit float (report 6.1)",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Token<'input> {
    Ident(&'input str),
    Integer(u32),
    Real(f64),
    StrLit(String),
    CharLit(u8),
    /// `EXIT` (PLAN.md W8.5).
    Exit,

    Module,
    Import,
    Const,
    Type,
    Var,
    Procedure,
    Begin,
    End,
    Return,
    If,
    Then,
    Elsif,
    Else,
    Case,
    Of,
    While,
    Do,
    Repeat,
    Until,
    For,
    By,
    To,
    Array,
    Record,
    Pointer,
    Div,
    Mod,
    Or,
    In,
    Is,
    Nil,
    True,
    False,

    Assign,
    Caret,
    Dot,
    DotDot,
    Comma,
    Semi,
    Colon,
    Bar,
    Plus,
    Minus,
    Star,
    Slash,
    Amp,
    Tilde,
    EqOp,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    LParen,
    RParen,
    LBrack,
    RBrack,
    LBrace,
    RBrace,
}

impl fmt::Display for Token<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Token::Ident(s) => write!(f, "{s}"),
            Token::Integer(v) => write!(f, "{v}"),
            Token::Real(v) => write!(f, "{v}"),
            Token::StrLit(s) => write!(f, "\"{s}\""),
            Token::CharLit(v) => write!(f, "{v:02X}X"),
            Token::Assign => write!(f, ":="),
            Token::Caret => write!(f, "^"),
            Token::Dot => write!(f, "."),
            Token::DotDot => write!(f, ".."),
            Token::Comma => write!(f, ","),
            Token::Semi => write!(f, ";"),
            Token::Colon => write!(f, ":"),
            Token::Bar => write!(f, "|"),
            Token::Plus => write!(f, "+"),
            Token::Minus => write!(f, "-"),
            Token::Star => write!(f, "*"),
            Token::Slash => write!(f, "/"),
            Token::Amp => write!(f, "&"),
            Token::Tilde => write!(f, "~"),
            Token::EqOp => write!(f, "="),
            Token::Ne => write!(f, "#"),
            Token::Lt => write!(f, "<"),
            Token::Le => write!(f, "<="),
            Token::Gt => write!(f, ">"),
            Token::Ge => write!(f, ">="),
            Token::LParen => write!(f, "("),
            Token::RParen => write!(f, ")"),
            Token::LBrack => write!(f, "["),
            Token::RBrack => write!(f, "]"),
            Token::LBrace => write!(f, "{{"),
            Token::RBrace => write!(f, "}}"),
            other => {
                for (kw, tok) in KEYWORDS {
                    if tok == other {
                        return write!(f, "{kw}");
                    }
                }
                write!(f, "?")
            }
        }
    }
}

/// The report's spelling of a reserved word, when `t` is one: what a
/// case-folding dialect needs to explain that an identifier is not one here
/// (PLAN.md W8.10).
pub fn keyword_spelling(t: &Token<'_>) -> Option<&'static str> {
    KEYWORDS.iter().find(|(_, tok)| tok == t).map(|(kw, _)| *kw)
}

/// Reserved words, with the spelling the report fixes (all upper case).
const KEYWORDS: &[(&str, Token<'static>)] = &[
    ("ARRAY", Token::Array),
    ("BEGIN", Token::Begin),
    ("BY", Token::By),
    ("CASE", Token::Case),
    ("CONST", Token::Const),
    ("DIV", Token::Div),
    ("DO", Token::Do),
    ("ELSE", Token::Else),
    ("ELSIF", Token::Elsif),
    ("END", Token::End),
    ("EXIT", Token::Exit),
    ("FALSE", Token::False),
    ("FOR", Token::For),
    ("IF", Token::If),
    ("IMPORT", Token::Import),
    ("IN", Token::In),
    ("IS", Token::Is),
    ("MOD", Token::Mod),
    ("MODULE", Token::Module),
    ("NIL", Token::Nil),
    ("OF", Token::Of),
    ("OR", Token::Or),
    ("POINTER", Token::Pointer),
    ("PROCEDURE", Token::Procedure),
    ("RECORD", Token::Record),
    ("REPEAT", Token::Repeat),
    ("RETURN", Token::Return),
    ("THEN", Token::Then),
    ("TO", Token::To),
    ("TRUE", Token::True),
    ("TYPE", Token::Type),
    ("UNTIL", Token::Until),
    ("VAR", Token::Var),
    ("WHILE", Token::While),
];

pub struct Lexer<'input> {
    input: &'input str,
    b: &'input [u8],
    pos: usize,
    /// Accept reserved words spelled in lower case (PLAN.md W8.10). Off by
    /// default, because the report makes capital and lower-case letters distinct
    /// (report §3), so a *report* program that uses `Begin` as an identifier would
    /// tokenise differently — which is what makes this the one extension that
    /// needs a switch rather than being additive.
    fold_keywords: bool,
}

impl<'input> Lexer<'input> {
    /// The report's lexer: reserved words are recognised in the spelling the
    /// report fixes, and nothing else.
    pub fn new(input: &'input str) -> Self {
        Lexer {
            input,
            b: input.as_bytes(),
            pos: 0,
            fold_keywords: false,
        }
    }

    /// The same, folding reserved-word case (PLAN.md W8.10, `-Olower`).
    pub fn with_folded_keywords(input: &'input str) -> Self {
        Lexer {
            input,
            b: input.as_bytes(),
            pos: 0,
            fold_keywords: true,
        }
    }

    fn at(&self, i: usize) -> u8 {
        if i < self.b.len() {
            self.b[i]
        } else {
            0
        }
    }

    /// Blanks, line breaks and (nested) comments, per report ch. 3.
    fn skip_trivia(&mut self) -> Result<(), LexicalError> {
        loop {
            match self.at(self.pos) {
                b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c => self.pos += 1,
                b'(' if self.at(self.pos + 1) == b'*' => {
                    let start = self.pos;
                    self.pos += 2;
                    let mut depth = 1usize;
                    while depth > 0 {
                        if self.pos >= self.b.len() {
                            return Err(LexicalError::UnterminatedComment(start));
                        }
                        if self.at(self.pos) == b'(' && self.at(self.pos + 1) == b'*' {
                            depth += 1;
                            self.pos += 2;
                        } else if self.at(self.pos) == b'*' && self.at(self.pos + 1) == b')' {
                            depth -= 1;
                            self.pos += 2;
                        } else {
                            self.pos += 1;
                        }
                    }
                }
                _ => return Ok(()),
            }
        }
    }

    fn is_ident_start(c: u8) -> bool {
        c.is_ascii_alphabetic()
    }
    fn is_ident_cont(c: u8) -> bool {
        c.is_ascii_alphanumeric()
    }
    fn hex_val(c: u8) -> Option<u32> {
        match c {
            b'0'..=b'9' => Some((c - b'0') as u32),
            b'A'..=b'F' => Some((c - b'A') as u32 + 10),
            _ => None,
        }
    }

    fn scan_number(&mut self) -> Spanned<Token<'input>, usize, LexicalError> {
        let start = self.pos;
        // Longest run of '0'..'9' and 'A'..'F' decides between integer and
        // hexadecimal integer; a trailing 'X' makes it a character constant.
        let mut p = self.pos;
        while Self::hex_val(self.at(p)).is_some() {
            p += 1;
        }
        let digits_end = p;
        match self.at(p) {
            b'H' => {
                let text = &self.input[start..digits_end];
                self.pos = p + 1;
                return match u32::from_str_radix(text, 16) {
                    Ok(v) => Ok((start, Token::Integer(v), self.pos)),
                    Err(_) => Err(LexicalError::NumberOutOfRange(text.to_string(), start)),
                };
            }
            b'X' => {
                let text = &self.input[start..digits_end];
                self.pos = p + 1;
                // A character constant is a byte: `100X` has no character.
                return match u32::from_str_radix(text, 16) {
                    Ok(v) if v <= 0xFF => Ok((start, Token::CharLit(v as u8), self.pos)),
                    _ => Err(LexicalError::NumberOutOfRange(text.to_string(), start)),
                };
            }
            _ => {}
        }
        // decimal part: only 0..9 count
        let mut digits_end = start;
        while self.at(digits_end).is_ascii_digit() {
            digits_end += 1;
        }
        let int_text = &self.input[start..digits_end];
        // `1..5` must not lex as a real.
        if self.at(digits_end) == b'.' && self.at(digits_end + 1) != b'.' {
            let mut q = digits_end + 1;
            while self.at(q).is_ascii_digit() {
                q += 1;
            }
            if self.at(q) == b'E' {
                let mut r = q + 1;
                if self.at(r) == b'+' || self.at(r) == b'-' {
                    r += 1;
                }
                if self.at(r).is_ascii_digit() {
                    while self.at(r).is_ascii_digit() {
                        r += 1;
                    }
                    let text = &self.input[start..r];
                    self.pos = r;
                    return match text.parse::<f64>() {
                        // The emitted REAL is a C float: a value the target
                        // cannot hold is a diagnostic, not a silent zero.
                        Ok(v) if v.is_finite() && v.abs() <= f32::MAX as f64 => {
                            Ok((start, Token::Real(v), self.pos))
                        }
                        _ => Err(LexicalError::BadReal(text.to_string(), start)),
                    };
                }
            }
            let text = &self.input[start..q];
            self.pos = q;
            return match text.parse::<f64>() {
                Ok(v) if v.is_finite() && v.abs() <= f32::MAX as f64 => {
                    Ok((start, Token::Real(v), self.pos))
                }
                _ => Err(LexicalError::BadReal(text.to_string(), start)),
            };
        }
        self.pos = digits_end;
        match int_text.parse::<u32>() {
            Ok(v) => Ok((start, Token::Integer(v), self.pos)),
            Err(_) => Err(LexicalError::NumberOutOfRange(int_text.to_string(), start)),
        }
    }
}

impl<'input> Iterator for Lexer<'input> {
    type Item = Spanned<Token<'input>, usize, LexicalError>;

    fn next(&mut self) -> Option<Self::Item> {
        if let Err(e) = self.skip_trivia() {
            self.pos = self.b.len();
            return Some(Err(e));
        }
        let start = self.pos;
        if start >= self.b.len() {
            return None;
        }
        let c = self.b[start];
        // identifiers and reserved words
        if Self::is_ident_start(c) {
            let mut p = start;
            while Self::is_ident_cont(self.at(p)) {
                p += 1;
            }
            let text = &self.input[start..p];
            self.pos = p;
            for (kw, tok) in KEYWORDS {
                let hit = if self.fold_keywords {
                    kw.eq_ignore_ascii_case(text)
                } else {
                    *kw == text
                };
                if hit {
                    return Some(Ok((start, tok.clone(), self.pos)));
                }
            }
            return Some(Ok((start, Token::Ident(text), self.pos)));
        }
        if c.is_ascii_digit() {
            return Some(self.scan_number());
        }
        if c == b'"' {
            let mut p = start + 1;
            while p < self.b.len() && self.b[p] != b'"' {
                p += 1;
            }
            if p >= self.b.len() {
                self.pos = self.b.len();
                return Some(Err(LexicalError::UnterminatedString(start)));
            }
            let text = self.input[start + 1..p].to_string();
            self.pos = p + 1;
            return Some(Ok((start, Token::StrLit(text), self.pos)));
        }
        let two = self.at(start + 1);
        let (tok, len) = match (c, two) {
            (b':', b'=') => (Token::Assign, 2),
            (b'.', b'.') => (Token::DotDot, 2),
            (b'<', b'=') => (Token::Le, 2),
            (b'>', b'=') => (Token::Ge, 2),
            (b'.', _) => (Token::Dot, 1),
            (b'^', _) => (Token::Caret, 1),
            (b',', _) => (Token::Comma, 1),
            (b';', _) => (Token::Semi, 1),
            (b':', _) => (Token::Colon, 1),
            (b'|', _) => (Token::Bar, 1),
            (b'+', _) => (Token::Plus, 1),
            (b'-', _) => (Token::Minus, 1),
            (b'*', _) => (Token::Star, 1),
            (b'/', _) => (Token::Slash, 1),
            (b'&', _) => (Token::Amp, 1),
            (b'~', _) => (Token::Tilde, 1),
            (b'=', _) => (Token::EqOp, 1),
            (b'#', _) => (Token::Ne, 1),
            (b'<', _) => (Token::Lt, 1),
            (b'>', _) => (Token::Gt, 1),
            (b'(', _) => (Token::LParen, 1),
            (b')', _) => (Token::RParen, 1),
            (b'[', _) => (Token::LBrack, 1),
            (b']', _) => (Token::RBrack, 1),
            (b'{', _) => (Token::LBrace, 1),
            (b'}', _) => (Token::RBrace, 1),
            _ => {
                let ch = self.input[start..].chars().next().unwrap();
                self.pos = start + ch.len_utf8();
                return Some(Err(LexicalError::IllegalCharacter(ch, start)));
            }
        };
        self.pos = start + len;
        Some(Ok((start, tok, self.pos)))
    }
}

/// Convenience for tests / the driver.
pub fn tokenize(input: &str) -> Vec<Spanned<Token<'_>, usize, LexicalError>> {
    Lexer::new(input).collect()
}
