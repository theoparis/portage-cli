use std::fmt;

use winnow::ascii::multispace0;
use winnow::combinator::{alt, cut_err, delimited, dispatch, opt, peek, preceded, repeat};
use winnow::error::StrContext;
use winnow::prelude::*;
use winnow::token::{any, take_while};

use crate::error::{Error, Result};

/// A node in a `REQUIRED_USE` expression tree.
///
/// `REQUIRED_USE` constrains which combinations of USE flags are valid.
/// Introduced in EAPI 4. The `AtMostOne` (`??`) operator was added in EAPI 5.
///
/// See [PMS 7.3.4](https://projects.gentoo.org/pms/9/pms.html#use-state-constraints).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RequiredUseExpr {
    /// A single USE flag (possibly negated with `!`).
    Flag {
        /// Flag name.
        name: String,
        /// `true` if prefixed with `!`.
        negated: bool,
    },
    /// `|| ( ... )` — at least one of the children must be satisfied.
    AnyOf(Vec<RequiredUseExpr>),
    /// `^^ ( ... )` — exactly one of the children must be satisfied (EAPI 4+).
    ExactlyOne(Vec<RequiredUseExpr>),
    /// `?? ( ... )` — at most one of the children may be satisfied (EAPI 5+).
    AtMostOne(Vec<RequiredUseExpr>),
    /// `flag? ( ... )` or `!flag? ( ... )` conditional group.
    UseConditional {
        /// USE flag name.
        flag: String,
        /// `true` for `!flag?` (negated conditional).
        negated: bool,
        /// Children guarded by this flag.
        entries: Vec<RequiredUseExpr>,
    },
    /// Top-level grouping: all children must be satisfied.
    All(Vec<RequiredUseExpr>),
}

impl RequiredUseExpr {
    /// Parse a `REQUIRED_USE` expression string.
    ///
    /// # Examples
    ///
    /// ```
    /// use portage_metadata::RequiredUseExpr;
    ///
    /// let expr = RequiredUseExpr::parse("|| ( flag1 flag2 )").unwrap();
    /// assert!(matches!(expr, RequiredUseExpr::AnyOf(_)));
    ///
    /// let expr = RequiredUseExpr::parse("^^ ( gui qt gtk )").unwrap();
    /// assert!(matches!(expr, RequiredUseExpr::ExactlyOne(_)));
    /// ```
    pub fn parse(input: &str) -> Result<Self> {
        let entries: Vec<RequiredUseExpr> = parse_required_use_string
            .parse(input)
            .map_err(|e| Error::InvalidRequiredUse(format!("{e}")))?;

        Ok(match entries.len() {
            0 => RequiredUseExpr::All(Vec::new()),
            1 => entries.into_iter().next().unwrap(),
            _ => RequiredUseExpr::All(entries),
        })
    }

    /// Return a copy with duplicate entries removed at every level (first occurrence wins).
    pub fn dedup(&self) -> Self {
        match self {
            RequiredUseExpr::Flag { .. } => self.clone(),
            RequiredUseExpr::All(children) => {
                RequiredUseExpr::All(dedup_required_use_children(children))
            }
            RequiredUseExpr::AnyOf(children) => {
                RequiredUseExpr::AnyOf(dedup_required_use_children(children))
            }
            RequiredUseExpr::ExactlyOne(children) => {
                RequiredUseExpr::ExactlyOne(dedup_required_use_children(children))
            }
            RequiredUseExpr::AtMostOne(children) => {
                RequiredUseExpr::AtMostOne(dedup_required_use_children(children))
            }
            RequiredUseExpr::UseConditional {
                flag,
                negated,
                entries,
            } => RequiredUseExpr::UseConditional {
                flag: flag.clone(),
                negated: *negated,
                entries: dedup_required_use_children(entries),
            },
        }
    }
}

fn dedup_required_use_children(children: &[RequiredUseExpr]) -> Vec<RequiredUseExpr> {
    let mut result: Vec<RequiredUseExpr> = Vec::with_capacity(children.len());
    for child in children {
        if !result.contains(child) {
            result.push(child.dedup());
        }
    }
    result
}

impl fmt::Display for RequiredUseExpr {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            RequiredUseExpr::Flag { name, negated } => {
                if *negated {
                    write!(f, "!{name}")
                } else {
                    write!(f, "{name}")
                }
            }
            RequiredUseExpr::AnyOf(entries) => {
                write!(f, "|| ( ")?;
                fmt_entries(f, entries)?;
                write!(f, " )")
            }
            RequiredUseExpr::ExactlyOne(entries) => {
                write!(f, "^^ ( ")?;
                fmt_entries(f, entries)?;
                write!(f, " )")
            }
            RequiredUseExpr::AtMostOne(entries) => {
                write!(f, "?? ( ")?;
                fmt_entries(f, entries)?;
                write!(f, " )")
            }
            RequiredUseExpr::UseConditional {
                flag,
                negated,
                entries,
            } => {
                if *negated {
                    write!(f, "!")?;
                }
                write!(f, "{flag}? ( ")?;
                fmt_entries(f, entries)?;
                write!(f, " )")
            }
            RequiredUseExpr::All(entries) => fmt_entries(f, entries),
        }
    }
}

