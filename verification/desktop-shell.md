# Desktop shell verification

The desktop target is the real Tauri 2 application in `src-tauri`; the browser preview is not accepted as shell evidence.

## Required checks

```sh
npm ci
npm run build
cargo test -p rex-providers
cargo check -p rex-harness
```

Linux needs GTK 3, WebKitGTK 4.1, librsvg 2 and their development metadata. On a normal build host, install the packages listed by Tauri. In a locked-down host, extract those distribution packages to a user-owned sysroot and run:

```sh
REX_TAURI_SYSROOT=/path/to/sysroot scripts/tauri-linux-env.sh cargo check -p rex-harness
```

A passing shell check must compile `src-tauri/src/main.rs`, including the registered provider commands. Provider bridge behavior is separately covered by `rex-providers` tests, including credential-file permissions, real HTTP status mapping, pagination, catalog normalization and the recorded live Gemini ListModels response.

Do not call the desktop shell complete from `npm run dev` or `npm run preview`; those are browser surfaces and do not prove native compilation or embedded command registration.
