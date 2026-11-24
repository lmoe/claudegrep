use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::embed::download::{self, Wheel};
use crate::embed::{Acceleration, Device, Gpu, cuda};

const ORT_VERSION: &str = "1.32.0";

/// The variable the ONNX Runtime loader takes the library's path from.
const DYLIB_PATH: &str = "ORT_DYLIB_PATH";

/// Makes the binding fail when the CUDA provider cannot start. Without it ONNX Runtime drops the provider without a
/// word, and the session runs on one CPU thread.
const FORCE_GPU: &str = "NEXT_PLAID_FORCE_GPU";

/// Makes the binding run a CUDA session on the CPU without saying so.
const FORCE_CPU: &str = "NEXT_PLAID_FORCE_CPU";

/// Where the binding keeps the models `CoreML` compiles.
const COREML_CACHE: &str = "NEXT_PLAID_COREML_CACHE_DIR";

/// The `onnxruntime` wheels are all for one Python version. Every wheel holds the library next to its Python module.
const LINUX_X86_64: Wheel = Wheel {
    path: "92/74/1f7c51522822e93aa91f633ebcd75688e2deebeca5901b2e6da8a8c5ad7c/onnxruntime-1.32.0-cp313-cp313-manylinux_2_28_x86_64.whl",
    sha256: "02ea12a28c069ea8510093d95a7f6ff8126bfd9fc5c0d3381c03b80fb2b424ba",
    files: &[("onnxruntime/capi/libonnxruntime.so.1.32.0", LINUX_LIBRARY)],
};
const LINUX_AARCH64: Wheel = Wheel {
    path: "96/c7/26be6be3ebe42597a521645a7bb18701ce0716af87af0cd2c9281d2e84a5/onnxruntime-1.32.0-cp313-cp313-manylinux_2_28_aarch64.whl",
    sha256: "54d02baff2df38777b4c3cc7af326406fe8b8a48f54720f4c446d2d327920a79",
    files: &[("onnxruntime/capi/libonnxruntime.so.1.32.0", LINUX_LIBRARY)],
};
const MACOS_AARCH64: Wheel = Wheel {
    path: "f1/0c/97eb4ac42266545d7630e8231f8cc969f5c3f144d2ee6cb568ef45f81360/onnxruntime-1.32.0-cp313-cp313-macosx_14_0_arm64.whl",
    sha256: "27bf5dad46355a8a435c9c4e1556464d99ca834ea44e0f452909712068b8821b",
    files: &[("onnxruntime/capi/libonnxruntime.1.32.0.dylib", MACOS_LIBRARY)],
};

/// The GPU build for Linux on `x86_64`, made for CUDA 13. ONNX Runtime loads the provider libraries from the directory
/// its own library is in.
const LINUX_X86_64_CUDA: Wheel = Wheel {
    path: "a9/b6/759cc7838e5640d30e365e3b8466987172526faabf1779f0bf776f566b1e/onnxruntime_gpu-1.32.0-cp313-cp313-manylinux_2_28_x86_64.whl",
    sha256: "d3cec70e7a7dcfb085e0e6458210211541a33ffbf89c6a6e4e3d6adf7262cd6d",
    files: &[
        ("onnxruntime/capi/libonnxruntime.so.1.32.0", LINUX_LIBRARY),
        ("onnxruntime/capi/libonnxruntime_providers_shared.so", "libonnxruntime_providers_shared.so"),
        ("onnxruntime/capi/libonnxruntime_providers_cuda.so", "libonnxruntime_providers_cuda.so"),
    ],
};

const LINUX_LIBRARY: &str = "libonnxruntime.so";
const MACOS_LIBRARY: &str = "libonnxruntime.dylib";

/// Chooses the ONNX Runtime library this process loads, downloading it once, and says whether documents can be embedded
/// on a GPU. A library named by `ORT_DYLIB_PATH` is used as it is.
pub fn prepare_runtime(device: Device) -> Result<Acceleration> {
    let cache = cache_dir()?;
    let given = std::env::var_os(DYLIB_PATH).is_some();
    let mut acceleration = wanted(device, Gpu::detect(!given));
    let library = match acceleration {
        _ if given => None,
        Acceleration::Gpu(Gpu::Cuda { .. }) => match cuda_runtime(&cache) {
            Ok(library) => Some(library),
            Err(error) => {
                acceleration = Acceleration::Unavailable(format!("{error:#}"));
                Some(cpu_runtime(&cache)?)
            }
        },
        _ => Some(cpu_runtime(&cache)?),
    };
    // SAFETY: `set_var` is sound only while no other thread reads or writes the environment. The process is still
    // single-threaded here: `app::Setup::open` calls this before it builds the rayon pool, and nothing before it
    // (parsing arguments, opening the store, loading the settings, listing the project directories) starts a thread.
    unsafe {
        if let Some(library) = library {
            std::env::set_var(DYLIB_PATH, library);
        }
        match acceleration {
            Acceleration::Gpu(Gpu::Cuda { .. }) => std::env::set_var(FORCE_GPU, "1"),
            Acceleration::Gpu(Gpu::CoreMl) => std::env::set_var(COREML_CACHE, cache.join("coreml")),
            Acceleration::None | Acceleration::Unavailable(_) => {}
        }
    }
    Ok(acceleration)
}

