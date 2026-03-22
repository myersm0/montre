use std::collections::HashMap;
use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};

use montre_core::Value;
use montre_index::forward::InMemoryForward;
use montre_index::forward_flat;
use montre_index::ForwardIndex;

const TAG_ABSENT: u8 = 0;
const TAG_STRING: u8 = 1;
const TAG_INT: u8 = 2;

struct LayerFile {
	writer: BufWriter<File>,
	path: PathBuf,
	positions_written: u64,
	is_numeric: bool,
}

pub struct StreamingForwardWriter {
	temp_dir: PathBuf,
	layers: Vec<(String, LayerFile)>,
	layer_index: HashMap<String, usize>,
	token_count: u64,
}

impl StreamingForwardWriter {
	pub fn new(temp_dir: &Path) -> std::io::Result<Self> {
		std::fs::create_dir_all(temp_dir)?;
		Ok(Self {
			temp_dir: temp_dir.to_path_buf(),
			layers: Vec::new(),
			layer_index: HashMap::new(),
			token_count: 0,
		})
	}

	pub fn append_from(&mut self, forward: InMemoryForward, offset: u64) -> std::io::Result<()> {
		let file_tokens = forward.token_count();
		if file_tokens == 0 {
			return Ok(());
		}

		for name in forward.layer_names() {
			let data = match forward.layer_data(name) {
				Some(d) => d,
				None => continue,
			};

			let is_numeric = montre_index::is_numeric_layer(data);
			let layer_file = self.get_or_create_layer(name, is_numeric)?;

			pad_absent(&mut layer_file.writer, layer_file.positions_written, offset)?;

			for val in data.iter() {
				write_value(&mut layer_file.writer, val)?;
			}
			layer_file.positions_written = offset + data.len() as u64;
		}

		self.token_count = self.token_count.max(offset + file_tokens);
		Ok(())
	}

	pub fn finalize(&mut self, output_path: &Path) -> std::io::Result<()> {
		for (_, layer_file) in &mut self.layers {
			pad_absent(&mut layer_file.writer, layer_file.positions_written, self.token_count)?;
			layer_file.writer.flush()?;
		}

		let mut layer_builds = Vec::with_capacity(self.layers.len());
		for (name, layer_file) in &self.layers {
			let values = read_temp_layer(&layer_file.path, self.token_count)?;
			let build = if layer_file.is_numeric {
				forward_flat::build_dense_numeric_layer(name, &values, self.token_count)
			} else {
				forward_flat::build_dict_encoded_layer(name, &values)
			};
			layer_builds.push(build);
		}

		forward_flat::write_mfwd(&layer_builds, self.token_count, output_path)
	}

	fn get_or_create_layer(&mut self, name: &str, is_numeric: bool) -> std::io::Result<&mut LayerFile> {
		if let Some(&idx) = self.layer_index.get(name) {
			return Ok(&mut self.layers[idx].1);
		}

		let path = self.temp_dir.join(format!("layer_{}.tmp", self.layers.len()));
		let file = File::create(&path)?;
		let writer = BufWriter::with_capacity(256 * 1024, file);
		let layer_file = LayerFile {
			writer,
			path,
			positions_written: 0,
			is_numeric,
		};

		let idx = self.layers.len();
		self.layers.push((name.to_string(), layer_file));
		self.layer_index.insert(name.to_string(), idx);
		Ok(&mut self.layers[idx].1)
	}
}

impl Drop for StreamingForwardWriter {
	fn drop(&mut self) {
		let _ = std::fs::remove_dir_all(&self.temp_dir);
	}
}

fn pad_absent(writer: &mut BufWriter<File>, from: u64, to: u64) -> std::io::Result<()> {
	for _ in from..to {
		writer.write_all(&[TAG_ABSENT])?;
	}
	Ok(())
}

