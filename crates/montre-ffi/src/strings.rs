use std::ffi::{CStr, CString};
use std::os::raw::c_char;
use std::ptr;

pub(crate) fn to_cstring(s: &str) -> *mut c_char {
	match CString::new(s) {
		Ok(c) => c.into_raw(),
		Err(_) => {
			let cleaned = s.replace('\0', "");
			CString::new(cleaned).unwrap().into_raw()
		}
	}
}

pub(crate) unsafe fn borrow_cstr<'a>(p: *const c_char) -> Option<&'a str> {
	if p.is_null() {
		return None;
	}
	CStr::from_ptr(p).to_str().ok()
}

pub(crate) unsafe fn export_array<T: Copy>(data: &[T], out_len: *mut u64) -> *mut T {
	if out_len.is_null() {
		return ptr::null_mut();
	}
	if data.is_empty() {
		*out_len = 0;
		return ptr::null_mut();
	}
	let layout = std::alloc::Layout::array::<T>(data.len()).unwrap();
	let array = std::alloc::alloc(layout) as *mut T;
	if array.is_null() {
		*out_len = 0;
		return ptr::null_mut();
	}
	ptr::copy_nonoverlapping(data.as_ptr(), array, data.len());
	*out_len = data.len() as u64;
	array
}

pub(crate) unsafe fn export_string_array(data: &[&str], out_len: *mut u64) -> *mut *mut c_char {
	if out_len.is_null() {
		return ptr::null_mut();
	}
	if data.is_empty() {
		*out_len = 0;
		return ptr::null_mut();
	}
	let layout = std::alloc::Layout::array::<*mut c_char>(data.len()).unwrap();
	let array = std::alloc::alloc(layout) as *mut *mut c_char;
	if array.is_null() {
		*out_len = 0;
		return ptr::null_mut();
	}
	for (i, s) in data.iter().enumerate() {
		*array.add(i) = to_cstring(s);
	}
	*out_len = data.len() as u64;
	array
}

#[no_mangle]
pub unsafe extern "C" fn montre_string_free(s: *mut c_char) {
	if !s.is_null() {
		drop(CString::from_raw(s));
	}
}

#[no_mangle]
pub unsafe extern "C" fn montre_string_array_free(array: *mut *mut c_char, len: u64) {
	if array.is_null() {
		return;
	}
	for i in 0..len as usize {
		let s = *array.add(i);
		if !s.is_null() {
			drop(CString::from_raw(s));
		}
	}
	let layout = std::alloc::Layout::array::<*mut c_char>(len as usize).unwrap();
	std::alloc::dealloc(array as *mut u8, layout);
}

#[no_mangle]
pub unsafe extern "C" fn montre_i32_array_free(array: *mut i32, len: u64) {
	if array.is_null() || len == 0 {
		return;
	}
	let layout = std::alloc::Layout::array::<i32>(len as usize).unwrap();
	std::alloc::dealloc(array as *mut u8, layout);
}

#[no_mangle]
pub unsafe extern "C" fn montre_u32_array_free(array: *mut u32, len: u64) {
	if array.is_null() || len == 0 {
		return;
	}
	let layout = std::alloc::Layout::array::<u32>(len as usize).unwrap();
	std::alloc::dealloc(array as *mut u8, layout);
}

#[no_mangle]
pub unsafe extern "C" fn montre_u64_array_free(array: *mut u64, len: u64) {
	if array.is_null() || len == 0 {
		return;
	}
	let layout = std::alloc::Layout::array::<u64>(len as usize).unwrap();
	std::alloc::dealloc(array as *mut u8, layout);
}
