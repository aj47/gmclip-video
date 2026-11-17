//! The regular-expression subset the RenderPlan schema uses, without a regex crate.
//!
//! Grammar: `^ atom* $`, where an atom is a literal character or a `[...]` class of
//! literals and `a-z` ranges, optionally followed by `{n}` or `{m,n}`. Every `pattern` in
//! `schemas/renderplan@0.json` is in this subset (checked by a test); a schema edit that
//! needs more fails [`Pattern::parse`] loudly instead of matching wrongly.

/// A parsed pattern.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Pattern {
    atoms: Vec<Atom>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Atom {
    /// Inclusive character ranges; a literal is `(c, c)`.
    set: Vec<(char, char)>,
    min: usize,
    max: usize,
}

impl Atom {
    fn accepts(&self, c: char) -> bool {
        self.set.iter().any(|&(lo, hi)| lo <= c && c <= hi)
    }
}

impl Pattern {
    /// Parses `src`, or says which part of the subset it leaves.
    pub fn parse(src: &str) -> Result<Self, String> {
        let body = src
            .strip_prefix('^')
            .and_then(|s| s.strip_suffix('$'))
            .ok_or("pattern must be anchored with ^ and $")?;
        let chars: Vec<char> = body.chars().collect();
        let mut atoms = Vec::new();
        let mut i = 0;
        while i < chars.len() {
            let set = match chars[i] {
                '[' => {
                    let close = chars[i..]
                        .iter()
                        .position(|&c| c == ']')
                        .map(|p| p + i)
                        .ok_or("unclosed [")?;
                    let class = &chars[i + 1..close];
                    if class.first() == Some(&'^') {
                        return Err("negated classes are not supported".into());
                    }
                    let mut set = Vec::new();
                    let mut j = 0;
                    while j < class.len() {
                        if class[j] == '\\' {
                            return Err("escapes are not supported".into());
                        }
                        if j + 2 < class.len() && class[j + 1] == '-' {
                            if class[j] > class[j + 2] {
                                return Err("reversed range".into());
                            }
                            set.push((class[j], class[j + 2]));
                            j += 3;
                        } else {
                            set.push((class[j], class[j]));
                            j += 1;
                        }
                    }
                    if set.is_empty() {
                        return Err("empty class".into());
                    }
                    i = close + 1;
                    set
                }
                c @ ('\\' | '.' | '*' | '+' | '?' | '(' | ')' | '|' | '{' | '}' | ']' | '^'
                | '$') => return Err(format!("{c:?} is outside the supported subset")),
                c => {
                    i += 1;
                    vec![(c, c)]
                }
            };
            let (min, max) = if chars.get(i) == Some(&'{') {
                let close = chars[i..]
                    .iter()
                    .position(|&c| c == '}')
                    .map(|p| p + i)
                    .ok_or("unclosed {")?;
                let q: String = chars[i + 1..close].iter().collect();
                i = close + 1;
                let num = |s: &str| s.parse::<usize>().map_err(|_| format!("bad count {s:?}"));
                match q.split_once(',') {
                    Some((m, n)) => (num(m)?, num(n)?),
                    None => (num(&q)?, num(&q)?),
                }
            } else {
                (1, 1)
            };
            if min > max {
                return Err("quantifier min > max".into());
            }
            atoms.push(Atom { set, min, max });
        }
        Ok(Self { atoms })
    }

    /// Whether the whole of `s` matches.
    pub fn is_match(&self, s: &str) -> bool {
        let chars: Vec<char> = s.chars().collect();
        Self::match_from(&self.atoms, &chars)
    }

    fn match_from(atoms: &[Atom], s: &[char]) -> bool {
        let Some((a, rest)) = atoms.split_first() else {
            return s.is_empty();
        };
        // Longest run this atom can take, then back off to its minimum.
        let run = s.iter().take(a.max).take_while(|&&c| a.accepts(c)).count();
        if run < a.min {
            return false;
        }
        (a.min..=run).rev().any(|n| Self::match_from(rest, &s[n..]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(p: &str, s: &str) -> bool {
        Pattern::parse(p).unwrap().is_match(s)
    }

    #[test]
    fn schema_patterns() {
        let id = "^[A-Za-z0-9_.:-]{1,96}$";
        assert!(m(id, "01-payoff-banana"));
        assert!(m(id, "a.b:c_d-e"));
        assert!(!m(id, ""));
        assert!(!m(id, "has space"));
        assert!(m(id, &"x".repeat(96)));
        assert!(!m(id, &"x".repeat(97)));
        assert!(!m(id, "é"));
        let sha = "^[0-9a-f]{64}$";
        assert!(m(sha, &"a".repeat(64)));
        assert!(!m(sha, &"A".repeat(64)));
        assert!(!m(sha, &"a".repeat(63)));
        assert!(m("^#[0-9a-fA-F]{6}$", "#0c1118"));
        assert!(!m("^#[0-9a-fA-F]{6}$", "0c1118"));
        assert!(!m("^#[0-9a-fA-F]{6}$", "#0c11180"));
        assert!(m("^#[0-9a-fA-F]{8}$", "#FFD400FF"));
        assert!(m("^[a-z_]{1,32}$", "captions_burned"));
        assert!(!m("^[a-z_]{1,32}$", "Cam"));
    }

    #[test]
    fn backtracks_between_atoms() {
        assert!(m("^[a-z]{0,3}b$", "aab"));
        assert!(m("^[ab]{1,3}b$", "abb"));
    }

    #[test]
    fn rejects_unsupported_syntax() {
        for p in [
            "[a-z]+", "^a+$", "^(a)$", "^a|b$", "^\\d$", "^[^a]$", "^a.$",
        ] {
            assert!(Pattern::parse(p).is_err(), "{p}");
        }
    }
}