fn write_value(writer: &mut BufWriter<File>, val: &Value) -> std::io::Result<()> {
	match val {
		Value::Str(s) if !s.is_empty() => {
			writer.write_all(&[TAG_STRING])?;
			let bytes = s.as_bytes();
			writer.write_all(&(bytes.len() as u16).to_le_bytes())?;
			writer.write_all(bytes)?;
		}
		Value::Int(n) => {
			writer.write_all(&[TAG_INT])?;
			writer.write_all(&n.to_le_bytes())?;
		}
		_ => {
			writer.write_all(&[TAG_ABSENT])?;
		}
	}
	Ok(())
}

fn read_temp_layer(path: &Path, token_count: u64) -> std::io::Result<Vec<Value>> {
	let file = File::open(path)?;
	let mut reader = BufReader::with_capacity(256 * 1024, file);
	let mut values = Vec::with_capacity(token_count as usize);

	let mut tag_buf = [0u8; 1];
	let mut len_buf = [0u8; 2];
	let mut int_buf = [0u8; 8];

	for _ in 0..token_count {
		reader.read_exact(&mut tag_buf)?;
		match tag_buf[0] {
			TAG_ABSENT => {
				values.push(Value::from(""));
			}
			TAG_STRING => {
				reader.read_exact(&mut len_buf)?;
				let len = u16::from_le_bytes(len_buf) as usize;
				let mut buf = vec![0u8; len];
				reader.read_exact(&mut buf)?;
				let s = std::str::from_utf8(&buf).map_err(|e| {
					std::io::Error::new(std::io::ErrorKind::InvalidData, e)
				})?;
				values.push(Value::from(s));
			}
			TAG_INT => {
				reader.read_exact(&mut int_buf)?;
				let n = i64::from_le_bytes(int_buf);
				values.push(Value::Int(n));
			}
			other => {
				return Err(std::io::Error::new(
					std::io::ErrorKind::InvalidData,
					format!("invalid tag byte: 0x{:02x}", other),
				));
			}
		}
	}

	Ok(values)
}

#[cfg(test)]
mod tests {
	use super::*;
	use montre_index::forward_flat::MappedForward;

	fn make_forward(words: &[&str], pos_tags: &[&str]) -> InMemoryForward {
		let mut fwd = InMemoryForward::new();
		let word = fwd.add_layer("word");
		let pos = fwd.add_layer("pos");
		for (i, (&w, &p)) in words.iter().zip(pos_tags.iter()).enumerate() {
			fwd.set(word, i as u64, Value::from(w));
			fwd.set(pos, i as u64, Value::from(p));
		}
		fwd
	}

	#[test]
	fn roundtrip_single_append() {
		let dir = tempfile::tempdir().unwrap();
		let temp = dir.path().join("tmp");
		let out = dir.path().join("forward.bin");

		let mut writer = StreamingForwardWriter::new(&temp).unwrap();
		let fwd = make_forward(&["the", "cat", "sat"], &["DET", "NOUN", "VERB"]);
		writer.append_from(fwd, 0).unwrap();
		writer.finalize(&out).unwrap();

		let mapped = MappedForward::open(&out).unwrap();
		assert_eq!(mapped.token_count(), 3);
		assert_eq!(mapped.get_str(0, "word"), Some("the"));
		assert_eq!(mapped.get_str(1, "word"), Some("cat"));
		assert_eq!(mapped.get_str(2, "word"), Some("sat"));
		assert_eq!(mapped.get_str(0, "pos"), Some("DET"));
		assert_eq!(mapped.get_str(1, "pos"), Some("NOUN"));
		assert_eq!(mapped.get_str(2, "pos"), Some("VERB"));
	}

