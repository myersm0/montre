use pyo3::prelude::*;

#[pyclass]
struct Corpus {
	inner: montre_index::Corpus,
}

#[pymethods]
impl Corpus {
	fn name(&self) -> &str {
		self.inner.name()
	}

	fn token_count(&self) -> u64 {
		self.inner.token_count()
	}

	fn layers(&self) -> Vec<String> {
		self.inner.layers().to_vec()
	}

	fn query(&self, query: &str) -> PyResult<Results> {
		let parsed = montre_query::parse(query)
			.map_err(|e| PyErr::new::<pyo3::exceptions::PyValueError, _>(e.to_string()))?;

		let plan = montre_query::planner::plan(&parsed)
			.map_err(|e| PyErr::new::<pyo3::exceptions::PyRuntimeError, _>(e.to_string()))?;

		let results = montre_query::executor::execute(&plan, &self.inner)
			.map_err(|e| PyErr::new::<pyo3::exceptions::PyRuntimeError, _>(e.to_string()))?;

		Ok(Results { inner: results })
	}
}

#[pyclass]
struct Results {
	inner: montre_query::Results,
}

#[pymethods]
impl Results {
	fn __len__(&self) -> usize {
		self.inner.len()
	}

	fn __iter__(slf: PyRef<'_, Self>) -> PyResult<Py<ResultsIter>> {
		let hits: Vec<_> = slf.inner.hits().to_vec();
		Py::new(slf.py(), ResultsIter { hits, index: 0 })
	}
}

#[pyclass]
struct ResultsIter {
	hits: Vec<montre_query::executor::Hit>,
	index: usize,
}

#[pymethods]
impl ResultsIter {
	fn __next__(&mut self) -> Option<Hit> {
		if self.index < self.hits.len() {
			let hit = &self.hits[self.index];
			self.index += 1;
			Some(Hit {
				start: hit.span.start,
				end: hit.span.end,
			})
		} else {
			None
		}
	}
}

#[pyclass]
#[derive(Clone)]
struct Hit {
	#[pyo3(get)]
	start: u64,
	#[pyo3(get)]
	end: u64,
}

#[pymethods]
impl Hit {
	fn __repr__(&self) -> String {
		format!("Hit({}, {})", self.start, self.end)
	}
}

#[pyfunction]
fn open(path: &str) -> PyResult<Corpus> {
	let inner = montre_index::open(path)
		.map_err(|e| PyErr::new::<pyo3::exceptions::PyIOError, _>(e.to_string()))?;
	Ok(Corpus { inner })
}

#[pymodule]
fn montre(m: &Bound<'_, PyModule>) -> PyResult<()> {
	m.add_function(wrap_pyfunction!(open, m)?)?;
	m.add_class::<Corpus>()?;
	m.add_class::<Results>()?;
	m.add_class::<Hit>()?;
	Ok(())
}
