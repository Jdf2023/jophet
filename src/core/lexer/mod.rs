// src/core/lexer/mod.rs
//! The lexical analyzer (lexer or tokenizer) for the Jophet language.
//!
//! This module is responsible for the first phase of compilation: converting a raw
//! string of source code into a sequence of `Token`s. It scans the input character
//! by character, grouping them into meaningful units like keywords, identifiers,
//! literals, and operators. It now recognizes documentation comments (`///`) and module-level
//! doc comments (`//!`) as distinct tokens, while discarding regular single-line (`//`)
//! and multi-line (`/* ... */`) comments.
//!
//! It also recognizes the `const` keyword, which marks declarations for compile-time
//! evaluation during semantic analysis.

use crate::core::ast::{Span, Token, TokenKind};
use crate::diagnostics::errors::{JophetError, LexerError};
use std::path::PathBuf;

/// The state machine for the lexical analysis process.
struct Lexer<'a> {
    /// The full source code string.
    source: &'a str,
    /// An iterator over the characters of the source code and their byte indices.
    chars: std::str::CharIndices<'a>,
    /// The current character being processed.
    current_char: Option<(usize, char)>,
    /// The next character in the source (one character of lookahead).
    peeked_char: Option<(usize, char)>,
}

impl<'a> Lexer<'a> {
    /// Creates a new `Lexer` for the given source code.
    /// It initializes the character stream and pre-loads the first two characters
    /// to enable one-character lookahead.
    fn new(source: &'a str) -> Self {
        let mut lexer = Lexer {
            source,
            chars: source.char_indices(),
            current_char: None,
            peeked_char: None,
        };
        // Prime the lexer by advancing twice.
        lexer.advance();
        lexer.advance();
        lexer
    }

    /// Advances the character stream by one position.
    /// `current_char` becomes the previous `peeked_char`, and a new character is peeked.
    fn advance(&mut self) {
        self.current_char = self.peeked_char;
        self.peeked_char = self.chars.next();
    }

    /// Returns the byte position of the current character.
    fn current_pos(&self) -> usize {
        self.current_char.map_or(self.source.len(), |(pos, _)| pos)
    }

    /// Consumes whitespace (excluding newlines).
    fn skip_whitespace(&mut self) {
        while let Some((_, c)) = self.current_char {
            if c.is_whitespace() && c != '\n' {
                self.advance();
                continue;
            }
            break;
        }
    }

    /// A helper function to create a `Token` with a given kind and span.
    fn make_token(&self, kind: TokenKind, start: usize, end: usize) -> Token {
        Token {
            kind,
            span: Span::new(start, end),
        }
    }

    /// A helper for creating a single-character token and advancing the stream.
    fn make_single_char_token(&mut self, kind: TokenKind) -> Token {
        let start = self.current_pos();
        self.advance();
        self.make_token(kind, start, start + 1)
    }

    /// A helper for creating a multi-character token after advancing.
    fn make_token_from_kind(&mut self, kind: TokenKind) -> Token {
        let start = self.current_pos();
        self.advance();
        self.make_token(kind, start, self.current_pos())
    }

    /// Fetches the next token from the source stream.
    ///
    /// This is the main method of the lexer. It skips whitespace, then determines
    /// the token type based on the current character and calls the appropriate
    /// helper method to lex it.
    fn next_token(&mut self) -> Result<Token, LexerError> {
        loop {
            self.skip_whitespace();

            if let Some((_, '/')) = self.current_char {
                if let Some((_, '*')) = self.peeked_char {
                    self.lex_multi_line_comment()?;
                    continue;
                } else if let Some((_, '/')) = self.peeked_char {
                    if let Some(token) = self.lex_doc_or_line_comment()? {
                        return Ok(token);
                    } else {
                        continue;
                    }
                }
            }
            break;
        }

        let start_pos = self.current_pos();

        // Check for End-of-File.
        let Some((_, ch)) = self.current_char else {
            return Ok(self.make_token(TokenKind::Eof, start_pos, start_pos));
        };

        // Main dispatch logic based on the current character.
        let token = match ch {
            '\n' => self.make_single_char_token(TokenKind::Newline),
            '(' => self.make_single_char_token(TokenKind::LParen),
            ')' => self.make_single_char_token(TokenKind::RParen),
            '[' => self.make_single_char_token(TokenKind::LBracket),
            ']' => self.make_single_char_token(TokenKind::RBracket),
            ',' => self.make_single_char_token(TokenKind::Comma),
            '?' => self.make_single_char_token(TokenKind::Question),
            '~' => self.make_single_char_token(TokenKind::Tilde),
            '.' => {
                self.advance();
                if let Some((_, '.')) = self.current_char {
                    self.make_token_from_kind(TokenKind::DoubleDot)
                } else {
                    self.make_token(TokenKind::Dot, start_pos, start_pos + 1)
                }
            }
            ':' => {
                self.advance();
                if let Some((_, ':')) = self.current_char {
                    self.make_token_from_kind(TokenKind::DoubleColon)
                } else {
                    self.make_token(TokenKind::Colon, start_pos, start_pos + 1)
                }
            }
            c if c.is_ascii_digit() => self.lex_number(),
            c if c.is_alphabetic() || c == '_' => self.lex_identifier(),
            '"' => self.lex_string()?,
            '\'' => self.lex_char()?,
            _ => self.lex_operator()?,
        };

        Ok(token)
    }

