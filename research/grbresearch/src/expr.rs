//! Address expressions, Cheat Engine style: `7FF6A000`, `0x1234`, `exe+0x1234`, `ntdll+0x10`,
//! `[exe+0x63A0D20]+0x490`, `[[exe+0x63EDAA0]+0x1F0]+0x18`, `$name` (a remembered result), with +
//! and -. Plain numbers are always hex. `[x]` reads the 8-byte pointer at x.

use crate::proc::ReadError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Expr {
    Num(u64),
    /// A module base by name; "exe" is the main module.
    Module(String),
    /// A remembered result, `$name`.
    Var(String),
    /// `[inner]`: the pointer stored at inner.
    Deref(Box<Expr>),
    Add(Box<Expr>, Box<Expr>),
    Sub(Box<Expr>, Box<Expr>),
}

impl std::fmt::Display for Expr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Expr::Num(n) => write!(f, "0x{n:X}"),
            Expr::Module(m) => write!(f, "{m}"),
            Expr::Var(v) => write!(f, "${v}"),
            Expr::Deref(e) => write!(f, "[{e}]"),
            Expr::Add(a, b) => write!(f, "{a}+{b}"),
            Expr::Sub(a, b) => write!(f, "{a}-{b}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Tok {
    Open,
    Close,
    Plus,
    Minus,
    Word(String),
}

fn lex(text: &str) -> Result<Vec<Tok>, String> {
    let mut out = Vec::new();
    let mut word = String::new();
    let flush = |word: &mut String, out: &mut Vec<Tok>| {
        if !word.is_empty() {
            out.push(Tok::Word(std::mem::take(word)));
        }
    };
    for c in text.chars() {
        match c {
            '[' | ']' | '+' | '-' => {
                flush(&mut word, &mut out);
                out.push(match c {
                    '[' => Tok::Open,
                    ']' => Tok::Close,
                    '+' => Tok::Plus,
                    _ => Tok::Minus,
                });
            }
            c if c.is_whitespace() => flush(&mut word, &mut out),
            c if c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '$') => word.push(c),
            _ => return Err(format!("unexpected '{c}' in address")),
        }
    }
    flush(&mut word, &mut out);
    Ok(out)
}

fn atom(w: &str) -> Result<Expr, String> {
    if let Some(v) = w.strip_prefix('$') {
        if v.is_empty() {
            return Err("empty $name".into());
        }
        return Ok(Expr::Var(v.to_string()));
    }
    let hex = w.strip_prefix("0x").or_else(|| w.strip_prefix("0X"));
    if let Some(h) = hex {
        return u64::from_str_radix(h, 16).map(Expr::Num).map_err(|_| format!("bad hex number '{w}'"));
    }
    if w.chars().all(|c| c.is_ascii_hexdigit()) {
        return u64::from_str_radix(w, 16).map(Expr::Num).map_err(|_| format!("number too big '{w}'"));
    }
    if w.chars().next().is_some_and(|c| c.is_ascii_digit()) && !w.contains('.') {
        return Err(format!("'{w}' is neither hex nor a module name"));
    }
    Ok(Expr::Module(w.to_string()))
}

struct Parser {
    toks: Vec<Tok>,
    pos: usize,
}

impl Parser {
    fn sum(&mut self) -> Result<Expr, String> {
        let mut left = self.term()?;
        loop {
            match self.toks.get(self.pos) {
                Some(Tok::Plus) => {
                    self.pos += 1;
                    left = Expr::Add(Box::new(left), Box::new(self.term()?));
                }
                Some(Tok::Minus) => {
                    self.pos += 1;
                    left = Expr::Sub(Box::new(left), Box::new(self.term()?));
                }
                _ => return Ok(left),
            }
        }
    }