/// What the setting makes of the GPU the machine has.
fn wanted(device: Device, detected: Option<Gpu>) -> Acceleration {
    match (device, detected) {
        (Device::Cpu, _) | (Device::Auto, None) => Acceleration::None,
        (Device::Auto | Device::Gpu, Some(gpu)) => Acceleration::Gpu(gpu),
        (Device::Gpu, None) => Acceleration::Unavailable("this machine has none that claudegrep can use".to_owned()),
    }
}

impl Gpu {
    /// The GPU of this machine: an NVIDIA card on Linux, Apple's own on a Mac with Apple Silicon. `libraries` says
    /// whether claudegrep brings CUDA and cuDNN along.
    fn detect(libraries: bool) -> Option<Self> {
        if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
            (std::env::var_os(FORCE_CPU).is_none() && cuda::has_driver()).then_some(Self::Cuda { libraries })
        } else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
            Some(Self::CoreMl)
        } else {
            None
        }
    }
}

fn cpu_runtime(cache: &Path) -> Result<PathBuf> {
    let (wheel, library) = release_for(std::env::consts::OS, std::env::consts::ARCH)?;
    let dir = cache.join("onnxruntime").join(ORT_VERSION);
    Ok(download::cached(&dir, &format!("ONNX Runtime {ORT_VERSION} (25 MiB)"), std::slice::from_ref(wheel))?.join(library))
}

fn cuda_runtime(cache: &Path) -> Result<PathBuf> {
    let dir = cache.join("onnxruntime-cuda").join(ORT_VERSION);
    let what = format!("ONNX Runtime {ORT_VERSION} for CUDA (240 MiB)");
    Ok(download::cached(&dir, &what, &[LINUX_X86_64_CUDA])?.join(LINUX_LIBRARY))
}

/// The wheel for `os` and `arch`, as `std::env::consts` names them, and the name its library is cached under.
fn release_for(os: &str, arch: &str) -> Result<(&'static Wheel, &'static str)> {
    Ok(match (os, arch) {
        ("linux", "x86_64") => (&LINUX_X86_64, LINUX_LIBRARY),
        ("linux", "aarch64") => (&LINUX_AARCH64, LINUX_LIBRARY),
        ("macos", "aarch64") => (&MACOS_AARCH64, MACOS_LIBRARY),
        (os, arch) => bail!("no ONNX Runtime download for {os}/{arch}; install it and set ORT_DYLIB_PATH"),
    })
}

pub fn cache_dir() -> Result<PathBuf> {
    Ok(dirs::cache_dir().context("no cache directory")?.join("claudegrep"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_supported_platform_has_a_release() {
        let table = [
            (("linux", "x86_64"), "manylinux_2_28_x86_64.whl", "onnxruntime/capi/libonnxruntime.so.1.32.0", "libonnxruntime.so"),
            (("linux", "aarch64"), "manylinux_2_28_aarch64.whl", "onnxruntime/capi/libonnxruntime.so.1.32.0", "libonnxruntime.so"),
            (("macos", "aarch64"), "macosx_14_0_arm64.whl", "onnxruntime/capi/libonnxruntime.1.32.0.dylib", "libonnxruntime.dylib"),
        ];
        for ((os, arch), platform, wanted, library) in table {
            let (wheel, cached) = release_for(os, arch).unwrap();
            assert!(wheel.path.ends_with(&format!("/onnxruntime-{ORT_VERSION}-cp313-cp313-{platform}")), "{os}/{arch}: {}", wheel.path);
            assert_eq!((wheel.files, cached), ([(wanted, library)].as_slice(), library), "{os}/{arch}");
            assert_eq!(wheel.sha256.len(), 64, "{os}/{arch}");
        }
    }

    #[test]
    fn the_cuda_build_brings_its_provider_libraries() {
        let cached: Vec<&str> = LINUX_X86_64_CUDA.files.iter().map(|&(_, name)| name).collect();
        assert_eq!(cached, ["libonnxruntime.so", "libonnxruntime_providers_shared.so", "libonnxruntime_providers_cuda.so"]);
        assert!(LINUX_X86_64_CUDA.path.ends_with(&format!("/onnxruntime_gpu-{ORT_VERSION}-cp313-cp313-manylinux_2_28_x86_64.whl")));
    }

    #[test]
    fn an_unsupported_platform_asks_for_ort_dylib_path() {
        for (os, arch) in [("windows", "x86_64"), ("linux", "riscv64"), ("macos", "x86_64")] {
            let error = release_for(os, arch).unwrap_err().to_string();
            assert_eq!(error, format!("no ONNX Runtime download for {os}/{arch}; install it and set ORT_DYLIB_PATH"));
        }
    }

    #[test]
    fn the_setting_decides_whether_a_detected_gpu_is_used() {
        let cuda = Gpu::Cuda { libraries: true };
        assert_eq!(wanted(Device::Auto, Some(cuda)), Acceleration::Gpu(cuda));
        assert_eq!(wanted(Device::Gpu, Some(Gpu::CoreMl)), Acceleration::Gpu(Gpu::CoreMl));
        assert_eq!(wanted(Device::Cpu, Some(cuda)), Acceleration::None);
    }

    #[test]
    fn without_a_gpu_only_asking_for_one_is_worth_a_warning() {
        assert_eq!(wanted(Device::Auto, None), Acceleration::None);
        assert_eq!(wanted(Device::Cpu, None), Acceleration::None);
        assert_eq!(wanted(Device::Gpu, None), Acceleration::Unavailable("this machine has none that claudegrep can use".to_owned()));
    }
}
