//! MSVC RTTI type names to readable C++ names: ".?AVFoo@Bar@@" -> "Bar::Foo".
//!
//! Handles nested names, templates (each with its own name back-reference table, as MSVC mangles
//! them), name back-references (digits), anonymous namespaces, built-in types, pointers and
//! references, function and member-function pointers, class/struct/union/enum arguments and integer
//! constants. Anything else (arrays, local classes) falls back to a plain split on '@', so a name
//! is never lost; callers keep the raw name beside it anyway.

pub fn demangle(raw: &str) -> String {
    let Some(body) = raw.strip_prefix(".?A").and_then(|r| r.get(1..)) else {
        return raw.to_string();
    };
    let mut p = Parser { s: body.as_bytes(), i: 0, names: Vec::new(), params: Vec::new() };
    match p.qualified() {
        Some(n) if p.i == p.s.len() => n,
        _ => fallback(body),
    }
}

/// "Foo@Bar@@" -> "Bar::Foo", no template handling.
fn fallback(body: &str) -> String {
    let core = body.strip_suffix("@@").unwrap_or(body);
    let mut parts: Vec<&str> = core.split('@').filter(|s| !s.is_empty()).collect();
    parts.reverse();
    parts.join("::")
}

struct Parser<'a> {
    s: &'a [u8],
    i: usize,
    /// Name back-reference table (at most 10, no duplicates).
    names: Vec<String>,
    /// Function parameter back-reference table (types longer than one letter).
    params: Vec<String>,
}

