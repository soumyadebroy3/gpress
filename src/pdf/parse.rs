//! Strict PDF syntax parsing. Anything that readers could interpret in different ways is an error here,
//! so the engine leaves such files untouched instead of choosing one reader's interpretation.

use super::object::*;

#[derive(Debug, Clone, PartialEq)]
pub struct ParseError {
    pub pos: usize,
    pub msg: &'static str,
}

pub type PResult<T> = Result<T, ParseError>;

pub struct Lexer<'a> {
    pub d: &'a [u8],
    pub pos: usize,
    /// A literal string held an unescaped carriage return. Readers disagree on its value (qpdf reads a line
    /// feed, as the standard says; pdf.js, PDFium and Poppler keep the carriage return), so callers that
    /// rewrite objects refuse it.
    pub raw_cr: bool,
}

fn hexval(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

/// Is `t` a PDF number? Returns Some(true) for integers, Some(false) for reals.
pub fn number_kind(t: &[u8]) -> Option<bool> {
    let mut i = 0;
    if i < t.len() && (t[i] == b'+' || t[i] == b'-') {
        i += 1;
    }
    let ds = i;
    while i < t.len() && t[i].is_ascii_digit() {
        i += 1;
    }
    let int_digits = i - ds;
    if i == t.len() {
        return if int_digits > 0 { Some(true) } else { None };
    }
    if t[i] != b'.' {
        return None;
    }
    i += 1;
    let fs = i;
    while i < t.len() && t[i].is_ascii_digit() {
        i += 1;
    }
    if i != t.len() || (int_digits == 0 && i == fs) {
        return None;
    }
    Some(false)
}

impl<'a> Lexer<'a> {
    pub fn new(d: &'a [u8], pos: usize) -> Self {
        Lexer { d, pos, raw_cr: false }
    }

    fn err<T>(&self, msg: &'static str) -> PResult<T> {
        Err(ParseError { pos: self.pos, msg })
    }

    pub fn skip_ws(&mut self) {
        while self.pos < self.d.len() {
            let c = self.d[self.pos];
            if is_white(c) {
                self.pos += 1;
            } else if c == b'%' {
                while self.pos < self.d.len() && self.d[self.pos] != b'\n' && self.d[self.pos] != b'\r' {
                    self.pos += 1;
                }
            } else {
                break;
            }
        }
    }

    pub fn peek(&self) -> Option<u8> {
        self.d.get(self.pos).copied()
    }

    /// A run of regular characters (keyword or number), without moving past whitespace first.
    pub fn regular_token(&mut self) -> &'a [u8] {
        let s = self.pos;
        while self.pos < self.d.len() && is_regular(self.d[self.pos]) {
            self.pos += 1;
        }
        &self.d[s..self.pos]
    }

    /// Skips whitespace and reads a keyword; returns it without consuming if it is not `kw`.
    pub fn expect_keyword(&mut self, kw: &[u8]) -> bool {
        self.skip_ws();
        let save = self.pos;
        let t = self.regular_token();
        if t == kw {
            true
        } else {
            self.pos = save;
            false
        }
    }

    pub fn parse_obj(&mut self) -> PResult<Obj> {
        self.parse_obj_depth(0)
    }

    fn parse_obj_depth(&mut self, depth: usize) -> PResult<Obj> {
        if depth > 256 {
            return self.err("nesting too deep");
        }
        self.skip_ws();
        let c = match self.peek() {
            Some(c) => c,
            None => return self.err("unexpected end"),
        };
        match c {
            b'/' => Ok(Obj::Name(self.parse_name()?)),
            b'(' => Ok(Obj::Str(self.parse_literal()?)),
            b'<' => {
                if self.d.get(self.pos + 1) == Some(&b'<') {
                    Ok(Obj::Dict(self.parse_dict(depth)?))
                } else {
                    Ok(Obj::Str(self.parse_hex()?))
                }
            }
            b'[' => {
                self.pos += 1;
                let mut a = Vec::new();
                loop {
                    self.skip_ws();
                    match self.peek() {
                        Some(b']') => {
                            self.pos += 1;
                            break;
                        }
                        Some(_) => a.push(self.parse_obj_depth(depth + 1)?),
                        None => return self.err("unterminated array"),
                    }
                }
                Ok(Obj::Array(a))
            }
            _ if is_regular(c) => {
                let start = self.pos;
                let t = self.regular_token();
                match t {
                    b"true" => return Ok(Obj::Bool(true)),
                    b"false" => return Ok(Obj::Bool(false)),
                    b"null" => return Ok(Obj::Null),
                    _ => {}
                }
                match number_kind(t) {
                    Some(true) => {
                        let unsigned = t[0].is_ascii_digit();
                        let val: Option<i64> = std::str::from_utf8(t).ok().and_then(|s| s.parse().ok());
                        if unsigned {
                            // Maybe "n g R".
                            let save = self.pos;
                            self.skip_ws();
                            let g0 = self.pos;
                            let g = self.regular_token();
                            if !g.is_empty() && g.iter().all(|c| c.is_ascii_digit()) {
                                self.skip_ws();
                                let r = self.regular_token();
                                if r == b"R" {
                                    let n: Option<u32> = std::str::from_utf8(t).ok().and_then(|s| s.parse().ok());
                                    let gn: Option<u16> = std::str::from_utf8(g).ok().and_then(|s| s.parse().ok());
                                    return match (n, gn) {
                                        (Some(n), Some(gn)) => Ok(Obj::Ref(n, gn)),
                                        _ => Err(ParseError { pos: g0, msg: "reference out of range" }),
                                    };
                                }
                            }
                            self.pos = save;
                        }
                        match val {
                            Some(v) => Ok(Obj::Int(v)),
                            // Too large for 64 bits: keep the digits as written.
                            None => Ok(Obj::Real(self.d[start..self.pos].to_vec())),
                        }
                    }
                    Some(false) => Ok(Obj::Real(t.to_vec())),
                    None => Err(ParseError { pos: start, msg: "unexpected keyword" }),
                }
            }
            _ => self.err("unexpected character"),
        }
    }

    fn parse_name(&mut self) -> PResult<Vec<u8>> {
        debug_assert_eq!(self.peek(), Some(b'/'));
        self.pos += 1;
        let mut out = Vec::new();
        while self.pos < self.d.len() && is_regular(self.d[self.pos]) {
            let c = self.d[self.pos];
            if c == b'#' {
                let h = self.d.get(self.pos + 1).and_then(|&x| hexval(x));
                let l = self.d.get(self.pos + 2).and_then(|&x| hexval(x));
                match (h, l) {
                    (Some(h), Some(l)) => {
                        out.push(h << 4 | l);
                        self.pos += 3;
                    }
                    _ => return self.err("bad # escape in name"),
                }
            } else {
                out.push(c);
                self.pos += 1;
            }
        }
        Ok(out)
    }

    fn parse_literal(&mut self) -> PResult<Vec<u8>> {
        self.pos += 1;
        let mut out = Vec::new();
        let mut depth = 1;
        loop {
            let c = match self.peek() {
                Some(c) => c,
                None => return self.err("unterminated string"),
            };
            self.pos += 1;
            match c {
                b'\\' => {
                    let e = match self.peek() {
                        Some(e) => e,
                        None => return self.err("unterminated string"),
                    };
                    self.pos += 1;
                    match e {
                        b'n' => out.push(b'\n'),
                        b'r' => out.push(b'\r'),
                        b't' => out.push(b'\t'),
                        b'b' => out.push(8),
                        b'f' => out.push(12),
                        b'(' | b')' | b'\\' => out.push(e),
                        b'0'..=b'7' => {
                            let mut v = (e - b'0') as u32;
                            for _ in 0..2 {
                                match self.peek() {
                                    Some(d @ b'0'..=b'7') => {
                                        v = v * 8 + (d - b'0') as u32;
                                        self.pos += 1;
                                    }
                                    _ => break,
                                }
                            }
                            out.push((v & 0xff) as u8);
                        }
                        b'\r' => {
                            if self.peek() == Some(b'\n') {
                                self.pos += 1;
                            }
                        }
                        b'\n' => {}
                        // The spec says an unknown escape is just the character itself.
                        _ => out.push(e),
                    }
                }
                b'(' => {
                    depth += 1;
                    out.push(c);
                }
                b')' => {
                    depth -= 1;
                    if depth == 0 {
                        return Ok(out);
                    }
                    out.push(c);
                }
                b'\r' => {
                    self.raw_cr = true;
                    if self.peek() == Some(b'\n') {
                        self.pos += 1;
                    }
                    out.push(b'\n');
                }
                _ => out.push(c),
            }
        }
    }

    fn parse_hex(&mut self) -> PResult<Vec<u8>> {
        self.pos += 1;
        let mut out = Vec::new();
        let mut hi: Option<u8> = None;
        loop {
            let c = match self.peek() {
                Some(c) => c,
                None => return self.err("unterminated hex string"),
            };
            self.pos += 1;
            if c == b'>' {
                if let Some(h) = hi {
                    out.push(h << 4);
                }
                return Ok(out);
            }
            if is_white(c) {
                continue;
            }
            let v = match hexval(c) {
                Some(v) => v,
                None => return Err(ParseError { pos: self.pos - 1, msg: "bad hex digit" }),
            };
            match hi {
                None => hi = Some(v),
                Some(h) => {
                    out.push(h << 4 | v);
                    hi = None;
                }
            }
        }
    }

    fn parse_dict(&mut self, depth: usize) -> PResult<Dict> {
        self.pos += 2;
        let mut d = Dict::default();
        loop {
            self.skip_ws();
            match self.peek() {
                Some(b'>') => {
                    if self.d.get(self.pos + 1) == Some(&b'>') {
                        self.pos += 2;
                        return Ok(d);
                    }
                    return self.err("bad dictionary end");
                }
                Some(b'/') => {
                    let k = self.parse_name()?;
                    let v = self.parse_obj_depth(depth + 1)?;
                    if d.has(&k) {
                        return self.err("duplicate key");
                    }
                    d.0.push((k, v));
                }
                Some(_) => return self.err("dictionary key is not a name"),
                None => return self.err("unterminated dictionary"),
            }
        }
    }
}

