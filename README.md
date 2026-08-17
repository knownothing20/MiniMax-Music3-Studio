# MiniMax Music3 Studio

Native Windows music studio for MiniMax Music3.

## Architecture

- **Desktop:** Tauri 2 + React UI.
- **Service:** Rust + Axum, one local process for library, queue and provider routing.
- **Local inference:** native C++/CUDA engine adapters with GGUF components.
- **Cloud inference:** OpenRouter through a capability-level provider registry.

The four capabilities are selected independently: music generation, speech-to-text, prompt
enhancement and cover art. This supports fully local, fully cloud and hybrid projects without
changing the project format.

## Status

The native foundation is being built. The initial Rust workspace exposes health, capability and
configuration endpoints. MiniMax Music3 native engine validation is the next delivery gate.

MiniMax Music3 weights are governed by their own community license. Commercial applications must
prominently display the MiniMax-Music3 name and implement the required safeguards.

## Native runtime builds

`scripts/build-release.ps1` always stages a universal CUDA runtime for the portable release; it
uses the architecture set pinned by the upstream `minimaxmusic.cpp` source. Do not publish a
targeted test binary as a portable release.

For local verification on an RTX 4090, build only its `sm_89` target into a disposable directory:

```powershell
.\scripts\build-minimax-runtime.ps1 -RuntimeBackend cuda -CudaArchitecture sm_89 -OutputDirectory "$env:TEMP\minimaxmusic-cpp-sm89"
```

`-CudaArchitecture native` asks CMake to target the GPU visible on the build machine. The helper
accepts only `universal`, `native`, and `sm_89`; targeted architectures require the CUDA backend,
an NVIDIA driver, `nvcc`, CMake, and a Visual Studio C++ build installation. It checks out the
pinned upstream commit and never edits upstream source files.