    /// Lexes a multi-character operator.
    /// It handles single-character operators (e.g., `+`), two-character operators
    /// (e.g., `+=`, `==`), and three-character operators (e.g., `<<=`).
    fn lex_operator(&mut self) -> Result<Token, LexerError> {
        let start_pos = self.current_pos();
        let Some((_, c1)) = self.current_char else {
            return Err(LexerError::UnexpectedEof);
        };
        self.advance();
        let c2 = self.current_char.map(|(_, c)| c);

        let kind = match (c1, c2) {
            // Two-character assignment operators
            ('+', Some('=')) => TokenKind::PlusEquals,
            ('-', Some('=')) => TokenKind::MinusEquals,
            ('/', Some('=')) => TokenKind::SlashEquals,
            ('%', Some('=')) => TokenKind::PercentEquals,
            ('=', Some('=')) => TokenKind::EqualEqual,
            ('=', Some('>')) => TokenKind::FatArrow, // =>
            ('!', Some('=')) => TokenKind::BangEquals,
            ('<', Some('=')) => TokenKind::LessEquals,
            ('>', Some('=')) => TokenKind::GreaterEquals,
            ('&', Some('=')) => TokenKind::AmpersandEquals,
            ('|', Some('=')) => TokenKind::PipeEquals,
            ('^', Some('=')) => TokenKind::CaretEquals,

            // Two-character logical/bitwise operators
            ('&', Some('&')) => TokenKind::AmpersandAmpersand,
            ('|', Some('|')) => TokenKind::PipePipe,
            
            // Exponentiation operator
            ('*', Some('*')) => {
                self.advance();
                if let Some((_, '=')) = self.current_char {
                    TokenKind::AsteriskAsteriskEquals
                } else {
                    return Ok(self.make_token(TokenKind::AsteriskAsterisk, start_pos, self.current_pos()));
                }
            }
            ('*', Some('=')) => TokenKind::AsteriskEquals,

            // Three-character shift-assignment operators
            ('<', Some('<')) => {
                self.advance();
                if let Some((_, '=')) = self.current_char {
                    TokenKind::LessLessEquals
                } else {
                    return Ok(self.make_token(TokenKind::LessLess, start_pos, self.current_pos()));
                }
            }
            ('>', Some('>')) => {
                self.advance();
                if let Some((_, '=')) = self.current_char {
                    TokenKind::GreaterGreaterEquals
                } else {
                    return Ok(self.make_token(
                        TokenKind::GreaterGreater,
                        start_pos,
                        self.current_pos(),
                    ));
                }
            }

            // Fallback to single-character operators
            ('+', _) => return Ok(self.make_token(TokenKind::Plus, start_pos, start_pos + 1)),
            ('-', _) => return Ok(self.make_token(TokenKind::Minus, start_pos, start_pos + 1)),
            ('*', _) => return Ok(self.make_token(TokenKind::Asterisk, start_pos, start_pos + 1)),
            ('/', _) => return Ok(self.make_token(TokenKind::Slash, start_pos, start_pos + 1)),
            ('%', _) => return Ok(self.make_token(TokenKind::Percent, start_pos, start_pos + 1)),
            ('=', _) => return Ok(self.make_token(TokenKind::Equal, start_pos, start_pos + 1)),
            ('!', _) => return Ok(self.make_token(TokenKind::Bang, start_pos, start_pos + 1)),
            ('<', _) => return Ok(self.make_token(TokenKind::LAngle, start_pos, start_pos + 1)),
            ('>', _) => return Ok(self.make_token(TokenKind::RAngle, start_pos, start_pos + 1)),
            ('&', _) => {
                return Ok(self.make_token(TokenKind::Ampersand, start_pos, start_pos + 1))
            }
            ('|', _) => return Ok(self.make_token(TokenKind::Pipe, start_pos, start_pos + 1)),
            ('^', _) => return Ok(self.make_token(TokenKind::Caret, start_pos, start_pos + 1)),

            _ => {
                return Err(LexerError::UnexpectedCharacter {
                    char: c1,
                    span: Span::new(start_pos, start_pos + 1),
                })
            }
        };
        // If we matched a 2 or 3 character operator, advance past the final character.
        self.advance();
        Ok(self.make_token(kind, start_pos, self.current_pos()))
    }

