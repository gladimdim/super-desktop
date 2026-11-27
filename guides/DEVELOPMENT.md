[← Back to README](../README.md)

# Development, Signing & Releases

This guide covers building SUPER DESKTOP locally, running the isolated test suite, managing the Pacman repository signing key, cutting releases, and building signed multi-arch (`x86_64` and `aarch64`) packages locally or in GitHub Actions.

---

## Building from a local clone

Clone the repository and run `./install.sh` inside the checkout. The installer leaves your local git branch and uncommitted edits alone and builds release binaries (`super-desktop`, `super-desktop-client`, and `sd-qml-host`) plus sets up `~/.local/bin/super-desktop`:

```bash
git clone https://github.com/gladimdim/super-desktop.git
cd super-desktop
./install.sh
```

After editing code locally:

```bash
./rebuild.sh
```

`rebuild.sh` builds both Rust binaries and `sd-qml-host` (`scripts/build-qml-host.sh`), installs updated files, and restarts the running overlay without interrupting your background `tmux` harness sessions.

To build only specific components during iteration:

```bash
cargo build --release --locked --bins
SD_QML_HOST_REQUIRED=1 scripts/build-qml-host.sh release
```

---

## Running the test suite

Run the full suite in a detached session with no controlling TTY and without desktop display access:

```bash
setsid -w env -u SD_GTK_TESTS_ON_DESKTOP cargo test --locked </dev/null
```

Notes on test isolation:
- `setsid -w ... </dev/null` detaches the controlling terminal so `shell_title` tests checking foreground behavior without a controlling TTY behave reliably.
- Keep `SD_GTK_TESTS_ON_DESKTOP` unset. GTK tests spawn isolated child processes against private `gtk4-broadwayd` instances (`127.0.0.1`) so tests never open windows on your desktop. Five large-screen GTK wrapper tests skip their assertions on Broadway's fixed 1024×768 screen and are only run interactively when explicitly requested with `SD_GTK_TESTS_ON_DESKTOP=1`.

Targeted regression and packaging checks:

```bash
cargo test toolbar_
cargo test card_title_
cargo test test_isolation
cargo test plugins_

python3 -m unittest discover -s tests -p 'test_arch_packaging.py'
bash tests/install_packages.sh
bash tests/install_binary_repo.sh
bash tests/pacman_repo_smoke.sh
```

---

## Pacman GPG signing key lifecycle

Prebuilt `super-desktop-bin` packages and the static Pacman databases hosted at `https://superdesktop.dmytrogladkyi.com/repo/$arch` are signed with a dedicated Ed25519 key.

### Committed vs secret material

| Artifact | Location | Purpose |
| --- | --- | --- |
| Public signing key | [`packaging/arch/super-desktop.gpg`](../packaging/arch/super-desktop.gpg) | Published at `/repo/super-desktop.gpg` for `pacman-key` |
| Pinned fingerprint | [`packaging/arch/super-desktop.gpg.fingerprint`](../packaging/arch/super-desktop.gpg.fingerprint) | Checked against `REPO_KEY_FINGERPRINT` in `install.sh` and CI/local signing |
| Private key | GitHub secret `PACMAN_GPG_PRIVATE_KEY` + offline backup outside git (`~/Documents/super-desktop-signing-keys/private-key.asc` / encrypted NAS) | Detach-signs `.pkg.tar.zst` and `super-desktop.{db,files}` |

> **Never commit `private-key.asc` or paste private key blocks into tracked files or logs.**

### Initial key setup (already done for this repo)

`scripts/init-pacman-key.sh` automates one-time key creation (5-year expiry), updates `packaging/arch/super-desktop.gpg`, `packaging/arch/super-desktop.gpg.fingerprint`, and `install.sh`, saves a `0700`/`0600` backup outside the repository, and uploads `PACMAN_GPG_PRIVATE_KEY` via `gh secret set`:

```bash
bash scripts/init-pacman-key.sh
```

### Extending expiry or rotating keys

Before the 5-year key expiration:
1. **Extend expiry** (keeps existing user trust intact): load your backed-up `private-key.asc` into an ephemeral `GNUPGHOME`, extend the key expiry with `gpg --quick-set-expire <FPR> 5y`, re-export `packaging/arch/super-desktop.gpg`, update the offline backup, and re-upload `gh secret set PACMAN_GPG_PRIVATE_KEY < private-key.asc`.
2. **Full rotation** (new key pair): run `SUPER_DESKTOP_KEY_BACKUP_DIR=/secure/path bash scripts/init-pacman-key.sh --force`, commit the updated public key, fingerprint, and `install.sh`, and rebuild/deploy the Pacman repository with the new key.

---

## Cutting a release (`vX.Y.Z`)

Enable the repository pre-commit hook once per clone so version mismatches between `Cargo.toml` and `Cargo.lock` are caught before committing:

```bash
git config core.hooksPath .githooks
```

Release workflow:

1. Start from a clean, pushed `master` branch with all tests passing.
2. Update `version = "X.Y.Z"` in both `Cargo.toml` and `Cargo.lock` in one commit (`Release X.Y.Z`) and push `master`.
3. Create and push the annotated git tag (never move, delete, or re-tag a published tag; fix mistakes with a new patch version):

```bash
git tag -a vX.Y.Z -m "SUPER DESKTOP X.Y.Z" <release-commit>
git push origin vX.Y.Z
gh release create vX.Y.Z --title "SUPER DESKTOP X.Y.Z" --notes-file <notes-file>
```

