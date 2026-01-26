use crate::ast::{Constraint, ConstraintOp, ConstraintValue, Query, TokenPattern};
use crate::{QueryError, Result};

pub fn parse(input: &str) -> Result<Query> {
	let input = input.trim();

	if input.is_empty() {
		return Err(QueryError::Parse {
			position: 0,
			message: "Empty query".into(),
		});
	}

	let mut tokens = Vec::new();
	let mut pos = 0;

	while pos < input.len() {
		let remaining = &input[pos..];

		if remaining.starts_with(char::is_whitespace) {
			pos += remaining.chars().next().unwrap().len_utf8();
			continue;
		}

		if remaining.starts_with('[') {
			let (pattern, consumed) = parse_token_pattern(remaining)?;
			tokens.push(Query::Token(pattern));
			pos += consumed;
		} else if remaining.starts_with('"') {
			let (word, consumed) = parse_quoted_word(remaining)?;
			tokens.push(Query::token(
				TokenPattern::new().with_constraint(Constraint {
					layer: "word".into(),
					op: ConstraintOp::Eq,
					value: ConstraintValue::Literal(word),
				}),
			));
			pos += consumed;
		} else {
			return Err(QueryError::Parse {
				position: pos,
				message: format!("Unexpected character: {}", remaining.chars().next().unwrap()),
			});
		}
	}

	if tokens.is_empty() {
		return Err(QueryError::Parse {
			position: 0,
			message: "Empty query".into(),
		});
	}

	if tokens.len() == 1 {
		Ok(tokens.remove(0))
	} else {
		Ok(Query::Sequence(tokens))
	}
}

fn parse_quoted_word(input: &str) -> Result<(String, usize)> {
	if !input.starts_with('"') {
		return Err(QueryError::Parse {
			position: 0,
			message: "Expected opening quote".into(),
		});
	}

	let end = input[1..].find('"').ok_or_else(|| QueryError::Parse {
		position: 0,
		message: "Unterminated string".into(),
	})?;

	let word = input[1..=end].to_string();
	Ok((word, end + 2))
}

fn parse_token_pattern(input: &str) -> Result<(TokenPattern, usize)> {
	if !input.starts_with('[') {
		return Err(QueryError::Parse {
			position: 0,
			message: "Expected '['".into(),
		});
	}

	let end = input.find(']').ok_or_else(|| QueryError::Parse {
		position: 0,
		message: "Unterminated token pattern".into(),
	})?;

	let inner = &input[1..end];
	let mut pattern = TokenPattern::new();

	if !inner.trim().is_empty() {
		for part in inner.split('&') {
			let constraint = parse_constraint(part.trim())?;
			pattern = pattern.with_constraint(constraint);
		}
	}

	Ok((pattern, end + 1))
}

fn parse_constraint(input: &str) -> Result<Constraint> {
	let (layer, op, value_str) = if let Some(idx) = input.find("!=") {
		(&input[..idx], ConstraintOp::Ne, &input[idx + 2..])
	} else if let Some(idx) = input.find('=') {
		(&input[..idx], ConstraintOp::Eq, &input[idx + 1..])
	} else {
		return Err(QueryError::Parse {
			position: 0,
			message: format!("Invalid constraint syntax: {}", input),
		});
	};

	let layer = layer.trim().to_string();
	let value_str = value_str.trim();

	let value = if value_str.starts_with('"') && value_str.ends_with('"') {
		ConstraintValue::Literal(value_str[1..value_str.len() - 1].to_string())
	} else if value_str.starts_with('/') && value_str.ends_with('/') {
		ConstraintValue::Regex(value_str[1..value_str.len() - 1].to_string())
	} else {
		return Err(QueryError::Parse {
			position: 0,
			message: format!("Value must be quoted or regex: {}", value_str),
		});
	};

	Ok(Constraint { layer, op, value })
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn parse_simple_word() {
		let query = parse(r#""house""#).unwrap();
		match query {
			Query::Token(pattern) => {
				assert_eq!(pattern.constraints.len(), 1);
				assert_eq!(pattern.constraints[0].layer, "word");
				assert_eq!(
					pattern.constraints[0].value,
					ConstraintValue::Literal("house".into())
				);
			}
			_ => panic!("Expected Token query"),
		}
	}

	#[test]
	fn parse_token_pattern() {
		let query = parse(r#"[pos="NOUN"]"#).unwrap();
		match query {
			Query::Token(pattern) => {
				assert_eq!(pattern.constraints.len(), 1);
				assert_eq!(pattern.constraints[0].layer, "pos");
				assert_eq!(
					pattern.constraints[0].value,
					ConstraintValue::Literal("NOUN".into())
				);
			}
			_ => panic!("Expected Token query"),
		}
	}

	#[test]
	fn parse_sequence() {
		let query = parse(r#"[pos="ADJ"] [word="bibelots"]"#).unwrap();
		match query {
			Query::Sequence(parts) => {
				assert_eq!(parts.len(), 2);
			}
			_ => panic!("Expected Sequence query"),
		}
	}

	#[test]
	fn parse_regex() {
		let query = parse(r#"[lemma=/^un.*/]"#).unwrap();
		match query {
			Query::Token(pattern) => {
				assert_eq!(pattern.constraints[0].layer, "lemma");
				assert_eq!(
					pattern.constraints[0].value,
					ConstraintValue::Regex("^un.*".into())
				);
			}
			_ => panic!("Expected Token query"),
		}
	}

	#[test]
	fn parse_empty_fails() {
		assert!(parse("").is_err());
		assert!(parse("   ").is_err());
	}
}
