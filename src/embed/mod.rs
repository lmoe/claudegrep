mod cuda;
mod download;
pub mod pipeline;
mod runtime;

use std::cell::RefCell;
use std::fmt::Display;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use ndarray::Array2;
use next_plaid_onnx::{Colbert, ExecutionProvider};
use strum::{EnumString, IntoStaticStr};

pub use runtime::prepare_runtime;

use crate::text::shorten;

pub const DEFAULT_MODEL: &str = "lightonai/LateOn-Code-edge";

const POOL_FACTOR: usize = 2;

/// The `device` setting: where documents are embedded. Queries are always encoded on the CPU, which has done one before
/// a GPU session is ready.
#[derive(Debug, Clone, Copy, PartialEq, Eq, EnumString, IntoStaticStr)]
#[strum(serialize_all = "lowercase")]
pub enum Device {
    /// The GPU when the machine has one. Once that has worked, `gpu` is stored.
    Auto,
    Cpu,
    Gpu,
}

/// A GPU documents can be embedded on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gpu {
    /// An NVIDIA card, through ONNX Runtime's CUDA provider. `libraries` says whether claudegrep brings CUDA and cuDNN:
    /// it does for the runtime it downloaded, and leaves a runtime the user supplied to find its own.
    Cuda { libraries: bool },
    /// Apple Silicon, through `CoreML`.
    CoreMl,
}

/// What the commands that embed know about the GPU before the first session exists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Acceleration {
    /// The setting asks for none, or it leaves the choice open and the machine has none.
    None,
    Gpu(Gpu),
    /// A GPU was to be used and cannot be, for this reason.
    Unavailable(String),
}

/// How many characters of a reason the line about the CPU shows. ONNX Runtime's errors run to a thousand.
const REASON_CHARS: usize = 300;

/// What ONNX Runtime writes between where in its source an error arose and what went wrong.
const ORT_ERROR: &str = "[ONNXRuntimeError] : ";

/// Says in one line that documents are embedded on the CPU although the GPU was to be used.
pub fn fall_back(reason: impl Display) {
    eprintln!("claudegrep: not using the GPU, embedding on the CPU: {}", brief(&reason.to_string()));
}

/// `reason` as one line of at most `REASON_CHARS` characters, starting at what went wrong.
fn brief(reason: &str) -> String {
    let what = reason.rsplit(ORT_ERROR).next().unwrap_or(reason);
    shorten(&what.split_whitespace().collect::<Vec<_>>().join(" "), REASON_CHARS)
}

/// Turns text into multi-vector embeddings, one row per token (or pooled group of tokens), scored with `MaxSim`.
pub trait Encoder {
    /// Names the encoder and its settings. Vectors made under another id are dropped and re-embedded.
    fn id(&self) -> &str;
    fn encode_documents(&self, texts: &[&str]) -> Result<Vec<Array2<f32>>>;
    fn encode_query(&self, query: &str) -> Result<Array2<f32>>;
}

impl<E: Encoder + ?Sized> Encoder for &E {
    fn id(&self) -> &str {
        (**self).id()
    }

    fn encode_documents(&self, texts: &[&str]) -> Result<Vec<Array2<f32>>> {
        (**self).encode_documents(texts)
    }

    fn encode_query(&self, query: &str) -> Result<Array2<f32>> {
        (**self).encode_query(query)
    }
}

pub struct Embedder {
    colbert: Colbert,
    id: String,
    dir: PathBuf,
    threads: usize,
}

impl Embedder {
    /// The model on the CPU.
    pub fn load(model: &str, threads: usize) -> Result<Self> {
        let dir = model_dir(model)?;
        let colbert = Colbert::builder(&dir)
            .with_execution_provider(ExecutionProvider::Cpu)
            .with_threads(threads)
            .build()
            .with_context(|| format!("loading {model} from {}", dir.display()))?;
        Ok(Self { colbert, id: id(model), dir, threads })
    }

    /// The same model on `gpu`. Its vectors are the CPU's, so both go by one id.
    pub fn on(&self, gpu: Gpu) -> Result<Self> {
        let provider = match gpu {
            Gpu::Cuda { libraries } => {
                if libraries {
                    cuda::load_libraries(&runtime::cache_dir()?)?;
                }
                ExecutionProvider::Cuda
            }
            Gpu::CoreMl => ExecutionProvider::CoreML,
        };
        let colbert = Colbert::builder(&self.dir).with_execution_provider(provider).with_threads(self.threads).build()?;
        Ok(Self { colbert, id: self.id.clone(), dir: self.dir.clone(), threads: self.threads })
    }
}

