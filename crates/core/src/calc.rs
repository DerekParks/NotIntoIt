//! Arithmetic for tax lines: `+ - * /`, parentheses, percentages, and
//! `min`, `max`, `round`, `abs`. Numbers may carry `$` and thousands commas.

use std::fmt;

#[derive(Debug, Clone, PartialEq)]
pub struct CalcError(String);

impl fmt::Display for CalcError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for CalcError {}

pub fn evaluate(expression: &str) -> Result<f64, CalcError> {
    let tokens = tokenize(expression)?;
    let mut parser = Parser { tokens, pos: 0 };
    let value = parser.expr()?;
    if let Some(t) = parser.tokens.get(parser.pos) {
        return Err(CalcError(format!("unexpected {t:?} in {expression:?}")));
    }
    if !value.is_finite() {
        return Err(CalcError(format!("{expression:?} is not a finite number")));
    }
    Ok(value)
}

#[derive(Debug, Clone, PartialEq)]
enum Token {
    Num(f64),
    Ident(String),
    Op(char),
}

fn tokenize(s: &str) -> Result<Vec<Token>, CalcError> {
    let mut out = Vec::new();
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        match c {
            ' ' | '\t' | '$' => i += 1,
            '0'..='9' | '.' => {
                let start = i;
                while i < chars.len() && (chars[i].is_ascii_digit() || chars[i] == '.' || is_grouping_comma(&chars, i))
                {
                    i += 1;
                }
                let raw: String = chars[start..i].iter().filter(|c| **c != ',').collect();
                let n = raw.parse().map_err(|_| CalcError(format!("bad number {raw:?}")))?;
                out.push(Token::Num(n));
            }
            'a'..='z' | 'A'..='Z' => {
                let start = i;
                while i < chars.len() && chars[i].is_ascii_alphabetic() {
                    i += 1;
                }
                out.push(Token::Ident(chars[start..i].iter().collect::<String>().to_lowercase()));
            }
            '+' | '-' | '*' | '/' | '(' | ')' | '%' | ';' | ',' => {
                out.push(Token::Op(c));
                i += 1;
            }
            '×' => {
                out.push(Token::Op('*'));
                i += 1;
            }
            _ => return Err(CalcError(format!("unexpected character {c:?}"))),
        }
    }
    Ok(out)
}

/// A comma followed by exactly three digits groups thousands; any other comma separates arguments.
fn is_grouping_comma(chars: &[char], i: usize) -> bool {
    chars[i] == ','
        && chars
            .get(i + 1..i + 4)
            .is_some_and(|d| d.iter().all(char::is_ascii_digit))
        && !chars.get(i + 4).is_some_and(char::is_ascii_digit)
}

struct Parser {
    tokens: Vec<Token>,
    pos: usize,
}

impl Parser {
    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.pos)
    }

    fn eat(&mut self, op: char) -> bool {
        if self.peek() == Some(&Token::Op(op)) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn expr(&mut self) -> Result<f64, CalcError> {
        let mut v = self.term()?;
        loop {
            if self.eat('+') {
                v += self.term()?;
            } else if self.eat('-') {
                v -= self.term()?;
            } else {
                return Ok(v);
            }
        }
    }

    fn term(&mut self) -> Result<f64, CalcError> {
        let mut v = self.unary()?;
        loop {
            if self.eat('*') {
                v *= self.unary()?;
            } else if self.eat('/') {
                let d = self.unary()?;
                if d == 0.0 {
                    return Err(CalcError("division by zero".into()));
                }
                v /= d;
            } else {
                return Ok(v);
            }
        }
    }

    fn unary(&mut self) -> Result<f64, CalcError> {
        if self.eat('-') {
            return Ok(-self.unary()?);
        }
        if self.eat('+') {
            return self.unary();
        }
        let v = self.atom()?;
        Ok(if self.eat('%') { v / 100.0 } else { v })
    }

    fn atom(&mut self) -> Result<f64, CalcError> {
        match self.tokens.get(self.pos).cloned() {
            Some(Token::Num(n)) => {
                self.pos += 1;
                Ok(n)
            }
            Some(Token::Op('(')) => {
                self.pos += 1;
                let v = self.expr()?;
                self.expect(')')?;
                Ok(v)
            }
            Some(Token::Ident(name)) => {
                self.pos += 1;
                self.expect('(')?;
                let mut args = vec![self.expr()?];
                while self.eat(';') || self.eat(',') {
                    args.push(self.expr()?);
                }
                self.expect(')')?;
                call(&name, &args)
            }
            other => Err(CalcError(format!("expected a number, got {other:?}"))),
        }
    }

    fn expect(&mut self, op: char) -> Result<(), CalcError> {
        if self.eat(op) {
            Ok(())
        } else {
            Err(CalcError(format!("expected '{op}'")))
        }
    }
}

fn call(name: &str, args: &[f64]) -> Result<f64, CalcError> {
    match (name, args) {
        ("min", [_, ..]) => Ok(args.iter().copied().fold(f64::INFINITY, f64::min)),
        ("max", [_, ..]) => Ok(args.iter().copied().fold(f64::NEG_INFINITY, f64::max)),
        ("abs", [x]) => Ok(x.abs()),
        ("round", [x]) => Ok(round_half_up(*x, 0)),
        ("round", [x, digits]) => Ok(round_half_up(*x, *digits as i32)),
        _ => Err(CalcError(format!(
            "unknown function {name}/{} (have min, max, abs, round)",
            args.len()
        ))),
    }
}

/// IRS rounding: half a dollar (or cent) and up rounds away from zero.
pub fn round_half_up(x: f64, digits: i32) -> f64 {
    let scale = 10f64.powi(digits);
    let scaled = x * scale;
    // Nudge past binary representation error, e.g. 2.675 * 100 = 267.49999.
    (scaled + scaled.signum() * 1e-9).round() / scale
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arithmetic() {
        assert_eq!(evaluate("72,805 + 665 + 486 + 2,532").unwrap(), 76_488.0);
        assert_eq!(evaluate("$75,303 - 23,625").unwrap(), 51_678.0);
        assert_eq!(evaluate("1700 + 12% * (48725 - 17000)").unwrap(), 5507.0);
        assert_eq!(evaluate("-(3 - 5) * 2").unwrap(), 4.0);
        assert_eq!(evaluate("min(5000, 3000, 13200)").unwrap(), 3000.0);
        assert_eq!(evaluate("min(5,000,3,000)").unwrap(), 3000.0);
        assert_eq!(evaluate("max(1,2)").unwrap(), 2.0);
        assert_eq!(evaluate("max(0; 3000 - 5000)").unwrap(), 0.0);
        assert_eq!(evaluate("round(1184.52)").unwrap(), 1185.0);
        assert_eq!(evaluate("round(2.675, 2)").unwrap(), 2.68);
        assert_eq!(evaluate("round(-0.5)").unwrap(), -1.0);
        assert_eq!(evaluate("2 × 3").unwrap(), 6.0);
    }

    #[test]
    fn errors() {
        assert!(evaluate("1 / 0").is_err());
        assert!(evaluate("2 +").is_err());
        assert!(evaluate("(1 + 2").is_err());
        assert!(evaluate("sqrt(4)").is_err());
        assert!(evaluate("1 2").is_err());
        assert!(evaluate("rm -rf").is_err());
    }
}
