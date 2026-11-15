//! Rules: the engine's tuning values as settings, each with a name, a default, a lowest and a highest value.
//! A group is the rules of one part of the engine, a struct in that part's file (`rule_group!` can write one);
//! `Options` holds one of each and reaches every rule by its full name, the group's prefix and then the
//! rule's own (`lossy_min_pixels`). A rule that is not set has its default.

use crate::api::{ArchiveRules, Error, Options};
use crate::flate::FlateRules;
use crate::pdf::lossy::LossyRules;

/// A group of rules: a struct with one `u64` field a rule, today's values as its default, and the table,
/// `set` and `get` every group has. One line a rule, `name = default, lowest, highest;`, after its doc comment.
macro_rules! rule_group {
    ($(#[$about:meta])* $vis:vis struct $group:ident {
        $($(#[$doc:meta])* $name:ident = $default:expr, $lowest:expr, $highest:expr;)*
    }) => {
        $(#[$about])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        $vis struct $group {
            $($(#[$doc])* pub(crate) $name: u64,)*
        }

        impl Default for $group {
            fn default() -> $group {
                $group { $($name: $default,)* }
            }
        }

        impl $group {
            /// Every rule of the group, in a fixed order: its name (without the group's prefix), its
            /// default, and the lowest and the highest value it takes.
            pub const RULES: &'static [(&'static str, u64, u64, u64)] =
                &[$((stringify!($name), $default, $lowest, $highest),)*];

            /// Sets the rule called `name`. False, and nothing is set, when there is no such rule or `value`
            /// is outside its range.
            pub fn set(&mut self, name: &str, value: u64) -> bool {
                match name {
                    $(stringify!($name) if ($lowest..=$highest).contains(&value) => self.$name = value,)*
                    _ => return false,
                }
                true
            }

            /// The value of the rule called `name`; None when there is no such rule.
            pub fn get(&self, name: &str) -> Option<u64> {
                Some(match name {
                    $(stringify!($name) => self.$name,)*
                    _ => return None,
                })
            }
        }
    };
}
pub(crate) use rule_group;

/// A group as the options reach it: its prefix, its table, and how to set and to read one of its rules.
type Group = (
    &'static str,
    &'static [(&'static str, u64, u64, u64)],
    fn(&mut Options, &str, u64) -> bool,
    fn(&Options, &str) -> Option<u64>,
);

/// Every group. A new one is its field in `Options` and one line here.
const GROUPS: &[Group] = &[
    ("lossy_", LossyRules::RULES, |o, n, v| o.lossy_rules.set(n, v), |o, n| o.lossy_rules.get(n)),
    ("flate_", FlateRules::RULES, |o, n, v| o.flate_rules.set(n, v), |o, n| o.flate_rules.get(n)),
    ("archive_", ArchiveRules::RULES, |o, n, v| o.archive_rules.set(n, v), |o, n| o.archive_rules.get(n)),
];

/// The group `name` belongs to, and the name without the group's prefix.
fn group(name: &str) -> Option<(&'static Group, &str)> {
    GROUPS.iter().find_map(|g| Some((g, name.strip_prefix(g.0)?)))
}

impl Options {
    /// Every rule there is: its full name, its default, and the lowest and the highest value it takes.
    pub fn rules() -> Vec<(String, u64, u64, u64)> {
        GROUPS.iter().flat_map(|g| g.1.iter().map(|r| (format!("{}{}", g.0, r.0), r.1, r.2, r.3))).collect()
    }

    /// The value of the rule called `name`; None when there is no such rule.
    pub fn rule(&self, name: &str) -> Option<u64> {
        let (g, rest) = group(name)?;
        g.3(self, rest)
    }

