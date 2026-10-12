//! `engines.superDesktop` ranges: space-separated comparators (all must hold),
//! alternatives joined by `||`. Comparators: `>=`, `>`, `<=`, `<`, `=`, `^`,
//! `~`, a bare version (exact) or `*`. Pre-release suffixes are ignored, as in
//! `updates::Version`.
use crate::updates::Version;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Range(Vec<Vec<(Op, Version)>>);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Op {
    Ge,
    Gt,
    Le,
    Lt,
    Eq,
}

impl Range {
    pub fn parse(text: &str) -> Option<Self> {
        let mut alternatives = Vec::new();
        for alternative in text.split("||") {
            let mut all = Vec::new();
            for comparator in alternative.split_whitespace() {
                all.extend(comparator_bounds(comparator)?);
            }
            if alternative.trim().is_empty() {
                return None;
            }
            alternatives.push(all);
        }
        (!alternatives.is_empty()).then_some(Range(alternatives))
    }

    pub fn contains(&self, version: Version) -> bool {
        self.0.iter().any(|all| {
            all.iter().all(|(op, bound)| match op {
                Op::Ge => version >= *bound,
                Op::Gt => version > *bound,
                Op::Le => version <= *bound,
                Op::Lt => version < *bound,
                Op::Eq => version == *bound,
            })
        })
    }
}

fn comparator_bounds(text: &str) -> Option<Vec<(Op, Version)>> {
    if text == "*" {
        return Some(Vec::new());
    }
    for (prefix, op) in [(">=", Op::Ge), ("<=", Op::Le), (">", Op::Gt), ("<", Op::Lt), ("=", Op::Eq)] {
        if let Some(rest) = text.strip_prefix(prefix) {
            return Some(vec![(op, Version::parse(rest)?)]);
        }
    }
    if let Some(rest) = text.strip_prefix('^') {
        let v = Version::parse(rest)?;
        let upper = if v.0 > 0 {
            Version(v.0 + 1, 0, 0)
        } else if v.1 > 0 {
            Version(0, v.1 + 1, 0)
        } else {
            Version(0, 0, v.2 + 1)
        };
        return Some(vec![(Op::Ge, v), (Op::Lt, upper)]);
    }
    if let Some(rest) = text.strip_prefix('~') {
        let v = Version::parse(rest)?;
        return Some(vec![(Op::Ge, v), (Op::Lt, Version(v.0, v.1 + 1, 0))]);
    }
    Some(vec![(Op::Eq, Version::parse(text)?)])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(text: &str) -> Version {
        Version::parse(text).unwrap()
    }

    #[test]
    fn plugin_spec_ranges() {
        let r = Range::parse(">=1.2.0").unwrap();
        assert!(r.contains(v("1.2.0")) && r.contains(v("2.0.0")) && !r.contains(v("1.1.20")));
        let r = Range::parse(">=1.2.0 <2.0.0").unwrap();
        assert!(r.contains(v("1.9.9")) && !r.contains(v("2.0.0")));
        let r = Range::parse("^1.2.3").unwrap();
        assert!(r.contains(v("1.9.0")) && !r.contains(v("2.0.0")) && !r.contains(v("1.2.2")));
        let r = Range::parse("^0.3.1").unwrap();
        assert!(r.contains(v("0.3.9")) && !r.contains(v("0.4.0")));
        let r = Range::parse("~1.2.0").unwrap();
        assert!(r.contains(v("1.2.7")) && !r.contains(v("1.3.0")));
        let r = Range::parse("1.0.0 || >=1.5.0").unwrap();
        assert!(r.contains(v("1.0.0")) && !r.contains(v("1.2.0")) && r.contains(v("1.6.0")));
        assert!(Range::parse("*").unwrap().contains(v("0.0.1")));
        for bad in ["", ">=", "1.2", "latest", ">=1.2.0 ||", "≥1.0.0"] {
            assert!(Range::parse(bad).is_none(), "{bad}");
        }
    }
}
