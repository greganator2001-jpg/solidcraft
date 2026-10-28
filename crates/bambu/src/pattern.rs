//! The little regular expressions in Bambu Studio's `compatible_printers_condition`
//! (`printer_model=~/.*A1.*/`, `printer_model=~/^Bambu Lab (A1|A1 mini)$/`).
//!
//! Supported: literals, `.`, `*`, `+`, `?` after a single character, `^`, `$`, `\` escapes, and groups
//! `( | )` (optionally `(?: )`) without a quantifier, which are expanded into separate patterns. Anything
//! else is "can't tell" (`None`). Matching is bounded, so a hostile pattern can't hang.

const MAX_EXPANSIONS: usize = 256;
const MAX_STEPS: usize = 200_000;

/// Does `text` contain a match of `pat`? `None` when the pattern uses something not supported.
pub fn search(pat: &str, text: &str) -> Option<bool> {
    let text: Vec<char> = text.chars().collect();
    let mut steps = 0usize;
    for alt in expand(pat)? {
        let toks = tokens(&alt)?;
        for start in 0..=text.len() {
            if seq(&toks, &text, start, &mut steps)? {
                return Some(true);
            }
        }
    }
    Some(false)
}

/// Expand groups `(a|b)` into the plain patterns they stand for.
fn expand(pat: &str) -> Option<Vec<String>> {
    let c: Vec<char> = pat.chars().collect();
    // find the first top-level group
    let mut i = 0;
    while i < c.len() {
        match c[i] {
            '\\' => i += 2,
            '(' => {
                let mut depth = 0usize;
                let mut j = i;
                let mut close = None;
                while j < c.len() {
                    match c[j] {
                        '\\' => j += 1,
                        '(' => depth += 1,
                        ')' => {
                            depth = depth.checked_sub(1)?;
                            if depth == 0 {
                                close = Some(j);
                                break;
                            }
                        }
                        _ => {}
                    }
                    j += 1;
                }
                let close = close?;
                if matches!(c.get(close + 1), Some('*' | '+' | '?')) {
                    return None; // a repeated group: not supported
                }
                let mut inner: String = c[i + 1..close].iter().collect();
                if let Some(rest) = inner.strip_prefix("?:") {
                    inner = rest.to_string();
                }
                let before: String = c[..i].iter().collect();
                let after: String = c[close + 1..].iter().collect();
                let mut out = Vec::new();
                for part in split_top(&inner) {
                    for tail in expand(&format!("{part}{after}"))? {
                        out.push(format!("{before}{tail}"));
                        if out.len() > MAX_EXPANSIONS {
                            return None;
                        }
                    }
                }
                return Some(out);
            }
            _ => i += 1,
        }
    }
    // no group left: split a top-level `|`
    let parts = split_top(pat);
    if parts.len() > 1 {
        let mut out = Vec::new();
        for p in parts {
            out.extend(expand(&p)?);
            if out.len() > MAX_EXPANSIONS {
                return None;
            }
        }
        return Some(out);
    }
    Some(vec![pat.to_string()])
}

/// Split at `|` that isn't inside a group or escaped.
fn split_top(s: &str) -> Vec<String> {
    let mut out = vec![String::new()];
    let mut depth = 0i32;
    let mut esc = false;
    for ch in s.chars() {
        if esc {
            if let Some(last) = out.last_mut() {
                last.push(ch);
            }
            esc = false;
            continue;
        }
        match ch {
            '\\' => esc = true,
            '(' => depth += 1,
            ')' => depth -= 1,
            '|' if depth == 0 => {
                out.push(String::new());
                continue;
            }
            _ => {}
        }
        if let Some(last) = out.last_mut() {
            last.push(ch);
        }
    }
    out
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Atom {
    Char(char),
    Any,
    Start,
    End,
}

#[derive(Debug, Clone, Copy)]
struct Tok {
    atom: Atom,
    min: usize,
    many: bool,
}

fn tokens(p: &str) -> Option<Vec<Tok>> {
    let c: Vec<char> = p.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < c.len() {
        let atom = match c[i] {
            '.' => Atom::Any,
            '^' => Atom::Start,
            '$' => Atom::End,
            '\\' => {
                i += 1;
                Atom::Char(*c.get(i)?)
            }
            '(' | ')' | '[' | ']' | '{' | '}' | '*' | '+' | '?' | '|' => return None,
            ch => Atom::Char(ch),
        };
        i += 1;
        let quant = match c.get(i) {
            Some('*') => Some((0, true)),
            Some('+') => Some((1, true)),
            Some('?') => Some((0, false)),
            _ => None,
        };
        let (min, many) = match quant {
            Some(q) => {
                if matches!(atom, Atom::Start | Atom::End) {
                    return None;
                }
                i += 1;
                q
            }
            None => (1, false),
        };
        out.push(Tok { atom, min, many });
    }
    Some(out)
}

fn one(a: Atom, s: &[char], p: usize) -> bool {
    match a {
        Atom::Char(ch) => s.get(p) == Some(&ch),
        Atom::Any => p < s.len(),
        Atom::Start | Atom::End => false,
    }
}

fn seq(toks: &[Tok], s: &[char], p: usize, steps: &mut usize) -> Option<bool> {
    *steps += 1;
    if *steps > MAX_STEPS {
        return None;
    }
    let Some((t, rest)) = toks.split_first() else { return Some(true) };
    match t.atom {
        Atom::Start => return if p == 0 { seq(rest, s, p, steps) } else { Some(false) },
        Atom::End => return if p == s.len() { seq(rest, s, p, steps) } else { Some(false) },
        _ => {}
    }
    // how many times the atom can repeat from p
    let max = if t.many { usize::MAX } else { 1 };
    let mut n = 0;
    while n < max && one(t.atom, s, p + n) {
        n += 1;
    }
    if n < t.min {
        return Some(false);
    }
    let mut k = n;
    loop {
        if seq(rest, s, p + k, steps)? {
            return Some(true);
        }
        if k == t.min {
            return Some(false);
        }
        k -= 1;
    }
}

#[cfg(test)]
mod tests {
    use super::search;

    #[test]
    fn bambu_conditions() {
        assert_eq!(search(".*A1.*", "Bambu Lab A1 mini"), Some(true));
        assert_eq!(search("^Bambu Lab A1$", "Bambu Lab A1"), Some(true));
        assert_eq!(search("^Bambu Lab A1$", "Bambu Lab A1 mini"), Some(false));
        assert_eq!(search("^Bambu Lab (A1|H2S)$", "Bambu Lab H2S"), Some(true));
        assert_eq!(search("^Bambu Lab (A1|H2S)$", "Bambu Lab H2D"), Some(false));
        assert_eq!(search("^Bambu Lab (?:A1( mini)?|P1S)$", "Bambu Lab A1 mini"), None); // group with ?: unsupported
        assert_eq!(search("X1", "Bambu Lab X1 Carbon"), Some(true));
        assert_eq!(search("a+b?c", "aaac"), Some(true));
        assert_eq!(search("a+b?c", "bc"), Some(false));
        assert_eq!(search("A1|H2", "Bambu Lab H2S"), Some(true));
        assert_eq!(search("P1[S]", "x"), None);
        // a pathological pattern stops instead of hanging
        let long = "a".repeat(5000);
        let _ = search(".*.*.*.*.*.*b", &long);
    }
}
