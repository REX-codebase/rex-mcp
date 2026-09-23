# Installing REX Harness (desktop app)

Download installers from the **Releases** page of
[REX-codebase/rex-harness](https://github.com/REX-codebase/rex-harness).
Releases are cut from tags like `v0.1.0` — the tag must match the `version`
in `src-tauri/tauri.conf.json`.

## Windows

- Download `REX-Harness_0.1.0_x64-setup.exe` (NSIS installer) or
  `REX-Harness_0.1.0_x64_en-US.msi`.
- The installers are **unsigned**, so Windows SmartScreen will warn on first
  run. Click **More info → Run anyway**. This is expected until the project
  has a code-signing certificate.

## macOS (Apple Silicon, arm64)

- Download `REX-Harness_0.1.0_aarch64.dmg`, open it, drag **REX Harness**
  into **Applications**.
- The app is **unsigned and not notarized**, so Gatekeeper will block the
  first launch ("Apple could not verify…"). Workaround: in Finder,
  **right-click (Ctrl-click) REX Harness → Open → Open**. You only need to do
  this once; afterwards it launches normally.
- Intel Macs: no Intel build is produced yet (CI builds on `macos-14`,
  arm64 only). It may run under Rosetta 2 — untested.

## Linux

- **AppImage**: download `REX Harness_0.1.0_amd64.AppImage`,
  `chmod +x` it, and run it. Works on most distros, no install needed.
- **deb** (Debian/Ubuntu): download `REX Harness_0.1.0_amd64.deb` and
  `sudo apt install ./"REX Harness_0.1.0_amd64.deb"` (pulls dependencies).
- System dependencies (already declared by the deb, needed for the AppImage
  on minimal systems): `libwebkit2gtk-4.1-0`, `libayatana-appindicator3-1`.

## Notes

- **No auto-update yet.** The updater plugin is disabled in the build
  because there are no signing keys. To get a new version, download the new
  installer from Releases and install over the old one.
- Your data (runs ledger, signing keys, config) lives outside the app
  bundle (see `$REX_STATE_DIR` / `~/.config/rex-harness`) and survives
  reinstalls.