    /// Sets the rule called `name`. A name that is no rule, or a value outside the rule's range, is an error
    /// that says which values the rule takes, and nothing is set.
    pub fn set_rule(&mut self, name: &str, value: u64) -> Result<(), Error> {
        if group(name).is_some_and(|(g, rest)| g.2(self, rest, value)) {
            return Ok(());
        }
        Err(Error::InvalidOptions(match Options::rules().into_iter().find(|r| r.0 == name) {
            Some((_, _, lowest, highest)) => format!("rule {name} is {value}: it takes {lowest} to {highest}"),
            None => format!("there is no rule called {name}"),
        }))
    }

    /// Sets rules from text, `name=value` with commas between (`lossy_min_saving=20,lossy_photo_floor=100`;
    /// spaces around a name or a value are skipped, and the empty text sets nothing). On an error no rule
    /// of the text is set.
    pub fn set_rules(&mut self, text: &str) -> Result<(), Error> {
        let mut with = self.clone();
        for item in text.split(',').filter(|_| !text.trim().is_empty()) {
            let bad =
                || Error::InvalidOptions(format!("rules are name=value with commas between, and {item:?} is not"));
            let (name, value) = item.split_once('=').ok_or_else(bad)?;
            with.set_rule(name.trim(), value.trim().parse().map_err(|_| bad())?)?;
        }
        *self = with;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rules_are_reached_by_name() {
        let d = Options::default();
        let all = Options::rules();
        assert!(!all.is_empty());
        for (name, default, lowest, highest) in &all {
            assert!(lowest <= default && default <= highest, "{name}");
            // Unset is the default, and so is the default set by hand.
            let mut o = d.clone();
            assert_eq!(o.rule(name), Some(*default), "{name}");
            assert!(o.set_rule(name, *default).is_ok() && o == d, "{name}");
            // Both ends are taken, and the values next to them are refused with the range in the message.
            for end in [*lowest, *highest] {
                assert!(o.set_rule(name, end).is_ok() && o.rule(name) == Some(end), "{name}");
            }
            for out in [lowest.checked_sub(1), highest.checked_add(1)].into_iter().flatten() {
                let e = o.set_rule(name, out).unwrap_err().to_string();
                assert!(e.contains(&format!("rule {name} is {out}: it takes {lowest} to {highest}")), "{e}");
                assert_eq!(o.rule(name), Some(*highest), "{name}");
            }
        }
        // A name is unique, and one that is no rule is an error.
        let mut names: Vec<&String> = all.iter().map(|r| &r.0).collect();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), all.len());
        let mut o = d.clone();
        for bad in ["", "lossy_", "min_pixels", "lossy_no_such_rule", "other_min_pixels"] {
            assert!(o.set_rule(bad, 1).unwrap_err().to_string().contains("there is no rule called"), "{bad}");
            assert!(o.rule(bad).is_none() && o == d, "{bad}");
        }
    }

    #[test]
    fn rules_are_set_from_text() {
        let d = Options::default();
        let (a, b) = (&Options::rules()[0], &Options::rules()[1]);
        let mut o = d.clone();
        assert!(o.set_rules("").is_ok() && o.set_rules("  ").is_ok() && o == d);
        assert!(o.set_rules(&format!(" {} = {} ,{}={}", a.0, a.3, b.0, b.2)).is_ok());
        assert_eq!((o.rule(&a.0), o.rule(&b.0)), (Some(a.3), Some(b.2)));
        // On an error nothing of the text is set: the first rule here is a good one.
        let was = o.clone();
        for bad in [
            format!("{}={},nothing=1", a.0, a.1),
            format!("{}={},{}", a.0, a.1, b.0),
            format!("{}={},{}=", a.0, a.1, b.0),
            format!("{}={},{}=-1", a.0, a.1, b.0),
            format!("{}={},{}=1.5", a.0, a.1, b.0),
            format!("{}={},", a.0, a.1),
            format!("{}={};{}={}", a.0, a.1, b.0, b.1),
        ] {
            assert!(matches!(o.set_rules(&bad), Err(Error::InvalidOptions(_))), "{bad}");
            assert!(o == was, "{bad}");
        }
    }
}