impl Parser<'_> {
    fn peek(&self) -> Option<u8> {
        self.s.get(self.i).copied()
    }

    fn eat(&mut self, text: &str) -> bool {
        if self.s[self.i..].starts_with(text.as_bytes()) {
            self.i += text.len();
            true
        } else {
            false
        }
    }

    fn next(&mut self) -> Option<u8> {
        let c = self.peek()?;
        self.i += 1;
        Some(c)
    }

    fn remember(&mut self, n: &str) {
        if self.names.len() < 10 && !self.names.iter().any(|x| x == n) {
            self.names.push(n.to_string());
        }
    }

    /// Name pieces, innermost first, ended by '@'. Returned outermost first, joined by "::".
    fn qualified(&mut self) -> Option<String> {
        let mut parts = Vec::new();
        loop {
            if self.peek()? == b'@' {
                self.i += 1;
                break;
            }
            parts.push(self.piece()?);
        }
        if parts.is_empty() {
            return None;
        }
        parts.reverse();
        Some(parts.join("::"))
    }

    fn piece(&mut self) -> Option<String> {
        match self.peek()? {
            c @ b'0'..=b'9' => {
                self.i += 1;
                self.names.get(usize::from(c - b'0')).cloned()
            }
            b'?' => {
                if self.eat("?$") {
                    self.template()
                } else if self.eat("?A") {
                    // Anonymous namespace: "?A0x1234abcd@".
                    self.ident()?;
                    let n = "`anonymous namespace'".to_string();
                    self.remember(&n);
                    Some(n)
                } else {
                    None
                }
            }
            _ => {
                let n = self.ident()?;
                self.remember(&n);
                Some(n)
            }
        }
    }

    /// Bytes up to '@' (consumed).
    fn ident(&mut self) -> Option<String> {
        let start = self.i;
        let len = self.s[start..].iter().position(|&c| c == b'@')?;
        if len == 0 {
            return None;
        }
        self.i = start + len + 1;
        Some(String::from_utf8_lossy(&self.s[start..start + len]).into_owned())
    }

    /// After "?$": name '@' args '@'. The name and args use a fresh back-reference table; the whole
    /// "name<args>" is then remembered in the outer one.
    fn template(&mut self) -> Option<String> {
        let outer = (std::mem::take(&mut self.names), std::mem::take(&mut self.params));
        let name = self.ident().inspect(|n| self.remember(n));
        let args = name.as_ref().and_then(|_| self.args());
        (self.names, self.params) = outer;
        let full = format!("{}<{}>", name?, args?.join(","));
        self.remember(&full);
        Some(full)
    }

    fn args(&mut self) -> Option<Vec<String>> {
        let mut out = Vec::new();
        loop {
            if self.peek()? == b'@' {
                self.i += 1;
                return Some(out);
            }
            // Parameter pack markers carry no argument.
            if self.eat("$$$V") || self.eat("$$V") || self.eat("$$Z") || self.eat("$S") {
                continue;
            }
            if self.eat("$0") {
                out.push(self.number()?.to_string());
            } else if self.eat("$$C") {
                out.push(self.cv_type()?);
            } else {
                out.push(self.ty()?);
            }
        }
    }

    /// MSVC encoded number: '?' for minus, a digit d = d+1, or hex letters A-P ended by '@'.
    fn number(&mut self) -> Option<i64> {
        let neg = self.eat("?");
        let v = match self.peek()? {
            c @ b'0'..=b'9' => {
                self.i += 1;
                i64::from(c - b'0') + 1
            }
            _ => {
                let mut v: i64 = 0;
                loop {
                    match self.next()? {
                        b'@' => break,
                        c @ b'A'..=b'P' => v = v.checked_mul(16)?.checked_add(i64::from(c - b'A'))?,
                        _ => return None,
                    }
                }
                v
            }
        };
        Some(if neg { -v } else { v })
    }

    /// A cv letter, then a type: "const T", "volatile T" or "T".
    fn cv_type(&mut self) -> Option<String> {
        let cv = self.next()?;
        let t = self.ty()?;
        Some(match cv {
            b'A' => t,
            b'B' => format!("const {t}"),
            b'C' => format!("volatile {t}"),
            b'D' => format!("const volatile {t}"),
            _ => return None,
        })
    }

    /// After "P6" / "P8<class>": [this qualifiers], calling convention, return type, parameters,
    /// throw spec. Returns (return type, parameters).
    fn func_type(&mut self, this_quals: bool) -> Option<(String, String)> {
        if this_quals {
            while matches!(self.peek()?, b'E' | b'F' | b'I') {
                self.i += 1;
            }
            if matches!(self.peek()?, b'G' | b'H') {
                self.i += 1; // & / && ref-qualifier
            }
            self.next()?; // Cv of `this`
        }
        self.next()?; // Calling convention
        let ret = if self.eat("?") { self.cv_type()? } else { self.ty()? };
        let params = self.params()?;
        if !(self.eat("Z") || self.eat("_E")) {
            return None;
        }
        Some((ret, params))
    }

    /// Function parameters: "X" (none), or types ended by '@' ('Z' = variadic). Digits refer to
    /// earlier parameters longer than one letter.
    fn params(&mut self) -> Option<String> {
        if self.eat("X") {
            return Some(String::new());
        }
        let mut v = Vec::new();
        loop {
            match self.peek()? {
                b'@' => {
                    self.i += 1;
                    break;
                }
                b'Z' => {
                    self.i += 1;
                    v.push("...".to_string());
                    break;
                }
                c @ b'0'..=b'9' => {
                    self.i += 1;
                    v.push(self.params.get(usize::from(c - b'0'))?.clone());
                }
                _ => {
                    let start = self.i;
                    let t = self.ty()?;
                    if self.i - start > 1 && self.params.len() < 10 {
                        self.params.push(t.clone());
                    }
                    v.push(t);
                }
            }
        }
        Some(v.join(","))
    }

    fn ty(&mut self) -> Option<String> {
        let c = self.next()?;
        let prim = match c {
            b'X' => "void",
            b'C' => "signed char",
            b'D' => "char",
            b'E' => "unsigned char",
            b'F' => "short",
            b'G' => "unsigned short",
            b'H' => "int",
            b'I' => "unsigned int",
            b'J' => "long",
            b'K' => "unsigned long",
            b'M' => "float",
            b'N' => "double",
            b'O' => "long double",
            b'_' => match self.next()? {
                b'N' => "bool",
                b'J' => "__int64",
                b'K' => "unsigned __int64",
                b'W' => "wchar_t",
                b'Q' => "char8_t",
                b'S' => "char16_t",
                b'U' => "char32_t",
                _ => return None,
            },
            b'T' | b'U' | b'V' => return self.qualified(),
            b'W' => {
                // Enum: a digit for the underlying type, then the name.
                if !self.next()?.is_ascii_digit() {
                    return None;
                }
                return self.qualified();
            }
            b'P' | b'Q' | b'R' | b'S' if self.eat("6") => {
                let (ret, params) = self.func_type(false)?;
                return Some(format!("{ret} (*)({params})"));
            }
            b'P' | b'Q' | b'R' | b'S' if self.eat("8") => {
                let class = self.qualified()?;
                let (ret, params) = self.func_type(true)?;
                return Some(format!("{ret} ({class}::*)({params})"));
            }
            b'P' | b'Q' | b'R' | b'S' | b'A' | b'B' => {
                // __ptr64 / __restrict / __unaligned markers.
                while matches!(self.peek()?, b'E' | b'F' | b'I') {
                    self.i += 1;
                }
                let inner = self.cv_type()?;
                let mark = if matches!(c, b'A' | b'B') { "&" } else { "*" };
                return Some(format!("{inner}{mark}"));
            }
            b'$' => {
                if self.eat("$Q") || self.eat("$R") {
                    while matches!(self.peek()?, b'E' | b'F' | b'I') {
                        self.i += 1;
                    }
                    return Some(format!("{}&&", self.cv_type()?));
                }
                if self.eat("$T") {
                    return Some("std::nullptr_t".into());
                }
                return None;
            }
            _ => return None,
        };
        Some(prim.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::demangle;

    #[test]
    fn plain_and_nested() {
        assert_eq!(demangle(".?AVFoo@Bar@@"), "Bar::Foo");
        assert_eq!(demangle(".?AUBaz@@"), "Baz");
        assert_eq!(demangle(".?AVx@y@z@@"), "z::y::x");
        assert_eq!(demangle(".?AV<lambda_1>@@"), "<lambda_1>");
    }

    #[test]
    fn templates_and_backrefs() {
        assert_eq!(demangle(".?AV?$vector@HV?$allocator@H@std@@@std@@"), "std::vector<int,std::allocator<int>>");
        // "2" refers to "std" in basic_string's own table (basic_string, char_traits<char>, std).
        assert_eq!(
            demangle(".?AV?$basic_string@DU?$char_traits@D@std@@V?$allocator@D@2@@std@@"),
            "std::basic_string<char,std::char_traits<char>,std::allocator<char>>"
        );
        assert_eq!(demangle(".?AV?$Foo@$0BA@@ns@@"), "ns::Foo<16>");
        assert_eq!(demangle(".?AV?$Foo@$00@@"), "Foo<1>");
        assert_eq!(demangle(".?AV?$Ptr@PEBD_N@@"), "Ptr<const char*,bool>");
        assert_eq!(demangle(".?AV?$H@VA@?A0x12ab34cd@@@?A0x12ab34cd@@"), "`anonymous namespace'::H<`anonymous namespace'::A>");
        // Back-reference in the outer table: "0" is "Inner" (first name seen).
        assert_eq!(demangle(".?AVInner@Outer@@"), "Outer::Inner");
    }

    #[test]
    fn function_pointers() {
        assert_eq!(demangle(".?AV?$F@P6AXXZ@@"), "F<void (*)()>");
        // Member function pointer; "1" is "W" in Slot's own table (Slot, W).
        assert_eq!(demangle(".?AU?$Slot@VW@@P81@EAAXAEBUIn@@H@Z@@"), "Slot<W,void (W::*)(const In&,int)>");
        // A repeated parameter type is a digit.
        assert_eq!(demangle(".?AV?$F@P6AXPEAVA@@0@Z@@"), "F<void (*)(A*,A*)>");
    }

    #[test]
    fn falls_back_without_losing_the_name() {
        // An array argument is not handled: plain split instead of nothing.
        let raw = ".?AV?$F@$$BY0BA@H@@";
        assert!(!demangle(raw).is_empty());
        assert_eq!(demangle("garbage"), "garbage");
    }
}
