use crate::ast::{Constraint, ConstraintOp, ConstraintValue, Query, TokenPattern};
use crate::{QueryError, Result};

/// Parser for CQL-like query language
///
/// Grammar (informal):
///   query      = sequence ("|" sequence)* within_clause?
///   sequence   = element+
///   element    = atom quantifier?
///   atom       = token_pattern | quoted_word | "(" query ")"
///   quantifier = "+" | "*" | "?" | "{" n "}" | "{" n "," m? "}"
///   token_pattern = "[" constraints? "]"
///   within_clause = "within" layer_name

pub fn parse(input: &str) -> Result<Query> {
	let mut parser = Parser::new(input);
	let query = parser.parse_query()?;
	parser.skip_whitespace();
	if parser.pos < parser.input.len() {
		return Err(QueryError::Parse {
			position: parser.pos,
			message: format!(
				"Unexpected trailing content: {}",
				&parser.input[parser.pos..]
			),
		});
	}
	Ok(query)
}

struct Parser<'a> {
	input: &'a str,
	pos: usize,
}

impl<'a> Parser<'a> {
	fn new(input: &'a str) -> Self {
		Self { input, pos: 0 }
	}

	fn remaining(&self) -> &str {
		&self.input[self.pos..]
	}

	fn peek_char(&self) -> Option<char> {
		self.remaining().chars().next()
	}

	fn skip_whitespace(&mut self) {
		while let Some(c) = self.peek_char() {
			if c.is_whitespace() {
				self.pos += c.len_utf8();
			} else {
				break;
			}
		}
	}

	fn consume(&mut self, expected: char) -> Result<()> {
		self.skip_whitespace();
		match self.peek_char() {
			Some(c) if c == expected => {
				self.pos += c.len_utf8();
				Ok(())
			}
			Some(c) => Err(QueryError::Parse {
				position: self.pos,
				message: format!("Expected '{}', found '{}'", expected, c),
			}),
			None => Err(QueryError::Parse {
				position: self.pos,
				message: format!("Expected '{}', found end of input", expected),
			}),
		}
	}

	fn try_consume(&mut self, expected: char) -> bool {
		self.skip_whitespace();
		if self.peek_char() == Some(expected) {
			self.pos += expected.len_utf8();
			true
		} else {
			false
		}
	}

	fn try_consume_str(&mut self, expected: &str) -> bool {
		self.skip_whitespace();
		if self.remaining().starts_with(expected) {
			let next_char = self.remaining()[expected.len()..].chars().next();
			if let Some(c) = next_char {
				if c.is_alphanumeric() || c == '_' {
					return false;
				}
			}
			self.pos += expected.len();
			true
		} else {
			false
		}
	}

	fn parse_query(&mut self) -> Result<Query> {
		let first = self.parse_sequence()?;

		self.skip_whitespace();
		if !self.try_consume('|') {
			return self.maybe_wrap_within(first);
		}

		let mut alternatives = vec![first];
		loop {
			alternatives.push(self.parse_sequence()?);
			self.skip_whitespace();
			if !self.try_consume('|') {
				break;
			}
		}

		let query = Query::Or(alternatives);
		self.maybe_wrap_within(query)
	}

	fn maybe_wrap_within(&mut self, query: Query) -> Result<Query> {
		self.skip_whitespace();
		if self.try_consume_str("within") {
			self.skip_whitespace();
			let layer = self.parse_identifier()?;
			Ok(Query::Within {
				inner: Box::new(query),
				span_layer: layer,
			})
		} else {
			Ok(query)
		}
	}

	fn parse_sequence(&mut self) -> Result<Query> {
		let mut elements = Vec::new();

		loop {
			self.skip_whitespace();
			if self.is_at_sequence_end() {
				break;
			}

			let element = self.parse_element()?;
			elements.push(element);
		}

		if elements.is_empty() {
			return Err(QueryError::Parse {
				position: self.pos,
				message: "Empty sequence".into(),
			});
		}

		if elements.len() == 1 {
			Ok(elements.remove(0))
		} else {
			Ok(Query::Sequence(elements))
		}
	}