	#[test]
	fn roundtrip_two_appends() {
		let dir = tempfile::tempdir().unwrap();
		let temp = dir.path().join("tmp");
		let out = dir.path().join("forward.bin");

		let mut writer = StreamingForwardWriter::new(&temp).unwrap();
		let fwd1 = make_forward(&["the", "cat"], &["DET", "NOUN"]);
		let fwd2 = make_forward(&["sat", "down"], &["VERB", "ADV"]);
		writer.append_from(fwd1, 0).unwrap();
		writer.append_from(fwd2, 2).unwrap();
		writer.finalize(&out).unwrap();

		let mapped = MappedForward::open(&out).unwrap();
		assert_eq!(mapped.token_count(), 4);
		assert_eq!(mapped.get_str(0, "word"), Some("the"));
		assert_eq!(mapped.get_str(1, "word"), Some("cat"));
		assert_eq!(mapped.get_str(2, "word"), Some("sat"));
		assert_eq!(mapped.get_str(3, "word"), Some("down"));
		assert_eq!(mapped.get_str(2, "pos"), Some("VERB"));
		assert_eq!(mapped.get_str(3, "pos"), Some("ADV"));
	}

	#[test]
	fn roundtrip_with_gap() {
		let dir = tempfile::tempdir().unwrap();
		let temp = dir.path().join("tmp");
		let out = dir.path().join("forward.bin");

		let mut writer = StreamingForwardWriter::new(&temp).unwrap();
		let fwd1 = make_forward(&["hello"], &["INTJ"]);
		let fwd2 = make_forward(&["world"], &["NOUN"]);
		writer.append_from(fwd1, 0).unwrap();
		writer.append_from(fwd2, 5).unwrap();
		writer.finalize(&out).unwrap();

		let mapped = MappedForward::open(&out).unwrap();
		assert_eq!(mapped.token_count(), 6);
		assert_eq!(mapped.get_str(0, "word"), Some("hello"));
		assert_eq!(mapped.get_str(5, "word"), Some("world"));
		assert_eq!(mapped.get_str(1, "word"), None);
		assert_eq!(mapped.get_str(4, "word"), None);
	}

	#[test]
	fn roundtrip_sparse_layer() {
		let dir = tempfile::tempdir().unwrap();
		let temp = dir.path().join("tmp");
		let out = dir.path().join("forward.bin");

		let mut fwd = InMemoryForward::new();
		let word = fwd.add_layer("word");
		let feats = fwd.add_layer("feats.Number");
		for i in 0..5u64 {
			fwd.set(word, i, Value::from(format!("w{}", i)));
		}
		fwd.set(feats, 1, Value::from("Sing"));
		fwd.set(feats, 3, Value::from("Plur"));

		let mut writer = StreamingForwardWriter::new(&temp).unwrap();
		writer.append_from(fwd, 0).unwrap();
		writer.finalize(&out).unwrap();

		let mapped = MappedForward::open(&out).unwrap();
		assert_eq!(mapped.get_str(1, "feats.Number"), Some("Sing"));
		assert_eq!(mapped.get_str(3, "feats.Number"), Some("Plur"));
		assert_eq!(mapped.get_str(0, "feats.Number"), None);
		assert_eq!(mapped.get_str(2, "feats.Number"), None);
	}

	#[test]
	fn matches_non_streaming_path() {
		let dir = tempfile::tempdir().unwrap();
		let streaming_out = dir.path().join("streaming.bin");
		let direct_out = dir.path().join("direct.bin");
		let temp = dir.path().join("tmp");

		let words = &["le", "chat", "noir", "dort"];
		let pos_tags = &["DET", "NOUN", "ADJ", "VERB"];

		let fwd = make_forward(words, pos_tags);
		montre_index::write_flat_forward(&fwd, &direct_out).unwrap();

		let fwd2 = make_forward(words, pos_tags);
		let mut writer = StreamingForwardWriter::new(&temp).unwrap();
		writer.append_from(fwd2, 0).unwrap();
		writer.finalize(&streaming_out).unwrap();

		let direct = MappedForward::open(&direct_out).unwrap();
		let streamed = MappedForward::open(&streaming_out).unwrap();

		assert_eq!(direct.token_count(), streamed.token_count());
		for i in 0..4u64 {
			assert_eq!(direct.get_str(i, "word"), streamed.get_str(i, "word"));
			assert_eq!(direct.get_str(i, "pos"), streamed.get_str(i, "pos"));
		}
	}
}