    /// Lexes a number literal, which can be an integer or a float.
    fn lex_number(&mut self) -> Token {
        let start_pos = self.current_pos();
        // Consume leading digits.
        while let Some((_, c)) = self.current_char {
            if !c.is_ascii_digit() {
                break;
            }
            self.advance();
        }
        // Check for a decimal point followed by more digits to identify a float.
        if let (Some((_, '.')), Some((_, c))) = (self.current_char, self.peeked_char) {
            if c.is_ascii_digit() {
                self.advance(); // Consume the '.'
                while let Some((_, c)) = self.current_char {
                    if !c.is_ascii_digit() {
                        break;
                    }
                    self.advance();
                }
                // Parse as a float.
                let literal_str = &self.source[start_pos..self.current_pos()];
                let value = literal_str.parse().unwrap();
                return self.make_token(
                    TokenKind::FloatLiteral(value),
                    start_pos,
                    self.current_pos(),
                );
            }
        }
        // Otherwise, parse as an integer.
        let literal_str = &self.source[start_pos..self.current_pos()];
        let value = literal_str.parse().unwrap();
        self.make_token(TokenKind::IntLiteral(value), start_pos, self.current_pos())
    }

    /// Parses a single escape sequence and returns the resulting character.
    fn parse_escape_sequence(&mut self, quote_char: char) -> Result<char, LexerError> {
        let start_pos = self.current_pos();
        self.advance(); // Consume the backslash

        let Some((_, escaped_char)) = self.current_char else {
            return Err(LexerError::UnexpectedEof);
        };
        let start_escape_pos = self.current_pos();

        let result_char = match escaped_char {
            'n' => '\n',
            't' => '\t',
            'r' => '\r',
            '0' => '\0',
            '\\' => '\\',
            ch if ch == quote_char => quote_char,
            'x' => {
                self.advance();
                let mut hex_code = String::new();
                for _ in 0..2 {
                    if let Some((_, c)) = self.current_char {
                        if c.is_ascii_hexdigit() {
                            hex_code.push(c);
                            self.advance();
                        } else {
                            break;
                        }
                    }
                }
                if hex_code.len() != 2 {
                    return Err(LexerError::InvalidUnicodeEscape {
                        message: "Expected two hexadecimal digits after '\\x'".to_string(),
                        span: Span::new(start_escape_pos, self.current_pos()),
                    });
                }
                let value = u8::from_str_radix(&hex_code, 16).unwrap();
                return Ok(value as char);
            }
            'u' => {
                self.advance();
                if self.current_char != Some((start_escape_pos + 1, '{')) {
                    return Err(LexerError::InvalidUnicodeEscape {
                        message: "Expected '{' after '\\u' for unicode escape".to_string(),
                        span: Span::new(start_escape_pos, self.current_pos()),
                    });
                }
                self.advance(); // Consume '{'
                let mut unicode_code = String::new();
                while let Some((_, c)) = self.current_char {
                    if c == '}' {
                        break;
                    }
                    if c.is_ascii_hexdigit() {
                        unicode_code.push(c);
                        self.advance();
                    } else {
                        return Err(LexerError::InvalidUnicodeEscape {
                            message: "Invalid character in unicode escape sequence".to_string(),
                            span: Span::new(start_escape_pos, self.current_pos()),
                        });
                    }
                }
                if self.current_char.map(|(_, c)| c) != Some('}') {
                    return Err(LexerError::InvalidUnicodeEscape {
                        message: "Unterminated unicode escape sequence, missing '}'".to_string(),
                        span: Span::new(start_escape_pos, self.current_pos()),
                    });
                }
                let value = u32::from_str_radix(&unicode_code, 16).map_err(|_| {
                    LexerError::InvalidUnicodeEscape {
                        message: "Invalid unicode code point value".to_string(),
                        span: Span::new(start_escape_pos, self.current_pos()),
                    }
                })?;
                let ch = std::char::from_u32(value).ok_or_else(|| {
                    LexerError::InvalidUnicodeEscape {
                        message: format!("Invalid unicode scalar value: U+{:X}", value),
                        span: Span::new(start_escape_pos, self.current_pos()),
                    }
                })?;
                self.advance(); // Consume '}'
                return Ok(ch);
            }
            _ => {
                return Err(LexerError::InvalidEscapeSequence {
                    char: escaped_char,
                    span: Span::new(start_pos, self.current_pos() + 1),
                });
            }
        };
        self.advance();
        Ok(result_char)
    }