	fn is_at_sequence_end(&self) -> bool {
		match self.peek_char() {
			None => true,
			Some('|') | Some(')') => true,
			Some('w') => self.remaining().starts_with("within"),
			_ => false,
		}
	}

	fn parse_element(&mut self) -> Result<Query> {
		let atom = self.parse_atom()?;
		self.parse_quantifier(atom)
	}

	fn parse_atom(&mut self) -> Result<Query> {
		self.skip_whitespace();

		match self.peek_char() {
			Some('[') => self.parse_token_pattern(),
			Some('"') => self.parse_quoted_word(),
			Some('(') => {
				self.consume('(')?;
				let inner = self.parse_query()?;
				self.consume(')')?;
				Ok(inner)
			}
			Some(c) => Err(QueryError::Parse {
				position: self.pos,
				message: format!("Unexpected character: '{}'", c),
			}),
			None => Err(QueryError::Parse {
				position: self.pos,
				message: "Unexpected end of input".into(),
			}),
		}
	}

	fn parse_quantifier(&mut self, inner: Query) -> Result<Query> {
		self.skip_whitespace();

		match self.peek_char() {
			Some('+') => {
				self.pos += 1;
				Ok(Query::Repetition {
					inner: Box::new(inner),
					min: 1,
					max: None,
				})
			}
			Some('*') => {
				self.pos += 1;
				Ok(Query::Repetition {
					inner: Box::new(inner),
					min: 0,
					max: None,
				})
			}
			Some('?') => {
				self.pos += 1;
				Ok(Query::Repetition {
					inner: Box::new(inner),
					min: 0,
					max: Some(1),
				})
			}
			Some('{') => {
				self.pos += 1;
				self.skip_whitespace();
				let min = self.parse_number()?;
				self.skip_whitespace();

				if self.try_consume('}') {
					Ok(Query::Repetition {
						inner: Box::new(inner),
						min,
						max: Some(min),
					})
				} else {
					self.consume(',')?;
					self.skip_whitespace();

					let max = if self.peek_char() == Some('}') {
						None
					} else {
						Some(self.parse_number()?)
					};

					self.consume('}')?;
					Ok(Query::Repetition {
						inner: Box::new(inner),
						min,
						max,
					})
				}
			}
			_ => Ok(inner),
		}
	}

	fn parse_number(&mut self) -> Result<u32> {
		let start = self.pos;
		while let Some(c) = self.peek_char() {
			if c.is_ascii_digit() {
				self.pos += 1;
			} else {
				break;
			}
		}

		if self.pos == start {
			return Err(QueryError::Parse {
				position: self.pos,
				message: "Expected number".into(),
			});
		}

		self.input[start..self.pos]
			.parse()
			.map_err(|_| QueryError::Parse {
				position: start,
				message: "Invalid number".into(),
			})
	}

	fn parse_identifier(&mut self) -> Result<String> {
		let start = self.pos;
		while let Some(c) = self.peek_char() {
			if c.is_alphanumeric() || c == '_' {
				self.pos += c.len_utf8();
			} else {
				break;
			}
		}

		if self.pos == start {
			return Err(QueryError::Parse {
				position: self.pos,
				message: "Expected identifier".into(),
			});
		}

		Ok(self.input[start..self.pos].to_string())
	}

	fn parse_token_pattern(&mut self) -> Result<Query> {
		self.consume('[')?;
		self.skip_whitespace();

		if self.try_consume(']') {
			return Ok(Query::Token(TokenPattern::any()));
		}

		let mut constraints = Vec::new();
		loop {
			constraints.push(self.parse_constraint()?);
			self.skip_whitespace();
			if !self.try_consume('&') {
				break;
			}
		}

		self.consume(']')?;

		let mut pattern = TokenPattern::new();
		for c in constraints {
			pattern = pattern.with_constraint(c);
		}
		Ok(Query::Token(pattern))
	}

	fn parse_constraint(&mut self) -> Result<Constraint> {
		self.skip_whitespace();
		let layer = self.parse_identifier()?;
		self.skip_whitespace();

		let op = if self.remaining().starts_with("!=") {
			self.pos += 2;
			ConstraintOp::Ne
		} else if self.try_consume('=') {
			ConstraintOp::Eq
		} else {
			return Err(QueryError::Parse {
				position: self.pos,
				message: "Expected '=' or '!='".into(),
			});
		};

		self.skip_whitespace();
		let value = self.parse_constraint_value()?;

		Ok(Constraint { layer, op, value })
	}