/// Why an object with an unescaped carriage return in a literal string is refused.
pub const RAW_CR: &str = "a literal string holds a raw carriage return (readers disagree on its value)";

/// Parses "n g obj ... endobj" at `pos`. For streams, `length` resolves the /Length value (which may be an
/// indirect reference). Returns (number, generation, object, end position). An object whose literal strings
/// hold an unescaped carriage return is an error (see `Lexer::raw_cr`).
pub fn parse_indirect(d: &[u8], pos: usize, length: &dyn Fn(&Obj) -> Option<usize>) -> PResult<(u32, u16, Obj, usize)> {
    let mut lx = Lexer::new(d, pos);
    lx.skip_ws();
    let n = lx.regular_token();
    let n: u32 =
        std::str::from_utf8(n).ok().and_then(|s| s.parse().ok()).ok_or(ParseError { pos, msg: "bad object number" })?;
    lx.skip_ws();
    let g = lx.regular_token();
    let g: u16 =
        std::str::from_utf8(g).ok().and_then(|s| s.parse().ok()).ok_or(ParseError { pos, msg: "bad generation" })?;
    if !lx.expect_keyword(b"obj") {
        return Err(ParseError { pos: lx.pos, msg: "missing obj" });
    }
    let o = lx.parse_obj()?;
    if lx.raw_cr {
        return Err(ParseError { pos, msg: RAW_CR });
    }
    let after = lx.pos;
    if lx.expect_keyword(b"stream") {
        let dict = match o {
            Obj::Dict(dd) => dd,
            _ => return Err(ParseError { pos: after, msg: "stream without dictionary" }),
        };
        // The keyword is followed by CR LF or LF.
        let mut p = lx.pos;
        if d.get(p) == Some(&b'\r') && d.get(p + 1) == Some(&b'\n') {
            p += 2;
        } else if d.get(p) == Some(&b'\n') {
            p += 1;
        } else {
            return Err(ParseError { pos: p, msg: "stream keyword not followed by an end of line" });
        }
        let len_obj = dict.get(b"Length").ok_or(ParseError { pos: p, msg: "stream without Length" })?;
        let len = length(len_obj).ok_or(ParseError { pos: p, msg: "unusable stream Length" })?;
        let end = p
            .checked_add(len)
            .filter(|&e| e <= d.len())
            .ok_or(ParseError { pos: p, msg: "stream runs past the end" })?;
        let data = d[p..end].to_vec();
        let mut lx2 = Lexer::new(d, end);
        if !lx2.expect_keyword(b"endstream") {
            return Err(ParseError { pos: end, msg: "Length does not end at endstream" });
        }
        if !lx2.expect_keyword(b"endobj") {
            return Err(ParseError { pos: lx2.pos, msg: "missing endobj" });
        }
        return Ok((n, g, Obj::Stream(Box::new(Stream { dict, data })), lx2.pos));
    }
    if !lx.expect_keyword(b"endobj") {
        return Err(ParseError { pos: lx.pos, msg: "missing endobj" });
    }
    Ok((n, g, o, lx.pos))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> Obj {
        Lexer::new(s.as_bytes(), 0).parse_obj().unwrap()
    }

    #[test]
    fn parses_and_writes_back_the_same_values() {
        let cases = [
            "<</Type/Page/Parent 3 0 R/MediaBox[0 0 612 792]/Rotate -90/X .5/Y -1.25/N null/B true>>",
            "[(a\\(b\\)c) <414243> (line\\nbreak) /A#20B 12 0 R 7]",
            "(nested (parens) ok)",
            "<< /K [ 1 2 3 ] /S /Name >>",
        ];
        for c in cases {
            let o = p(c);
            let back = to_bytes(&o);
            assert_eq!(Lexer::new(&back, 0).parse_obj().unwrap(), o, "{c} -> {}", String::from_utf8_lossy(&back));
        }
        assert_eq!(p("(a\r\nb)"), Obj::Str(b"a\nb".to_vec()));
        assert_eq!(p("(\\101\\60)"), Obj::Str(b"A0".to_vec()));
        assert_eq!(p("<41 4>"), Obj::Str(vec![0x41, 0x40]));
        assert_eq!(p("(\\q)"), Obj::Str(b"q".to_vec()));
        assert_eq!(p("12 0 R"), Obj::Ref(12, 0));
        assert_eq!(p("/"), Obj::Name(vec![]));
        assert_eq!(to_bytes(&Obj::Array(vec![Obj::Name(vec![]), Obj::Int(5)])), b"[/ 5]".to_vec());
    }

    #[test]
    fn refuses_ambiguous_syntax() {
        for bad in ["<</A 1/A 2>>", "/A#4", "(abc", "<4G>", "[1 2", "<</A>>"] {
            assert!(Lexer::new(bad.as_bytes(), 0).parse_obj().is_err(), "{bad}");
        }
    }

    #[test]
    fn raw_carriage_returns_are_flagged() {
        // An unescaped CR (alone or before LF) is flagged; escaped line ends and a raw LF are not.
        for (s, flagged) in [
            ("(a\rb)", true),
            ("(a\r\nb)", true),
            ("<< /T [(x) (a\\\r\rb)] >>", true),
            ("(a\nb)", false),
            ("(a\\\rb)", false),
            ("(a\\\r\nb)", false),
            ("(a\\rb)", false),
            ("<0D0A>", false),
        ] {
            let mut lx = Lexer::new(s.as_bytes(), 0);
            lx.parse_obj().unwrap();
            assert_eq!(lx.raw_cr, flagged, "{s:?}");
        }
        let obj = |body: &str| format!("3 0 obj\n{body}\nendobj\n");
        let len = |_: &Obj| None;
        assert_eq!(parse_indirect(obj("<< /T (a\rb) >>").as_bytes(), 0, &len).unwrap_err().msg, RAW_CR);
        assert!(parse_indirect(obj("<< /T (a\\\r\nb) >>").as_bytes(), 0, &len).is_ok());
    }

    #[test]
    fn numbers() {
        assert_eq!(number_kind(b"12"), Some(true));
        assert_eq!(number_kind(b"-12"), Some(true));
        assert_eq!(number_kind(b"1.5"), Some(false));
        assert_eq!(number_kind(b".5"), Some(false));
        assert_eq!(number_kind(b"5."), Some(false));
        assert_eq!(number_kind(b"-.5"), Some(false));
        assert_eq!(number_kind(b"."), None);
        assert_eq!(number_kind(b"1e5"), None);
        assert_eq!(number_kind(b"--5"), None);
    }
}