    /// Lexes a double-quoted string literal, handling escape sequences.
    fn lex_string(&mut self) -> Result<Token, LexerError> {
        let start_pos = self.current_pos();
        self.advance(); // Consume the opening `"`
        let mut content = String::new();
        while let Some((_, c)) = self.current_char {
            match c {
                '"' => {
                    self.advance(); // Consume the closing `"`
                    return Ok(self.make_token(
                        TokenKind::StringLiteral(content),
                        start_pos,
                        self.current_pos(),
                    ));
                }
                '\\' => {
                    let escaped = self.parse_escape_sequence('"')?;
                    content.push(escaped);
                }
                _ => {
                    content.push(c);
                    self.advance();
                }
            }
        }
        // If the loop finishes without finding a closing quote, the string is unterminated.
        Err(LexerError::UnterminatedString {
            span: Span::new(start_pos, self.current_pos()),
        })
    }

    /// Lexes a single-quoted character literal, handling escape sequences.
    fn lex_char(&mut self) -> Result<Token, LexerError> {
        let start_pos = self.current_pos();
        self.advance(); // Consume the opening `'`

        let char_val = match self.current_char {
            Some((_, '\\')) => self.parse_escape_sequence('\'')?,
            Some((_, '\'')) => {
                return Err(LexerError::UnterminatedChar {
                    span: Span::new(start_pos, self.current_pos()),
                });
            }
            Some((_, c)) => {
                self.advance();
                c
            }
            None => {
                return Err(LexerError::UnterminatedChar {
                    span: Span::new(start_pos, self.current_pos()),
                });
            }
        };

        if let Some((_, '\'')) = self.current_char {
            self.advance(); // Consume the closing `'`
            return Ok(self.make_token(
                TokenKind::CharLiteral(char_val),
                start_pos,
                self.current_pos(),
            ));
        }
        Err(LexerError::UnterminatedChar {
            span: Span::new(start_pos, self.current_pos()),
        })
    }

    /// Lexes an identifier or a keyword.
    /// It consumes a sequence of alphanumeric characters and underscores, then checks
    /// if the resulting string is a reserved keyword. If not, it's classified as
    /// an `Identifier` or a `Type` based on its starting case.
    fn lex_identifier(&mut self) -> Token {
        let start_pos = self.current_pos();
        while let Some((_, c)) = self.current_char {
            if !c.is_alphanumeric() && c != '_' {
                break;
            }
            self.advance();
        }
        let ident_str = &self.source[start_pos..self.current_pos()];

        // Match against keywords.
        let kind = match ident_str {
            "delete" => TokenKind::Delete,
            "new" => TokenKind::New,
            "public" => TokenKind::Public,
            "import" => TokenKind::Import,
            "function" => TokenKind::Function,
            "end" => TokenKind::End,
            "return" => TokenKind::Return,
            // Compile-time qualifier
            "const" => TokenKind::Const,
            "mutable" => TokenKind::Mutable,
            "if" => TokenKind::If,
            "else" => TokenKind::Else,
            "while" => TokenKind::While,
            "for" => TokenKind::For,
            "in" => TokenKind::In,
            "struct" => TokenKind::Struct,
            "implement" => TokenKind::Implement,
            "enum" => TokenKind::Enum,
            "union" => TokenKind::Union,
            "error" => TokenKind::Error,
            "try" => TokenKind::Try,
            "catch" => TokenKind::Catch,
            "switch" => TokenKind::Switch,
            "case" => TokenKind::Case,
            "of" => TokenKind::Of,
            "do" => TokenKind::Do,
            "yield" => TokenKind::Yield,
            "break" => TokenKind::Break,
            "continue" => TokenKind::Continue,
            "allow" => TokenKind::Allow,
            "trait" => TokenKind::Trait,
            "raw" => TokenKind::Raw,
            "self" => TokenKind::Identifier("self".to_string()),
            "true" => TokenKind::BoolLiteral(true),
            "false" => TokenKind::BoolLiteral(false),
            "nothing" => TokenKind::NothingLiteral,
            "Self" => TokenKind::Type("Self".to_string()),
            "Nothing" => TokenKind::Type("Nothing".to_string()),
            // Special handling for the two-word `tagged union` keyword.
            "tagged" => {
                self.skip_whitespace();
                let next_start = self.current_pos();

                // Look ahead without consuming to see if the next word is "union".
                let mut next_ident_end = next_start;
                let mut temp_chars = self.source[next_start..].chars();
                while let Some(c) = temp_chars.next() {
                    if !c.is_alphanumeric() && c != '_' {
                        break;
                    }
                    next_ident_end += c.len_utf8();
                }

                if &self.source[next_start..next_ident_end] == "union" {
                    // It is `tagged union`, so consume the `union` part.
                    while self.current_pos() < next_ident_end {
                        self.advance();
                    }
                    TokenKind::TaggedUnion
                } else {
                    // It's just the identifier "tagged".
                    TokenKind::Identifier("tagged".to_string())
                }
            }
            _ => {
                // If not a keyword, classify as Identifier or Type based on casing.
                let first_char = ident_str.chars().next().unwrap();
                if first_char.is_uppercase() && first_char.is_alphabetic() {
                    TokenKind::Type(ident_str.to_string())
                } else {
                    TokenKind::Identifier(ident_str.to_string())
                }
            }
        };
        self.make_token(kind, start_pos, self.current_pos())
    }
    
