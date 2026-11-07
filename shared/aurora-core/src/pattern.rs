//! IDA-style byte patterns: `48 8B 05 ?? ?? ?? ?? 48 85 C0`. `?` or `??` is a wildcard. Used for
//! signature scans so nothing depends on fixed addresses.

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pattern {
    /// `None` is a wildcard.
    bytes: Vec<Option<u8>>,
}

impl Pattern {
    pub fn parse(text: &str) -> Result<Self, String> {
        let mut bytes = Vec::new();
        for tok in text.split_whitespace() {
            match tok {
                "?" | "??" => bytes.push(None),
                _ => {
                    if tok.len() != 2 {
                        return Err(format!("bad byte '{tok}': want two hex digits or ??"));
                    }
                    let b = u8::from_str_radix(tok, 16).map_err(|_| format!("bad byte '{tok}'"))?;
                    bytes.push(Some(b));
                }
            }
        }
        if bytes.is_empty() {
            return Err("empty pattern".into());
        }
        if bytes.iter().all(Option::is_none) {
            return Err("pattern is all wildcards".into());
        }
        Ok(Pattern { bytes })
    }

    pub fn len(&self) -> usize {
        self.bytes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    /// Does the pattern match at the start of `hay`?
    pub fn matches_at(&self, hay: &[u8]) -> bool {
        hay.len() >= self.bytes.len()
            && self.bytes.iter().zip(hay).all(|(p, b)| p.is_none_or(|p| p == *b))
    }

    /// Every offset in `hay` where the pattern matches, in order. `limit` stops the search after
    /// that many hits (0 = no limit).
    pub fn find_all(&self, hay: &[u8], limit: usize) -> Vec<usize> {
        let mut hits = Vec::new();
        let n = self.bytes.len();
        if hay.len() < n {
            return hits;
        }
        // Skip ahead on the first fixed byte: cheap and good enough for a few hundred MB.
        let first_fixed = self.bytes.iter().position(Option::is_some).unwrap_or(0);
        let first_byte = self.bytes[first_fixed].unwrap_or(0);
        let mut i = 0;
        let last_start = hay.len() - n;
        while i <= last_start {
            let probe = i + first_fixed;
            if hay[probe] != first_byte {
                match hay[probe + 1..=last_start + first_fixed].iter().position(|&b| b == first_byte) {
                    Some(skip) => {
                        i += skip + 1;
                        continue;
                    }
                    None => break,
                }
            }
            if self.matches_at(&hay[i..]) {
                hits.push(i);
                if limit != 0 && hits.len() >= limit {
                    break;
                }
            }
            i += 1;
        }
        hits
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_rejects() {
        let p = Pattern::parse("48 8b ?? 05 ?").unwrap();
        assert_eq!(p.len(), 5);
        assert!(Pattern::parse("").is_err());
        assert!(Pattern::parse("?? ??").is_err());
        assert!(Pattern::parse("4 8").is_err());
        assert!(Pattern::parse("ZZ").is_err());
    }

    #[test]
    fn finds_with_wildcards() {
        let hay = [0x00, 0x48, 0x8B, 0x11, 0x05, 0x48, 0x8B, 0x22, 0x05, 0x48];
        let p = Pattern::parse("48 8B ?? 05").unwrap();
        assert_eq!(p.find_all(&hay, 0), vec![1, 5]);
        assert_eq!(p.find_all(&hay, 1), vec![1]);
        assert!(p.find_all(&hay[..3], 0).is_empty());
    }

    #[test]
    fn leading_wildcard_and_edges() {
        let hay = [0xAA, 0x10, 0x20, 0xBB, 0x10, 0x20];
        let p = Pattern::parse("?? 10 20").unwrap();
        assert_eq!(p.find_all(&hay, 0), vec![0, 3]);
        let p = Pattern::parse("10 20").unwrap();
        assert_eq!(p.find_all(&hay, 0), vec![1, 4]);
        let p = Pattern::parse("20 BB 10 20").unwrap();
        assert_eq!(p.find_all(&hay, 0), vec![2]);
        let p = Pattern::parse("FF").unwrap();
        assert!(p.find_all(&hay, 0).is_empty());
    }
}