/// Names what the vectors depend on: the model and the pooling of document tokens.
fn id(model: &str) -> String {
    format!("{model}:pool{POOL_FACTOR}")
}

/// Embeds documents on a GPU for as long as that works, and on the CPU from then on, after saying so.
pub struct Documents<'a> {
    cpu: &'a dyn Encoder,
    gpu: RefCell<Option<Box<dyn Encoder + 'a>>>,
}

impl<'a> Documents<'a> {
    pub fn new(cpu: &'a dyn Encoder) -> Self {
        Self { cpu, gpu: RefCell::new(None) }
    }

    pub fn use_gpu(&self, gpu: impl Encoder + 'a) {
        self.gpu.replace(Some(Box::new(gpu)));
    }

    /// Whether documents are embedded on the GPU, which they are until it fails once.
    pub fn on_gpu(&self) -> bool {
        self.gpu.borrow().is_some()
    }
}

impl Encoder for Documents<'_> {
    fn id(&self) -> &str {
        self.cpu.id()
    }

    fn encode_documents(&self, texts: &[&str]) -> Result<Vec<Array2<f32>>> {
        if let Some(gpu) = self.gpu.take() {
            match gpu.encode_documents(texts) {
                Ok(embeddings) => {
                    self.gpu.replace(Some(gpu));
                    return Ok(embeddings);
                }
                Err(error) => fall_back(format_args!("{error:#}")),
            }
        }
        self.cpu.encode_documents(texts)
    }

    fn encode_query(&self, query: &str) -> Result<Array2<f32>> {
        self.cpu.encode_query(query)
    }
}

impl Encoder for Embedder {
    fn id(&self) -> &str {
        &self.id
    }

    fn encode_documents(&self, texts: &[&str]) -> Result<Vec<Array2<f32>>> {
        self.colbert.encode_documents(texts, Some(POOL_FACTOR))
    }

    fn encode_query(&self, query: &str) -> Result<Array2<f32>> {
        self.colbert.encode_queries(&[query])?.pop().context("the encoder returned no query embedding")
    }
}

fn model_dir(model: &str) -> Result<PathBuf> {
    local_dir(model).map_or_else(|| download(model), Ok)
}

fn local_dir(model: &str) -> Option<PathBuf> {
    let path = Path::new(model);
    path.is_dir().then(|| path.to_path_buf())
}