4. Add the new version row to the protocol compatibility table in [guides/INSTALL.md](INSTALL.md#installing-a-specific-version) and update current desktop version references in `docs/index.html` and `docs/llms.txt` in a follow-up commit on `master`.

---

## Building and publishing signed `x86_64` and `aarch64` packages

Both architectures are built inside clean **Omarchy `stable`** containers (`packaging/arch/builder/Dockerfile`) to avoid rolling Arch library mismatches (`openssl` vs `openssl-3`, newer `glibc` floors), tested inside the container (including `package_tests::packaged_desktop_lifecycle`), packaged as `super-desktop-bin`, and GPG-signed into static symlink-free Pacman repositories (`scripts/build-pacman-repo.sh`).

### Option A — GitHub Actions (automated, native runners)

`.github/workflows/arch-release.yml` runs automatically on published non-prerelease GitHub Releases (or manually via workflow dispatch):

```bash
gh workflow run arch-release.yml --ref master -f tag=vX.Y.Z -f publish=true
```

What CI runs:
- `build`: builds cleanly on native `ubuntu-latest` (`x86_64`) and native `ubuntu-24.04-arm` (`aarch64`), runs the full test suite + `package_tests::packaged_desktop_lifecycle`, and produces `super-desktop-bin-X.Y.Z-1-<arch>.pkg.tar.zst`, `super-desktop-X.Y.Z-linux-<arch>.tar.gz`, and `super-desktop-X.Y.Z-source.tar.gz`.
- `build-repo`: imports `PACMAN_GPG_PRIVATE_KEY`, checks fingerprint equality against `packaging/arch/super-desktop.gpg.fingerprint`, runs `scripts/build-pacman-repo.sh` for both architectures, generates `SHA256SUMS`, and archives `pacman-repo-{x86_64,aarch64}.tar.gz`.
- `publish-release`: attaches all immutable assets (without `--clobber`) to GitHub Release `vX.Y.Z`.
- `deploy-pages`: triggers `.github/workflows/pages.yml` to publish `docs/` plus `/repo/{x86_64,aarch64}` to GitHub Pages.

> **Note:** On public repositories, GitHub-hosted standard runners (`ubuntu-latest` and `ubuntu-24.04-arm`) do not bill Actions minutes, and self-hosted runners should **not** be connected to public repositories because untrusted pull requests could execute arbitrary code locally.

### Option B — Local rootless Podman build, sign & upload (`scripts/release-local.sh`)

To build, test, GPG-sign, and upload multi-arch packages from your own Omarchy workstation (or verify a tag without consuming CI jobs):

**Host prerequisites:**

```bash
sudo pacman -S --needed podman pacman-contrib gnupg github-cli
# Required when cross-building aarch64 packages on an x86_64 host:
sudo pacman -S --needed qemu-user-static qemu-user-static-binfmt
sudo systemctl restart systemd-binfmt
```

**Dry-run local build & sign (no GitHub upload):**

```bash
scripts/release-local.sh \
  --tag vX.Y.Z \
  --arch x86_64 \
  --key-file ~/Documents/super-desktop-signing-keys/private-key.asc
```

(`--key-file` also defaults automatically to `$SUPER_DESKTOP_SIGNING_KEY`, `~/Documents/super-desktop-signing-keys/private-key.asc`, or `~/.config/super-desktop-signing/private-key.asc`. Use `--no-sign` for unsigned local smoke builds.)

**Full multi-arch build, GPG sign, GitHub Release upload & Pages publish:**

```bash
scripts/release-local.sh --tag vX.Y.Z --arch all --upload
```

What `scripts/release-local.sh` does:
1. Clones exact immutable annotated tag `vX.Y.Z` into a temporary workspace and checks `Cargo.toml` matches `X.Y.Z`.
2. Builds `packaging/arch/builder` per target platform (`linux/amd64`, `linux/arm64`) with rootless `podman` (or `--engine docker`).
3. Runs `scripts/build-arch-release.sh` and `cargo test --locked package_tests::packaged_desktop_lifecycle -- --ignored` as unprivileged `builder` inside the container, then restores container `/out` ownership back to UID 0 so artifacts belong cleanly to your host user.
4. Imports your signing key into an ephemeral `mktemp -d` `GNUPGHOME`, verifies the 40-character fingerprint matches `packaging/arch/super-desktop.gpg.fingerprint`, runs `scripts/build-pacman-repo.sh` for each arch, and writes `SHA256SUMS`.
5. With `--upload`, checks for existing assets (never `--clobber`s), uploads all assets to GitHub Release `vX.Y.Z`, and dispatches `pages.yml` on `master` once both `pacman-repo-x86_64.tar.gz` and `pacman-repo-aarch64.tar.gz` exist on the release.

---

## Verifying a published release & Pacman repository

Verify release checksums, detached signatures, and live GitHub Pages repository files:

```bash
# 1. Check live public key & repository databases
curl -fsSL https://superdesktop.dmytrogladkyi.com/repo/super-desktop.gpg | gpg --show-keys --with-fingerprint
for arch in x86_64 aarch64; do
  curl -fsSI "https://superdesktop.dmytrogladkyi.com/repo/${arch}/super-desktop.db"
  curl -fsSI "https://superdesktop.dmytrogladkyi.com/repo/${arch}/super-desktop.db.sig"
done

# 2. Verify downloaded binary archive + .sig + SHA256SUMS offline
tmp="$(mktemp -d)" && chmod 700 "$tmp"
GNUPGHOME="$tmp" gpg --batch --import packaging/arch/super-desktop.gpg
GNUPGHOME="$tmp" gpg --batch --verify super-desktop-bin-X.Y.Z-1-x86_64.pkg.tar.zst.sig super-desktop-bin-X.Y.Z-1-x86_64.pkg.tar.zst
sha256sum -c SHA256SUMS --ignore-missing
rm -rf "$tmp"
```
