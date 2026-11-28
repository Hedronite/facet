//! Minimal argument parsing for Facet-owned commands. Mirrors upstream's
//! non-interactive style: `--flag value` or `--flag=value`, unknown options
//! rejected as `invalid_arguments`.

use crate::FacetError;

#[derive(Debug, Default)]
pub(crate) struct Parsed {
    positionals: Vec<String>,
    values: Vec<(String, String)>,
    switches: Vec<String>,
}

pub(crate) fn parse(
    args: &[String],
    value_flags: &[&str],
    switch_flags: &[&str],
) -> Result<Parsed, FacetError> {
    let mut parsed = Parsed::default();
    let mut iter = args.iter();
    while let Some(argument) = iter.next() {
        if let Some((flag, value)) = argument.split_once('=')
            && value_flags.contains(&flag)
        {
            parsed.values.push((flag.to_owned(), value.to_owned()));
            continue;
        }
        if value_flags.contains(&argument.as_str()) {
            let value = iter.next().ok_or_else(|| {
                FacetError::invalid_arguments(format!("{argument} requires a value"))
            })?;
            parsed.values.push((argument.clone(), value.clone()));
            continue;
        }
        if switch_flags.contains(&argument.as_str()) {
            if parsed.switches.contains(argument) {
                return Err(FacetError::invalid_arguments(format!(
                    "{argument} may only be specified once"
                )));
            }
            parsed.switches.push(argument.clone());
            continue;
        }
        if argument.starts_with('-') && argument != "-" {
            return Err(FacetError::invalid_arguments(format!(
                "unknown option: {argument}"
            )));
        }
        parsed.positionals.push(argument.clone());
    }
    Ok(parsed)
}

impl Parsed {
    pub(crate) fn positionals(&self) -> &[String] {
        &self.positionals
    }

    /// A single-valued flag; repeating it is an error.
    pub(crate) fn value(&self, flag: &str) -> Result<Option<&str>, FacetError> {
        let mut found = self
            .values
            .iter()
            .filter(|(name, _)| name == flag)
            .map(|(_, value)| value.as_str());
        let first = found.next();
        if found.next().is_some() {
            return Err(FacetError::invalid_arguments(format!(
                "{flag} may only be specified once"
            )));
        }
        Ok(first)
    }

    /// Every value of a repeatable flag, in order.
    pub(crate) fn values(&self, flag: &str) -> Vec<&str> {
        self.values
            .iter()
            .filter(|(name, _)| name == flag)
            .map(|(_, value)| value.as_str())
            .collect()
    }

    pub(crate) fn switch(&self, flag: &str) -> bool {
        self.switches.iter().any(|name| name == flag)
    }

    pub(crate) fn parsed_value<T: std::str::FromStr>(
        &self,
        flag: &str,
        what: &str,
    ) -> Result<Option<T>, FacetError> {
        self.value(flag)?
            .map(|value| {
                value.parse::<T>().map_err(|_| {
                    FacetError::invalid_arguments(format!("{flag} expects {what}, got {value:?}"))
                })
            })
            .transpose()
    }
}

#[cfg(test)]
mod tests {
    use super::parse;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|item| (*item).to_owned()).collect()
    }

    #[test]
    fn parses_values_switches_and_positionals() {
        let parsed = parse(
            &args(&["a", "--limit", "5", "--tag=x", "--tag", "y", "--yes", "b"]),
            &["--limit", "--tag"],
            &["--yes"],
        )
        .unwrap();
        assert_eq!(parsed.positionals(), ["a", "b"]);
        assert_eq!(parsed.value("--limit").unwrap(), Some("5"));
        assert_eq!(parsed.values("--tag"), ["x", "y"]);
        assert!(parsed.switch("--yes"));
        assert_eq!(
            parsed.parsed_value::<usize>("--limit", "a number").unwrap(),
            Some(5)
        );
    }

    #[test]
    fn rejects_unknown_and_duplicate_options() {
        assert!(parse(&args(&["--nope"]), &[], &[]).is_err());
        assert!(parse(&args(&["--yes", "--yes"]), &[], &["--yes"]).is_err());
        assert!(parse(&args(&["--limit"]), &["--limit"], &[]).is_err());
        let parsed = parse(&args(&["--limit", "1", "--limit", "2"]), &["--limit"], &[]).unwrap();
        assert!(parsed.value("--limit").is_err());
        assert!(parsed.parsed_value::<usize>("--limit", "a number").is_err());
    }
}
