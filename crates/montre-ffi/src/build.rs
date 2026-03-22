use std::os::raw::c_char;

use crate::error::{set_error, clear_error};
use crate::strings::borrow_cstr;

/// Build a single-component corpus from a directory of CoNLL-U files.
/// Returns 1 on success, 0 on failure (check montre_last_error).
#[no_mangle]
pub unsafe extern "C" fn montre_build_directory(
	name: *const c_char,
	input_dir: *const c_char,
	output_dir: *const c_char,
	decompose_feats: i32,
	strict: i32,
) -> i32 {
	clear_error();

	let Some(name_str) = borrow_cstr(name) else {
		set_error("null corpus name".into());
		return 0;
	};
	let Some(input_str) = borrow_cstr(input_dir) else {
		set_error("null input directory".into());
		return 0;
	};
	let Some(output_str) = borrow_cstr(output_dir) else {
		set_error("null output directory".into());
		return 0;
	};

	let input = std::path::Path::new(input_str);
	let output = std::path::Path::new(output_str);

	let builder = match montre_build::builder::CorpusBuilder::from_directory(
		name_str,
		input,
		decompose_feats != 0,
		strict != 0,
	) {
		Ok(b) => b,
		Err(e) => {
			set_error(e.to_string());
			return 0;
		}
	};

	match builder.build(output) {
		Ok(()) => 1,
		Err(e) => {
			set_error(e.to_string());
			0
		}
	}
}

/// Build a multi-component corpus from a TOML manifest.
/// If decompose_feats is nonzero, overrides the manifest setting.
/// Returns 1 on success, 0 on failure (check montre_last_error).
#[no_mangle]
pub unsafe extern "C" fn montre_build_manifest(
	manifest_path: *const c_char,
	output_dir: *const c_char,
	decompose_feats: i32,
	strict: i32,
) -> i32 {
	clear_error();

	let Some(manifest_str) = borrow_cstr(manifest_path) else {
		set_error("null manifest path".into());
		return 0;
	};
	let Some(output_str) = borrow_cstr(output_dir) else {
		set_error("null output directory".into());
		return 0;
	};

	let output = std::path::Path::new(output_str);

	let mut builder = match montre_build::MultiCorpusBuilder::from_manifest(manifest_str) {
		Ok(b) => b,
		Err(e) => {
			set_error(e.to_string());
			return 0;
		}
	};

	builder = builder.strict(strict != 0);
	if decompose_feats != 0 {
		builder = builder.decompose_feats(true);
	}

	match builder.build(output) {
		Ok(()) => 1,
		Err(e) => {
			set_error(e.to_string());
			0
		}
	}
}