	fn parse_constraint_value(&mut self) -> Result<ConstraintValue> {
		match self.peek_char() {
			Some('"') => {
				self.pos += 1;
				let start = self.pos;
				while let Some(c) = self.peek_char() {
					if c == '"' {
						let value = self.input[start..self.pos].to_string();
						self.pos += 1;
						return Ok(ConstraintValue::Literal(value));
					} else if c == '\\' {
						self.pos += 1;
						if self.peek_char().is_some() {
							self.pos += 1;
						}
					} else {
						self.pos += c.len_utf8();
					}
				}
				Err(QueryError::Parse {
					position: start - 1,
					message: "Unterminated string".into(),
				})
			}
			Some('/') => {
				self.pos += 1;
				let start = self.pos;
				while let Some(c) = self.peek_char() {
					if c == '/' {
						let pattern = self.input[start..self.pos].to_string();
						self.pos += 1;
						return Ok(ConstraintValue::Regex(pattern));
					} else if c == '\\' {
						self.pos += 1;
						if self.peek_char().is_some() {
							self.pos += 1;
						}
					} else {
						self.pos += c.len_utf8();
					}
				}
				Err(QueryError::Parse {
					position: start - 1,
					message: "Unterminated regex".into(),
				})
			}
			_ => Err(QueryError::Parse {
				position: self.pos,
				message: "Expected quoted string or regex".into(),
			}),
		}
	}

