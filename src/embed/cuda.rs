//! CUDA for ONNX Runtime's GPU build on Linux: whether the machine has an NVIDIA driver, and the libraries the
//! runtime's CUDA provider needs, taken from NVIDIA's wheels on `PyPI` when the machine does not have them.

use std::path::Path;

use anyhow::Result;

/// The driver's own CUDA library. A machine with a working NVIDIA driver has it, with or without a CUDA toolkit.
const DRIVER: &str = "libcuda.so.1";

pub fn has_driver() -> bool {
    imp::open(DRIVER.as_ref()).is_ok()
}

/// Loads CUDA 13 and cuDNN 9 into the process, where the provider finds them by name once ONNX Runtime loads it. Each
/// is the machine's own when it has it, and else comes from a directory under `cache`, which is downloaded once.
pub fn load_libraries(cache: &Path) -> Result<()> {
    imp::load_libraries(cache)
}

#[cfg(unix)]
mod imp {
    use std::ffi::OsStr;
    use std::path::Path;

    use anyhow::{Context, Result};
    use libloading::os::unix::{Library, RTLD_GLOBAL, RTLD_LAZY};

    use crate::embed::download::{self, Wheel};

    /// A set of libraries that is installed as a whole, with where to get it when the machine has none.
    struct Libraries {
        name: &'static str,
        /// What the machine's own installation is known by.
        installed: &'static [&'static str],
        /// The files to load from the cached directory, each before the ones that load it.
        files: &'static [&'static str],
        /// The cached directory, under the cache.
        dir: &'static str,
        size: &'static str,
        wheels: &'static [Wheel],
    }

    /// The provider links against these four.
    const CUDA: Libraries = Libraries {
        name: "CUDA 13",
        installed: &["libcudart.so.13", "libcublasLt.so.13", "libcublas.so.13", "libcurand.so.10"],
        files: &["libcudart.so.13", "libcublasLt.so.13", "libcublas.so.13", "libcurand.so.10"],
        dir: "cuda/13",
        size: "500 MiB",
        wheels: &[
            Wheel {
                path: "0b/7c/14b8f5518f8951e9dca57ea80f44f8ea8bcc8671812e6cc38009f3205e57/\
                       nvidia_cuda_runtime-13.5.37-py3-none-manylinux2014_x86_64.manylinux_2_17_x86_64.whl",
                sha256: "df2b79a8852f10d1fef8c848489c49eebb5754051bf870202a862831412b179d",
                files: &[("nvidia/cu13/lib/libcudart.so.13", "libcudart.so.13")],
            },
            Wheel {
                path: "27/11/db2c1a4321c2145928f87e0f647104d3a908f78d4d582908759ce81a0c17/\
                       nvidia_cublas-13.10.1.6-py3-none-manylinux1_x86_64.manylinux_2_5_x86_64.whl",
                sha256: "0f26cf49a81377fd96f519e28fd56c5f638a2581f303d531f60e633fb222e1ea",
                files: &[
                    ("nvidia/cu13/lib/libcublasLt.so.13", "libcublasLt.so.13"),
                    ("nvidia/cu13/lib/libcublas.so.13", "libcublas.so.13"),
                ],
            },
            Wheel {
                path: "12/63/636e5dc9afb2959bfc0042a357d5567358df829b66c96d9fc36d77dc0596/\
                       nvidia_curand-10.4.5.25-py3-none-manylinux_2_27_x86_64.whl",
                sha256: "22bb226a857e5888fe6b54ae465fc4371296bcff13a35f79e19691bcf454f529",
                files: &[("nvidia/cu13/lib/libcurand.so.10", "libcurand.so.10")],
            },
        ],
    };

    /// The provider loads cuDNN when a model first needs it, which the embedding models do for their last step. cuDNN
    /// is a front library that loads its parts by name, and of those the models need the two cached here.
    const CUDNN: Libraries = Libraries {
        name: "cuDNN 9",
        installed: &["libcudnn.so.9"],
        files: &["libcudnn_graph.so.9", "libcudnn_ops.so.9", "libcudnn.so.9"],
        dir: "cudnn/9",
        size: "490 MiB",
        wheels: &[Wheel {
            path: "ef/6f/94535c450d44a68ef0b43d7a5e54c0e5ecda2ca3db7daec5d54c8d94e0d8/nvidia_cudnn_cu13-9.28.1.4-py3-none-manylinux_2_27_x86_64.whl",
            sha256: "a54f205b1db1f2895d99f398754db1369816f0569dd459731f925635cf7b65ec",
            files: &[
                ("nvidia/cudnn/lib/libcudnn_graph.so.9", "libcudnn_graph.so.9"),
                ("nvidia/cudnn/lib/libcudnn_ops.so.9", "libcudnn_ops.so.9"),
                ("nvidia/cudnn/lib/libcudnn.so.9", "libcudnn.so.9"),
            ],
        }],
    };

    pub fn open(library: &OsStr) -> Result<Library> {
        // SAFETY: loading a library runs its initializers. These are the NVIDIA driver's and CUDA's own libraries,
        // from the system or from wheels checked against the hashes above, and their initializers expect nothing
        // of the process.
        unsafe { Library::open(Some(library), RTLD_LAZY | RTLD_GLOBAL) }.with_context(|| format!("loading {}", library.display()))
    }

    pub fn load_libraries(cache: &Path) -> Result<()> {
        [CUDA, CUDNN].iter().try_for_each(|libraries| libraries.load(cache))
    }

    impl Libraries {
        fn load(&self, cache: &Path) -> Result<()> {
            if let Ok(installed) = self.installed.iter().map(|name| open(name.as_ref())).collect::<Result<Vec<_>>>() {
                installed.into_iter().for_each(std::mem::forget);
                return Ok(());
            }
            let what = format!("{} from NVIDIA ({})", self.name, self.size);
            let dir = download::cached(&cache.join(self.dir), &what, self.wheels)?;
            for name in self.files {
                std::mem::forget(open(dir.join(name).as_os_str())?);
            }
            Ok(())
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn every_file_that_is_loaded_comes_from_a_wheel() {
            for libraries in [CUDA, CUDNN] {
                let mut cached: Vec<&str> = libraries.wheels.iter().flat_map(|wheel| wheel.files).map(|&(_, name)| name).collect();
                let mut loaded = libraries.files.to_vec();
                cached.sort_unstable();
                loaded.sort_unstable();
                assert_eq!(cached, loaded, "{}", libraries.name);
            }
        }

        #[test]
        fn a_library_that_does_not_exist_is_an_error_that_names_it() {
            let error = format!("{:#}", open("libclaudegrep-no-such.so.9".as_ref()).unwrap_err());
            assert!(error.starts_with("loading libclaudegrep-no-such.so.9: "), "{error}");
        }
    }
}

#[cfg(not(unix))]
mod imp {
    use std::ffi::OsStr;
    use std::path::Path;

    use anyhow::{Result, bail};

    pub fn open(_: &OsStr) -> Result<()> {
        bail!("no CUDA on this platform")
    }

    pub fn load_libraries(_: &Path) -> Result<()> {
        bail!("no CUDA on this platform")
    }
}
