use std::io::{BufRead, BufReader, Read};

use crate::format::{CorpusReader, ParsedSentence, ParsedToken};
use crate::{BuildError, Result};
use montre_core::Span;

pub struct ConllUReader<R> {
	reader: BufReader<R>,
	current_position: u64,
	line_number: usize,
}

impl<R: Read> ConllUReader<R> {
	pub fn new(reader: R) -> Self {
		Self {
			reader: BufReader::new(reader),
			current_position: 0,
			line_number: 0,
		}
	}

	fn parse_sentence(&mut self, lines: &[String], start_line: usize) -> Result<ParsedSentence> {
		let start_position = self.current_position;
		let mut tokens = Vec::new();

		for (offset, line) in lines.iter().enumerate() {
			if line.starts_with('#') {
				continue;
			}

			let fields: Vec<&str> = line.split('\t').collect();
			if fields.len() < 10 {
				return Err(BuildError::Parse {
					line: start_line + offset,
					message: format!("Expected 10 fields, found {}", fields.len()),
				});
			}

			let id = fields[0];
			if id.contains('-') || id.contains('.') {
				continue;
			}

			let token = ParsedToken {
				word: fields[1].to_string(),
				lemma: non_empty(fields[2]),
				pos: non_empty(fields[3]),
				xpos: non_empty(fields[4]),
				feats: non_empty(fields[5]),
				head: fields[6].parse().ok(),
				deprel: non_empty(fields[7]),
			};

			tokens.push(token);
			self.current_position += 1;
		}

		Ok(ParsedSentence {
			span: Span::new(start_position, self.current_position),
			tokens,
		})
	}
}

fn non_empty(s: &str) -> Option<String> {
	if s == "_" || s.is_empty() {
		None
	} else {
		Some(s.to_string())
	}
}

impl<R: Read> CorpusReader for ConllUReader<R> {
	fn read_sentences(&mut self) -> Result<Vec<ParsedSentence>> {
		let mut sentences = Vec::new();
		let mut current_lines = Vec::new();
		let mut sentence_start_line = 1;
		let mut line = String::new();

		loop {
			line.clear();
			let bytes_read = self.reader.read_line(&mut line)?;
			self.line_number += 1;

			if bytes_read == 0 {
				if !current_lines.is_empty() {
					let sentence = self.parse_sentence(&current_lines, sentence_start_line)?;
					if !sentence.tokens.is_empty() {
						sentences.push(sentence);
					}
				}
				break;
			}

			let trimmed = line.trim();
			if trimmed.is_empty() {
				if !current_lines.is_empty() {
					let sentence = self.parse_sentence(&current_lines, sentence_start_line)?;
					if !sentence.tokens.is_empty() {
						sentences.push(sentence);
					}
					current_lines.clear();
				}
				sentence_start_line = self.line_number + 1;
			} else {
				current_lines.push(trimmed.to_string());
			}
		}

		Ok(sentences)
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use std::io::Cursor;

	const SAMPLE_CONLLU: &str = r#"# sent_id = 1
# text = The cat sat.
1	The	the	DET	DT	_	2	det	_	_
2	cat	cat	NOUN	NN	_	3	nsubj	_	_
3	sat	sit	VERB	VBD	_	0	root	_	_
4	.	.	PUNCT	.	_	3	punct	_	_

# sent_id = 2
# text = Dogs bark.
1	Dogs	dog	NOUN	NNS	_	2	nsubj	_	_
2	bark	bark	VERB	VBP	_	0	root	_	_
3	.	.	PUNCT	.	_	2	punct	_	_
"#;

	#[test]
	fn parse_conllu() {
		let cursor = Cursor::new(SAMPLE_CONLLU);
		let mut reader = ConllUReader::new(cursor);
		let sentences = reader.read_sentences().unwrap();

		assert_eq!(sentences.len(), 2);

		assert_eq!(sentences[0].tokens.len(), 4);
		assert_eq!(sentences[0].tokens[0].word, "The");
		assert_eq!(sentences[0].tokens[0].lemma, Some("the".into()));
		assert_eq!(sentences[0].tokens[0].pos, Some("DET".into()));
		assert_eq!(sentences[0].span.start, 0);
		assert_eq!(sentences[0].span.end, 4);

		assert_eq!(sentences[1].tokens.len(), 3);
		assert_eq!(sentences[1].tokens[0].word, "Dogs");
		assert_eq!(sentences[1].span.start, 4);
		assert_eq!(sentences[1].span.end, 7);
	}
}