fn download(model: &str) -> Result<PathBuf> {
    let api = hf_hub::api::sync::ApiBuilder::new().with_progress(true).build()?;
    let repo = api.model(model.to_owned());
    let weights = repo.get("model.onnx").with_context(|| format!("{model} has no model.onnx; is it an ONNX ColBERT export?"))?;
    for file in ["tokenizer.json", "onnx_config.json"] {
        repo.get(file).with_context(|| format!("downloading {file} of {model}"))?;
    }
    for file in ["config.json", "config_sentence_transformers.json"] {
        let _ = repo.get(file);
    }
    weights.parent().map(Path::to_path_buf).context("model file has no parent directory")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::max_sim;
    use crate::test_support::encoder::FakeEncoder;

    #[test]
    fn devices_go_by_their_lowercase_names() {
        for (device, name) in [(Device::Auto, "auto"), (Device::Cpu, "cpu"), (Device::Gpu, "gpu")] {
            assert_eq!(<&str>::from(device), name);
            assert_eq!(name.parse(), Ok(device));
        }
        assert!("GPU".parse::<Device>().is_err());
    }

    #[test]
    fn a_reason_is_one_line_that_starts_at_what_went_wrong() {
        assert_eq!(brief("this machine has none"), "this machine has none");
        assert_eq!(
            brief("loading libcudart.so.13:\n  cannot open shared object file"),
            "loading libcudart.so.13: cannot open shared object file"
        );
        let ort = "Failed to configure CUDA: Error { msg: \"/src/cuda_info.cc:66 static Info FromOptions(const Options&) \
                   [ONNXRuntimeError] : 1 : FAIL : CUDA failure 100: no CUDA-capable device is detected\" }";
        assert_eq!(brief(ort), "1 : FAIL : CUDA failure 100: no CUDA-capable device is detected\" }");
        assert_eq!(crate::text::char_len(&brief(&"x".repeat(2000))), REASON_CHARS);
    }

    #[test]
    fn documents_are_embedded_on_the_gpu_while_it_works() {
        let (cpu, gpu) = (FakeEncoder::new(), FakeEncoder::new());
        let documents = Documents::new(&cpu);
        assert!(!documents.on_gpu());
        documents.encode_documents(&["on the cpu"]).unwrap();
        documents.use_gpu(&gpu);
        documents.encode_documents(&["on the gpu", "this too"]).unwrap();
        assert!(documents.on_gpu());
        assert_eq!((cpu.batches(), gpu.batches()), (vec![1], vec![2]));
    }

    #[test]
    fn a_gpu_that_fails_hands_the_batch_and_all_later_ones_to_the_cpu() {
        let (cpu, gpu) = (FakeEncoder::new(), FakeEncoder::new().failing_on("broken"));
        let documents = Documents::new(&cpu);
        documents.use_gpu(&gpu);
        assert_eq!(documents.encode_documents(&["fine"]).unwrap().len(), 1);
        assert_eq!(documents.encode_documents(&["broken", "batch"]).unwrap().len(), 2);
        assert!(!documents.on_gpu());
        documents.encode_documents(&["fine again"]).unwrap();
        assert_eq!((cpu.batches(), gpu.batches()), (vec![2, 1], vec![1, 2]));
    }

    #[test]
    fn queries_are_encoded_on_the_cpu() {
        let (cpu, gpu) = (FakeEncoder::new(), FakeEncoder::new());
        let documents = Documents::new(&cpu);
        documents.use_gpu(&gpu);
        assert_eq!(documents.encode_query("a query").unwrap(), cpu.encode_query("a query").unwrap());
        assert_eq!(documents.id(), cpu.id());
    }

    const DIM: usize = 48;

    #[test]
    fn the_id_names_model_and_pooling() {
        assert_eq!(id(DEFAULT_MODEL), "lightonai/LateOn-Code-edge:pool2");
        assert_eq!(id("/models/colbert"), "/models/colbert:pool2");
    }

    #[test]
    fn a_local_directory_is_used_as_it_is() {
        let dir = tempfile::TempDir::new().unwrap();
        let model = dir.path().to_str().unwrap();
        assert_eq!(local_dir(model), Some(dir.path().to_path_buf()));
        assert_eq!(model_dir(model).unwrap(), dir.path());
    }

    #[test]
    fn anything_else_is_a_hugging_face_repo() {
        let dir = tempfile::TempDir::new().unwrap();
        let file = dir.path().join("model.onnx");
        std::fs::write(&file, "").unwrap();
        assert_eq!(local_dir(file.to_str().unwrap()), None);
        assert_eq!(local_dir(dir.path().join("missing").to_str().unwrap()), None);
        assert_eq!(local_dir(DEFAULT_MODEL), None);
    }

    /// Loads the real default model from the local caches, downloading it and ONNX Runtime if they are missing.
    /// `prepare_runtime` may set `ORT_DYLIB_PATH`, which is only sound while no other thread touches the environment,
    /// so this is the only ignored test: `--ignored` runs it on its own.
    #[test]
    #[ignore = "needs the real model; run with `cargo test -- --ignored`"]
    fn the_default_model_ranks_a_related_sentence_above_an_unrelated_one() {
        prepare_runtime(Device::Cpu).unwrap();
        let embedder = Embedder::load(DEFAULT_MODEL, 2).unwrap();
        assert!(embedder.id().starts_with("lightonai/LateOn-Code-edge:"), "{}", embedder.id());

        let query = embedder.encode_query("why does the database migration fail").unwrap();
        let documents = embedder
            .encode_documents(&[
                "The sqlite schema migration failed because the new column was missing.",
                "We baked a chocolate cake for the birthday party.",
            ])
            .unwrap();
        assert_eq!(query.ncols(), DIM);
        for row in query.rows() {
            assert!((row.dot(&row) - 1.0).abs() < 1e-3, "a query row has unit length");
        }
        for document in &documents {
            assert_eq!(document.ncols(), DIM);
            assert!(document.nrows() > 0);
            // Pooling averages neighbouring token vectors, so document rows are at most unit length.
            assert!(document.rows().into_iter().all(|row| row.dot(&row) <= 1.0 + 1e-3));
        }
        let related = max_sim(query.view(), documents[0].view());
        let unrelated = max_sim(query.view(), documents[1].view());
        assert!(related > unrelated, "related {related} vs unrelated {unrelated}");
    }
}