    fn term(&mut self) -> Result<Expr, String> {
        let t = self.toks.get(self.pos).cloned();
        self.pos += 1;
        match t {
            Some(Tok::Open) => {
                let inner = self.sum()?;
                if self.toks.get(self.pos) != Some(&Tok::Close) {
                    return Err("missing ']'".into());
                }
                self.pos += 1;
                Ok(Expr::Deref(Box::new(inner)))
            }
            Some(Tok::Word(w)) => atom(&w),
            Some(Tok::Close) => Err("unexpected ']'".into()),
            Some(Tok::Plus) | Some(Tok::Minus) => Err("two signs in a row".into()),
            None => Err("address ends too early".into()),
        }
    }
}

pub fn parse(text: &str) -> Result<Expr, String> {
    let toks = lex(text)?;
    if toks.is_empty() {
        return Err("empty address".into());
    }
    let mut p = Parser { toks, pos: 0 };
    let e = p.sum()?;
    if p.pos != p.toks.len() {
        return Err("extra text after the address (unbalanced ']'?)".into());
    }
    Ok(e)
}

/// What an expression needs from the outside world.
pub trait Env {
    /// (module name as loaded, base).
    fn module(&self, name: &str) -> Option<(String, u64)>;
    fn var(&self, name: &str) -> Option<u64>;
    fn read_ptr(&self, addr: u64) -> Result<u64, ReadError>;
}

/// One pointer read along the way.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step {
    /// The bracketed expression that was read, e.g. "[exe+0x63A0D20]".
    pub text: String,
    pub addr: u64,
    pub result: Result<u64, ReadError>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EvalError {
    UnknownModule(String),
    UnknownVar(String),
    Overflow(String),
    /// A pointer read failed at this step (the last one in the step list).
    Read(Step),
}

impl std::fmt::Display for EvalError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EvalError::UnknownModule(m) => write!(f, "no module named '{m}' (try `modules`)"),
            EvalError::UnknownVar(v) => write!(f, "nothing remembered as ${v}"),
            EvalError::Overflow(t) => write!(f, "address overflows at {t}"),
            EvalError::Read(s) => match s.result {
                Ok(v) => write!(f, "{} at 0x{:X} = 0x{v:X}", s.text, s.addr),
                Err(e) => write!(f, "{} at 0x{:X} failed: {e}", s.text, s.addr),
            },
        }
    }
}

/// Evaluate, recording every pointer read in `steps` (also the failing one).
pub fn eval(e: &Expr, env: &dyn Env, steps: &mut Vec<Step>) -> Result<u64, EvalError> {
    match e {
        Expr::Num(n) => Ok(*n),
        Expr::Module(m) => env.module(m).map(|(_, b)| b).ok_or_else(|| EvalError::UnknownModule(m.clone())),
        Expr::Var(v) => env.var(v).ok_or_else(|| EvalError::UnknownVar(v.clone())),
        Expr::Deref(inner) => {
            let addr = eval(inner, env, steps)?;
            let result = env.read_ptr(addr);
            let step = Step { text: e.to_string(), addr, result };
            steps.push(step.clone());
            result.map_err(|_| EvalError::Read(step))
        }
        Expr::Add(a, b) => {
            let (x, y) = (eval(a, env, steps)?, eval(b, env, steps)?);
            x.checked_add(y).ok_or_else(|| EvalError::Overflow(e.to_string()))
        }
        Expr::Sub(a, b) => {
            let (x, y) = (eval(a, env, steps)?, eval(b, env, steps)?);
            x.checked_sub(y).ok_or_else(|| EvalError::Overflow(e.to_string()))
        }
    }
}

