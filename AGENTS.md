# MiniMax Music3 Studio guidelines

This is a native Windows desktop studio: Tauri 2 shell, Rust/Axum service and native C++/CUDA engines.

- Model and generation API work must follow `/home/leon/workspace/MODEL_GENERATION_API_INTEGRATION.md`.
- Do not add Python or Node.js to the runtime path.
- Model engines are adapters; no UI or server code may hardcode one model's behaviour.
- OmniBridge Project SDK through the project `model_port` is the only public entry for cloud model calls.
- This project owns business roles, product UI, domain prompts and business result persistence.
- OmniBridge owns Provider and Deployment configuration, credentials, Base URLs, upstream model IDs,
  Route candidate order and safe fallback policy. None of those belong in the project Profile or browser.
- Text, music, ASR and cover art are independent capabilities. A local capability being unavailable must
  not disable a ready cloud capability in another family.
- Existing OpenRouter and self-hosted/local compatibility paths for legacy cover and ASR workflows are
  migration boundaries only. Do not add new direct Provider calls; route new cloud capabilities through OmniBridge.
- The browser must never hold a Gateway key, Provider key, durable task token or private child task ID.
- Non-idempotent generation is submit-once. After `accepted`, a persisted child handle, or
  `submission_unknown`, recovery is GET-only and must never replay POST or switch Route candidates.
- Every local engine must expose install state, capability metadata, cancellation and progress before it reaches the UI.
- Model weights, API keys, media and caches are runtime user data; never commit them.
- Real paid model calls require explicit owner authorization; a health check or successful Profile resolve is not generation proof.