	fn parse_quoted_word(&mut self) -> Result<Query> {
		self.consume('"')?;
		let start = self.pos;

		while let Some(c) = self.peek_char() {
			if c == '"' {
				let word = self.input[start..self.pos].to_string();
				self.pos += 1;
				return Ok(Query::token(
					TokenPattern::new().with_constraint(Constraint {
						layer: "word".into(),
						op: ConstraintOp::Eq,
						value: ConstraintValue::Literal(word),
					}),
				));
			} else if c == '\\' {
				self.pos += 1;
				if self.peek_char().is_some() {
					self.pos += 1;
				}
			} else {
				self.pos += c.len_utf8();
			}
		}

		Err(QueryError::Parse {
			position: start - 1,
			message: "Unterminated string".into(),
		})
	}
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
			}
			_ => panic!("Expected Token query"),
		}
	}

	#[test]
	fn parse_matchall() {
		let query = parse(r#"[]"#).unwrap();
		match query {
			Query::Token(pattern) => {
				assert!(pattern.constraints.is_empty());
			}
			_ => panic!("Expected Token query"),
		}
	}

	#[test]
	fn parse_negation() {
		let query = parse(r#"[pos!="PUNCT"]"#).unwrap();
		match query {
			Query::Token(pattern) => {
				assert_eq!(pattern.constraints[0].op, ConstraintOp::Ne);
			}
			_ => panic!("Expected Token query"),
		}
	}

	#[test]
	fn parse_sequence() {
		let query = parse(r#"[pos="DET"] [pos="NOUN"]"#).unwrap();
		match query {
			Query::Sequence(parts) => {
				assert_eq!(parts.len(), 2);
			}
			_ => panic!("Expected Sequence query"),
		}
	}

	#[test]
	fn parse_plus_quantifier() {
		let query = parse(r#"[pos="ADJ"]+"#).unwrap();
		match query {
			Query::Repetition { min, max, .. } => {
				assert_eq!(min, 1);
				assert_eq!(max, None);
			}
			_ => panic!("Expected Repetition query"),
		}
	}

	#[test]
	fn parse_star_quantifier() {
		let query = parse(r#"[pos="ADJ"]*"#).unwrap();
		match query {
			Query::Repetition { min, max, .. } => {
				assert_eq!(min, 0);
				assert_eq!(max, None);
			}
			_ => panic!("Expected Repetition query"),
		}
	}

	#[test]
	fn parse_optional_quantifier() {
		let query = parse(r#"[pos="ADV"]?"#).unwrap();
		match query {
			Query::Repetition { min, max, .. } => {
				assert_eq!(min, 0);
				assert_eq!(max, Some(1));
			}
			_ => panic!("Expected Repetition query"),
		}
	}

	#[test]
	fn parse_exact_repetition() {
		let query = parse(r#"[]{3}"#).unwrap();
		match query {
			Query::Repetition { min, max, .. } => {
				assert_eq!(min, 3);
				assert_eq!(max, Some(3));
			}
			_ => panic!("Expected Repetition query"),
		}
	}

	#[test]
	fn parse_range_repetition() {
		let query = parse(r#"[]{2,5}"#).unwrap();
		match query {
			Query::Repetition { min, max, .. } => {
				assert_eq!(min, 2);
				assert_eq!(max, Some(5));
			}
			_ => panic!("Expected Repetition query"),
		}
	}

	#[test]
	fn parse_min_repetition() {
		let query = parse(r#"[]{2,}"#).unwrap();
		match query {
			Query::Repetition { min, max, .. } => {
				assert_eq!(min, 2);
				assert_eq!(max, None);
			}
			_ => panic!("Expected Repetition query"),
		}
	}

	#[test]
	fn parse_alternation() {
		let query = parse(r#"[pos="NOUN"] | [pos="VERB"]"#).unwrap();
		match query {
			Query::Or(alts) => {
				assert_eq!(alts.len(), 2);
			}
			_ => panic!("Expected Or query"),
		}
	}

	#[test]
	fn parse_within_sentence() {
		let query = parse(r#"[pos="DET"] [pos="NOUN"] within s"#).unwrap();
		match query {
			Query::Within { span_layer, .. } => {
				assert_eq!(span_layer, "s");
			}
			_ => panic!("Expected Within query"),
		}
	}

	#[test]
	fn parse_within_document() {
		let query = parse(r#"[lemma="house"] within doc"#).unwrap();
		match query {
			Query::Within { span_layer, .. } => {
				assert_eq!(span_layer, "doc");
			}
			_ => panic!("Expected Within query"),
		}
	}

	#[test]
	fn parse_complex_pattern() {
		let query = parse(r#"[pos="DET"] [pos="ADJ"]* [pos="NOUN"]+ within s"#).unwrap();
		match query {
			Query::Within { inner, span_layer } => {
				assert_eq!(span_layer, "s");
				match *inner {
					Query::Sequence(parts) => {
						assert_eq!(parts.len(), 3);
					}
					_ => panic!("Expected Sequence inside Within"),
				}
			}
			_ => panic!("Expected Within query"),
		}
	}

	#[test]
	fn parse_grouped_alternation() {
		let query = parse(r#"[pos="DET"] ([pos="ADJ"] | [pos="ADV"]) [pos="NOUN"]"#).unwrap();
		match query {
			Query::Sequence(parts) => {
				assert_eq!(parts.len(), 3);
				match &parts[1] {
					Query::Or(alts) => assert_eq!(alts.len(), 2),
					_ => panic!("Expected Or in middle"),
				}
			}
			_ => panic!("Expected Sequence query"),
		}
	}

	#[test]
	fn parse_regex() {
		let query = parse(r#"[lemma=/^un.*/]"#).unwrap();
		match query {
			Query::Token(pattern) => {
				assert_eq!(
					pattern.constraints[0].value,
					ConstraintValue::Regex("^un.*".into())
				);
			}
			_ => panic!("Expected Token query"),
		}
	}

	#[test]
	fn parse_conjunction() {
		let query = parse(r#"[word="house" & pos="NOUN"]"#).unwrap();
		match query {
			Query::Token(pattern) => {
				assert_eq!(pattern.constraints.len(), 2);
				assert_eq!(pattern.constraints[0].layer, "word");
				assert_eq!(pattern.constraints[1].layer, "pos");
			}
			_ => panic!("Expected Token query"),
		}
	}

	#[test]
	fn parse_empty_fails() {
		assert!(parse("").is_err());
		assert!(parse("   ").is_err());
	}

	#[test]
	fn parse_unterminated_bracket_fails() {
		assert!(parse("[pos=\"NOUN\"").is_err());
	}

	#[test]
	fn parse_unterminated_string_fails() {
		assert!(parse(r#""house"#).is_err());
	}
}