/// Does the expression read memory at all (contains brackets)?
pub fn has_deref(e: &Expr) -> bool {
    match e {
        Expr::Deref(_) => true,
        Expr::Add(a, b) | Expr::Sub(a, b) => has_deref(a) || has_deref(b),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    struct Fake {
        mem: HashMap<u64, u64>,
    }

    impl Env for Fake {
        fn module(&self, name: &str) -> Option<(String, u64)> {
            match name.to_ascii_lowercase().as_str() {
                "exe" | "grb_vulkan.exe" | "grb_vulkan" => Some(("GRB_vulkan.exe".into(), 0x1_4000_0000)),
                "ntdll.dll" | "ntdll" => Some(("ntdll.dll".into(), 0x7FF8_0000_0000)),
                _ => None,
            }
        }
        fn var(&self, name: &str) -> Option<u64> {
            (name == "body").then_some(0x2000_0000)
        }
        fn read_ptr(&self, addr: u64) -> Result<u64, ReadError> {
            self.mem.get(&addr).copied().ok_or(ReadError::Free)
        }
    }

    fn fake() -> Fake {
        let mut mem = HashMap::new();
        mem.insert(0x1_4000_0000 + 0x63A0D20, 0x3000_0000);
        mem.insert(0x1_4000_0000 + 0x63EDAA0, 0x4000_0000);
        mem.insert(0x4000_0000 + 0x1F0, 0x5000_0000);
        Fake { mem }
    }

    fn run(text: &str) -> (Result<u64, EvalError>, Vec<Step>) {
        let e = parse(text).unwrap();
        let mut steps = Vec::new();
        (eval(&e, &fake(), &mut steps), steps)
    }

    #[test]
    fn numbers_are_hex() {
        assert_eq!(parse("1234").unwrap(), Expr::Num(0x1234));
        assert_eq!(parse("0x1234").unwrap(), Expr::Num(0x1234));
        assert_eq!(parse("7FF6A000").unwrap(), Expr::Num(0x7FF6_A000));
        assert!(parse("0xZZ").is_err());
        assert!(parse("12g").is_err());
    }

    #[test]
    fn modules_and_math() {
        assert_eq!(run("exe+0x10").0, Ok(0x1_4000_0010));
        assert_eq!(run("GRB_VULKAN.EXE+10").0, Ok(0x1_4000_0010));
        assert_eq!(run("ntdll+0x20-0x10").0, Ok(0x7FF8_0000_0010));
        assert_eq!(run("$body+20").0, Ok(0x2000_0020));
        assert_eq!(run(" exe + 0x10 ").0, Ok(0x1_4000_0010));
        assert!(matches!(run("nope.dll+1").0, Err(EvalError::UnknownModule(_))));
        assert!(matches!(run("$who").0, Err(EvalError::UnknownVar(_))));
        assert!(matches!(run("0-1").0, Err(EvalError::Overflow(_))));
    }

    #[test]
    fn pointer_chains() {
        let (r, steps) = run("[exe+0x63A0D20]+0x490");
        assert_eq!(r, Ok(0x3000_0490));
        assert_eq!(steps.len(), 1);
        assert_eq!(steps[0].text, "[exe+0x63A0D20]");
        let (r, steps) = run("[[exe+0x63EDAA0]+0x1F0]+0x18");
        assert_eq!(r, Ok(0x5000_0018));
        assert_eq!(steps.len(), 2);
        assert_eq!(steps[1].addr, 0x4000_01F0);
        let (r, steps) = run("[[exe+0x63EDAA0]+0x1F8]+0x18");
        assert_eq!(steps.len(), 2);
        match r {
            Err(EvalError::Read(s)) => {
                assert_eq!(s.addr, 0x4000_01F8);
                assert_eq!(s.result, Err(ReadError::Free));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn bad_brackets() {
        assert!(parse("[exe+1").is_err());
        assert!(parse("exe+1]").is_err());
        assert!(parse("[]").is_err());
        assert!(parse("exe++1").is_err());
        assert!(parse("").is_err());
        assert!(parse("exe*2").is_err());
    }

    #[test]
    fn prints_back() {
        let e = parse("[[exe+63EDAA0]+1F0]+18").unwrap();
        assert_eq!(e.to_string(), "[[exe+0x63EDAA0]+0x1F0]+0x18");
        assert!(has_deref(&e));
        assert!(!has_deref(&parse("exe+1").unwrap()));
    }
}