fn fmt_entries(f: &mut fmt::Formatter, entries: &[RequiredUseExpr]) -> fmt::Result {
    for (i, entry) in entries.iter().enumerate() {
        if i > 0 {
            write!(f, " ")?;
        }
        write!(f, "{entry}")?;
    }
    Ok(())
}

// Winnow parsers

fn is_flag_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '+' || c == '@'
}

fn parse_any_of(input: &mut &str) -> ModalResult<RequiredUseExpr> {
    "||".parse_next(input)?;
    multispace0.parse_next(input)?;
    cut_err(delimited(
        '(',
        parse_required_use_entries,
        (multispace0, ')'),
    ))
    .context(StrContext::Label("'||' group"))
    .map(RequiredUseExpr::AnyOf)
    .parse_next(input)
}

fn parse_exactly_one(input: &mut &str) -> ModalResult<RequiredUseExpr> {
    "^^".parse_next(input)?;
    multispace0.parse_next(input)?;
    cut_err(delimited(
        '(',
        parse_required_use_entries,
        (multispace0, ')'),
    ))
    .context(StrContext::Label("'^^' group"))
    .map(RequiredUseExpr::ExactlyOne)
    .parse_next(input)
}

fn parse_at_most_one(input: &mut &str) -> ModalResult<RequiredUseExpr> {
    "??".parse_next(input)?;
    multispace0.parse_next(input)?;
    cut_err(delimited(
        '(',
        parse_required_use_entries,
        (multispace0, ')'),
    ))
    .context(StrContext::Label("'??' group"))
    .map(RequiredUseExpr::AtMostOne)
    .parse_next(input)
}

fn parse_use_conditional(input: &mut &str) -> ModalResult<RequiredUseExpr> {
    let negated = opt('!').parse_next(input)?.is_some();
    let flag: String = take_while(1.., is_flag_char)
        .verify(|name: &str| {
            // Validate flag name according to PMS 3.1.4
            name.chars()
                .next()
                .is_some_and(|c| c.is_ascii_alphanumeric())
        })
        .map(|s: &str| s.to_string())
        .parse_next(input)?;
    '?'.parse_next(input)?;
    multispace0.parse_next(input)?;
    let entries = cut_err(delimited(
        '(',
        parse_required_use_entries,
        (multispace0, ')'),
    ))
    .context(StrContext::Label("USE conditional group"))
    .parse_next(input)?;
    Ok(RequiredUseExpr::UseConditional {
        flag,
        negated,
        entries,
    })
}

/// Parse a bare flag: `flag` or `!flag`.
fn parse_flag(input: &mut &str) -> ModalResult<RequiredUseExpr> {
    (
        opt('!'),
        take_while(1.., is_flag_char)
            .verify(|name: &str| {
                // Validate flag name according to PMS 3.1.4
                name.chars()
                    .next()
                    .is_some_and(|c| c.is_ascii_alphanumeric())
            })
            .map(|s: &str| s.to_string()),
    )
        .map(|(neg, name)| RequiredUseExpr::Flag {
            name,
            negated: neg.is_some(),
        })
        .parse_next(input)
}

fn parse_paren_group(input: &mut &str) -> ModalResult<Vec<RequiredUseExpr>> {
    delimited(
        '(',
        parse_required_use_entries,
        cut_err((multispace0, ')')).context(StrContext::Label("closing ')'")),
    )
    .parse_next(input)
}

fn parse_required_use_entry(input: &mut &str) -> ModalResult<Vec<RequiredUseExpr>> {
    dispatch! {peek(any);
        '|' => parse_any_of.map(|e| vec![e]),
        '^' => parse_exactly_one.map(|e| vec![e]),
        '(' => parse_paren_group,
        '?' => parse_at_most_one.map(|e| vec![e]),
        _ => alt((
            parse_use_conditional.map(|e| vec![e]),
            parse_flag.map(|e| vec![e]),
        )),
    }
    .parse_next(input)
}

fn parse_required_use_entries(input: &mut &str) -> ModalResult<Vec<RequiredUseExpr>> {
    repeat(0.., preceded(multispace0, parse_required_use_entry))
        .fold(
            Vec::new,
            |mut acc: Vec<RequiredUseExpr>, batch: Vec<RequiredUseExpr>| {
                acc.extend(batch);
                acc
            },
        )
        .parse_next(input)
}

