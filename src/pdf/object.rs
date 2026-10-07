//! PDF objects and their compact serialisation.

/// A PDF object. Numbers keep their written form so they are written back exactly as they were.
#[derive(Clone, Debug, PartialEq)]
pub enum Obj {
    Null,
    Bool(bool),
    Int(i64),
    /// A real number exactly as written (validated syntax).
    Real(Vec<u8>),
    /// Name bytes without the leading '/', with #xx escapes decoded.
    Name(Vec<u8>),
    /// String bytes with escapes decoded (literal or hexadecimal in the source).
    Str(Vec<u8>),
    Array(Vec<Obj>),
    Dict(Dict),
    Ref(u32, u16),
    Stream(Box<Stream>),
}

/// A dictionary, keys in their original order.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Dict(pub Vec<(Vec<u8>, Obj)>);

#[derive(Clone, Debug, PartialEq)]
pub struct Stream {
    pub dict: Dict,
    /// The stream's bytes as stored (still encoded by its filters).
    pub data: Vec<u8>,
}

impl Dict {
    pub fn get(&self, key: &[u8]) -> Option<&Obj> {
        self.0.iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }
    pub fn get_mut(&mut self, key: &[u8]) -> Option<&mut Obj> {
        self.0.iter_mut().find(|(k, _)| k == key).map(|(_, v)| v)
    }
    pub fn set(&mut self, key: &[u8], v: Obj) {
        if let Some(slot) = self.get_mut(key) {
            *slot = v;
        } else {
            self.0.push((key.to_vec(), v));
        }
    }
    pub fn remove(&mut self, key: &[u8]) -> Option<Obj> {
        let i = self.0.iter().position(|(k, _)| k == key)?;
        Some(self.0.remove(i).1)
    }
    pub fn has(&self, key: &[u8]) -> bool {
        self.get(key).is_some()
    }
    pub fn name(&self, key: &[u8]) -> Option<&[u8]> {
        match self.get(key) {
            Some(Obj::Name(n)) => Some(n),
            _ => None,
        }
    }
}

impl Obj {
    pub fn as_dict(&self) -> Option<&Dict> {
        match self {
            Obj::Dict(d) => Some(d),
            Obj::Stream(s) => Some(&s.dict),
            _ => None,
        }
    }
    pub fn as_int(&self) -> Option<i64> {
        match self {
            Obj::Int(i) => Some(*i),
            _ => None,
        }
    }
    pub fn as_name(&self) -> Option<&[u8]> {
        match self {
            Obj::Name(n) => Some(n),
            _ => None,
        }
    }

    /// Every reference inside this object (not following them).
    pub fn refs(&self, out: &mut Vec<(u32, u16)>) {
        match self {
            Obj::Ref(n, g) => out.push((*n, *g)),
            Obj::Array(a) => a.iter().for_each(|o| o.refs(out)),
            Obj::Dict(d) => d.0.iter().for_each(|(_, v)| v.refs(out)),
            Obj::Stream(s) => s.dict.0.iter().for_each(|(_, v)| v.refs(out)),
            _ => {}
        }
    }
}

// ---------------------------------------------------------------- writing

#[inline]
pub fn is_white(c: u8) -> bool {
    matches!(c, 0 | 9 | 10 | 12 | 13 | 32)
}

#[inline]
pub fn is_delim(c: u8) -> bool {
    matches!(c, b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%')
}

#[inline]
pub fn is_regular(c: u8) -> bool {
    !is_white(c) && !is_delim(c)
}

/// Appends tokens with the least whitespace that keeps them apart.
pub struct Writer<'a> {
    pub out: &'a mut Vec<u8>,
}

impl<'a> Writer<'a> {
    fn sep_for(&mut self, first: u8) {
        if let Some(&last) = self.out.last() {
            // A bare '/' can only end an empty name, which would swallow a following regular token.
            if (is_regular(last) || last == b'/') && is_regular(first) {
                self.out.push(b' ');
            }
        }
    }

    fn token(&mut self, t: &[u8]) {
        if let Some(&f) = t.first() {
            self.sep_for(f);
        }
        self.out.extend_from_slice(t);
    }

    pub fn obj(&mut self, o: &Obj) {
        match o {
            Obj::Null => self.token(b"null"),
            Obj::Bool(true) => self.token(b"true"),
            Obj::Bool(false) => self.token(b"false"),
            Obj::Int(i) => self.token(i.to_string().as_bytes()),
            Obj::Real(r) => self.token(r),
            Obj::Name(n) => self.name(n),
            Obj::Str(s) => self.string(s),
            Obj::Array(a) => {
                self.out.push(b'[');
                for x in a {
                    self.obj(x);
                }
                self.out.push(b']');
            }
            Obj::Dict(d) => self.dict(d),
            Obj::Ref(n, g) => self.token(format!("{n} {g} R").as_bytes()),
            Obj::Stream(_) => panic!("streams are written as indirect objects"),
        }
    }

    pub fn dict(&mut self, d: &Dict) {
        self.out.extend_from_slice(b"<<");
        for (k, v) in &d.0 {
            self.name(k);
            self.obj(v);
        }
        self.out.extend_from_slice(b">>");
    }

    pub fn name(&mut self, n: &[u8]) {
        self.out.push(b'/');
        for &c in n {
            if c == b'#' || !(0x21..=0x7e).contains(&c) || is_delim(c) {
                self.out.extend_from_slice(format!("#{c:02X}").as_bytes());
            } else {
                self.out.push(c);
            }
        }
    }

    /// A string as a literal or hexadecimal string, whichever is shorter. Both read back as the same bytes.
    pub fn string(&mut self, s: &[u8]) {
        let mut lit = Vec::with_capacity(s.len() + 2);
        lit.push(b'(');
        for &c in s {
            match c {
                b'(' | b')' | b'\\' => {
                    lit.push(b'\\');
                    lit.push(c);
                }
                // A raw CR would be read back as LF: escape it.
                b'\r' => lit.extend_from_slice(b"\\r"),
                _ => lit.push(c),
            }
        }
        lit.push(b')');
        if lit.len() <= 2 * s.len() + 2 {
            self.out.extend_from_slice(&lit);
        } else {
            self.out.push(b'<');
            for &c in s {
                self.out.extend_from_slice(format!("{c:02X}").as_bytes());
            }
            self.out.push(b'>');
        }
    }
}

/// Serialises a non-stream object compactly.
pub fn to_bytes(o: &Obj) -> Vec<u8> {
    let mut v = Vec::new();
    Writer { out: &mut v }.obj(o);
    v
}