    /// Lexes a doc comment (`///`), a module-level doc comment (`//!`), or a regular line comment (`//`).
    /// If it's a doc comment, it returns a `DocComment` or `ModuleDocComment` token.
    /// If it's a line comment, it skips it and returns `None`.
    fn lex_doc_or_line_comment(&mut self) -> Result<Option<Token>, LexerError> {
        let start_pos = self.current_pos();
        self.advance(); // consume first '/'
        self.advance(); // consume second '/'

        let token_kind = match self.current_char {
            Some((_, '/')) => Some(TokenKind::DocComment("".into())), // Placeholder
            Some((_, '!')) => Some(TokenKind::ModuleDocComment("".into())), // Placeholder
            _ => None, // It's a regular line comment: //
        };

        if let Some(kind_template) = token_kind {
            self.advance(); // consume third char ('/' or '!')

            // Consume optional space after `///` or `//!`
            if let Some((_, ' ')) = self.current_char {
                self.advance();
            }
            let content_start_pos = self.current_pos();
            while let Some((_, c)) = self.current_char {
                if c == '\n' {
                    break;
                }
                self.advance();
            }
            let content = self.source[content_start_pos..self.current_pos()].to_string();
            let final_kind = match kind_template {
                TokenKind::DocComment(_) => TokenKind::DocComment(content),
                TokenKind::ModuleDocComment(_) => TokenKind::ModuleDocComment(content),
                _ => unreachable!(),
            };
            let token = self.make_token(
                final_kind,
                start_pos,
                self.current_pos(),
            );
            Ok(Some(token))
        } else {
            while let Some((_, c)) = self.current_char {
                if c == '\n' {
                    break;
                }
                self.advance();
            }
            Ok(None)
        }
    }

    /// Lexes and consumes a multi-line comment.
    /// Returns an error if the comment is not terminated before the end of the file.
    fn lex_multi_line_comment(&mut self) -> Result<(), LexerError> {
        let start_pos = self.current_pos();
        self.advance(); // consume '/'
        self.advance(); // consume '*'

        loop {
            // Check for end of file, which means the comment is unterminated.
            if self.current_char.is_none() {
                return Err(LexerError::UnterminatedMultiLineComment {
                    span: Span::new(start_pos, self.current_pos()),
                });
            }

            // Check for the closing '*/'.
            if let (Some((_, '*')), Some((_, '/'))) = (self.current_char, self.peeked_char) {
                self.advance(); // consume '*'
                self.advance(); // consume '/'
                return Ok(());
            }

            self.advance();
        }
    }
}

/// The public entry point to the lexer.
///
/// This function creates a `Lexer` and consumes it, collecting all tokens
/// into a vector until `Eof` is reached.
///
/// # Arguments
/// * `source` - The source code to tokenize.
///
// # Returns
/// A `Result` containing either a `Vec<Token>` on success or a `JophetError`
/// wrapping a `LexerError` on failure.
pub fn tokenize(source: &str, file_path: PathBuf) -> Result<Vec<Token>, JophetError> {
    let mut lexer = Lexer::new(source);
    let mut tokens = Vec::new();
    loop {
        let token = lexer.next_token().map_err(|error| JophetError::LexerError {
            error,
            file_path: file_path.clone(),
        })?;
        if token.kind == TokenKind::Eof {
            tokens.push(token);
            break;
        }
        tokens.push(token);
    }
    Ok(tokens)
}