pub(crate) fn parse_required_use_string(input: &mut &str) -> ModalResult<Vec<RequiredUseExpr>> {
    let entries = parse_required_use_entries(input)?;
    multispace0.parse_next(input)?;
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_single_flag() {
        let expr = RequiredUseExpr::parse("ssl").unwrap();
        assert_eq!(
            expr,
            RequiredUseExpr::Flag {
                name: "ssl".to_string(),
                negated: false,
            }
        );
    }

    #[test]
    fn parse_negated_flag() {
        let expr = RequiredUseExpr::parse("!debug").unwrap();
        assert_eq!(
            expr,
            RequiredUseExpr::Flag {
                name: "debug".to_string(),
                negated: true,
            }
        );
    }

    #[test]
    fn parse_any_of() {
        let expr = RequiredUseExpr::parse("|| ( flag1 flag2 )").unwrap();
        match expr {
            RequiredUseExpr::AnyOf(entries) => {
                assert_eq!(entries.len(), 2);
            }
            _ => unreachable!("expected AnyOf"),
        }
    }

    #[test]
    fn parse_exactly_one() {
        let expr = RequiredUseExpr::parse("^^ ( gui qt gtk )").unwrap();
        match expr {
            RequiredUseExpr::ExactlyOne(entries) => {
                assert_eq!(entries.len(), 3);
            }
            _ => unreachable!("expected ExactlyOne"),
        }
    }

    #[test]
    fn parse_at_most_one() {
        let expr = RequiredUseExpr::parse("?? ( flag1 flag2 )").unwrap();
        match expr {
            RequiredUseExpr::AtMostOne(entries) => {
                assert_eq!(entries.len(), 2);
            }
            _ => unreachable!("expected AtMostOne"),
        }
    }

    #[test]
    fn parse_use_conditional() {
        let expr = RequiredUseExpr::parse("ssl? ( gnutls )").unwrap();
        match expr {
            RequiredUseExpr::UseConditional {
                flag,
                negated,
                entries,
            } => {
                assert_eq!(flag, "ssl");
                assert!(!negated);
                assert_eq!(entries.len(), 1);
            }
            _ => unreachable!("expected UseConditional"),
        }
    }

    #[test]
    fn parse_complex() {
        let expr =
            RequiredUseExpr::parse("|| ( python_targets_python3_6 python_targets_python3_7 )")
                .unwrap();
        match expr {
            RequiredUseExpr::AnyOf(entries) => {
                assert_eq!(entries.len(), 2);
            }
            _ => unreachable!("expected AnyOf"),
        }
    }

    #[test]
    fn parse_empty() {
        let expr = RequiredUseExpr::parse("").unwrap();
        assert_eq!(expr, RequiredUseExpr::All(Vec::new()));
    }

    #[test]
    fn display_round_trip_any_of() {
        let input = "|| ( flag1 flag2 )";
        let expr = RequiredUseExpr::parse(input).unwrap();
        let reparsed = RequiredUseExpr::parse(&expr.to_string()).unwrap();
        assert_eq!(expr, reparsed);
    }

    #[test]
    fn display_round_trip_exactly_one() {
        let input = "^^ ( gui qt gtk )";
        let expr = RequiredUseExpr::parse(input).unwrap();
        let reparsed = RequiredUseExpr::parse(&expr.to_string()).unwrap();
        assert_eq!(expr, reparsed);
    }

    #[test]
    fn display_round_trip_at_most_one() {
        let input = "?? ( a b )";
        let expr = RequiredUseExpr::parse(input).unwrap();
        let reparsed = RequiredUseExpr::parse(&expr.to_string()).unwrap();
        assert_eq!(expr, reparsed);
    }

    #[test]
    fn display_round_trip_conditional() {
        let input = "ssl? ( gnutls )";
        let expr = RequiredUseExpr::parse(input).unwrap();
        let reparsed = RequiredUseExpr::parse(&expr.to_string()).unwrap();
        assert_eq!(expr, reparsed);
    }

    #[test]
    fn invalid_flag_starting_with_hyphen() {
        assert!(RequiredUseExpr::parse("-flag").is_err());
    }

    #[test]
    fn invalid_flag_starting_with_at() {
        assert!(RequiredUseExpr::parse("@flag").is_err());
    }

    #[test]
    fn valid_flag_with_at_character() {
        let expr = RequiredUseExpr::parse("flag@name").unwrap();
        assert_eq!(
            expr,
            RequiredUseExpr::Flag {
                name: "flag@name".to_string(),
                negated: false,
            }
        );
    }

    #[test]
    fn valid_use_conditional_with_at() {
        let expr = RequiredUseExpr::parse("flag@name? ( ssl )").unwrap();
        match expr {
            RequiredUseExpr::UseConditional {
                flag,
                negated,
                entries,
            } => {
                assert_eq!(flag, "flag@name");
                assert!(!negated);
                assert_eq!(entries.len(), 1);
            }
            _ => unreachable!("expected UseConditional"),
        }
    }

    #[test]
    fn invalid_use_conditional_flag_starting_with_hyphen() {
        assert!(RequiredUseExpr::parse("-flag? ( ssl )").is_err());
    }
}
