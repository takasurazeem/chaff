# Packaging and signing

What builds, what is signed, and what is not. Written so nobody has to guess which of the
two they are looking at.

---

## What builds

```bash
pnpm tauri build --features custom-protocol
```

`--features custom-protocol` is **required**. Without it the binary serves `devUrl` and looks
for a dev server on `localhost:1420`; on a machine that is not running one, the window opens
and shows nothing. This was the Linux "Could not connect to localhost" failure.

| platform | artefacts |
|---|---|
| macOS | `Chaff.app`, `Chaff_<version>_<arch>.dmg` |
| Linux | `.deb`, `.rpm`, `.AppImage` |
| Windows | `.msi`, `.exe` (NSIS) |

Verified on macOS:

```
Chaff.app                      28.21 MiB
Chaff_0.1.0_aarch64.dmg        11.82 MiB
```

The DMG mounts and contains `Chaff.app`, checked with `hdiutil attach`. An earlier round of
this project shipped a **stale DMG** — the app was dragged from a bundle built seven minutes
before — which is why the bundle is mounted and inspected rather than assumed.

---

## What is signed

**Nothing, yet.** Every artefact is unsigned, and the sections below say what that costs and
what it would take.

This is not an oversight. Signing needs credentials that belong to the publisher:

| platform | what is needed | where it comes from |
|---|---|---|
| macOS | a **Developer ID Application** certificate | an Apple Developer account, $99/year |
| Windows | an **EV or OV code-signing** certificate | a CA, ~$200–400/year |
| Linux | nothing — `.deb`/`.rpm` are not signed by default | — |

Neither certificate can be created by anyone but their owner, which is why this is stated
rather than done.

### What the current build actually is

Measured on the bundle this repository produces:

```
$ codesign -dv target/release/bundle/macos/Chaff.app
CodeDirectory v=20400 flags=0x20002(adhoc,linker-signed)

$ spctl -a -vv target/release/bundle/macos/Chaff.app
code has no resources but signature indicates they must be present
```

**Ad-hoc signed** — which is what the Rust linker does by default on Apple silicon, not a
Developer ID signature. It is enough for the binary to run on the machine that built it and
**not** enough for Gatekeeper to accept it from anywhere else. `spctl` says so explicitly.

### What that means in practice

**macOS.** Gatekeeper blocks it when downloaded: *"Chaff cannot be opened because Apple cannot
check it for malicious software."* A locally built copy runs fine. The workaround is
right-click → Open, or `xattr -d com.apple.quarantine /Applications/Chaff.app`.

**Windows.** SmartScreen shows *"Windows protected your PC"* until the binary has reputation,
which an EV certificate buys immediately and an OV one accrues over time.

**Linux.** Nothing. Package managers do not require it.

---

## Adding signing

### macOS

```bash
export APPLE_CERTIFICATE="<base64 of the .p12>"
export APPLE_CERTIFICATE_PASSWORD="<password>"
export APPLE_SIGNING_IDENTITY="Developer ID Application: Your Name (TEAMID)"
export APPLE_ID="you@example.com"
export APPLE_PASSWORD="<app-specific password>"
export APPLE_TEAM_ID="TEAMID"
pnpm tauri build --features custom-protocol
```

Tauri signs and notarises when these are present, and skips both when they are not — which is
why the build works today and will keep working when they are added.

`APPLE_PASSWORD` must be an **app-specific password**, not the account password. An account
password fails with an authentication error that does not say why.

### Windows

```powershell
$env:TAURI_SIGNING_PRIVATE_KEY = "<the .pfx>"
$env:TAURI_SIGNING_PRIVATE_KEY_PASSWORD = "<password>"
pnpm tauri build --features custom-protocol
```

Or set `bundle.windows.certificateThumbprint` in `tauri.conf.json` to a certificate already
in the Windows certificate store.

---

## Verifying a build before shipping it

**A successful build is not a successful install.** This project has already shipped a stale
DMG once — the app was dragged from a bundle built seven minutes earlier.

```bash
./tools/mac/install.sh          # builds, installs, and verifies what landed
```

It hashes the installed binary, checks the **commit stamp is inside it**, and launches it.
`git rev-parse --short HEAD` must appear in the installed binary's strings.

On Linux:

```bash
distrobox enter chaff-build -- bash -lc 'cd ~/chaff && cargo build --release -p chaff --features custom-protocol'
bash tools/linux/perf.sh target/release/chaff
```

---

## The Linux container needs three things the host does not

1. `webkit2gtk4.1-devel` and friends — the webview.
2. **`gcc-c++`** — ONNX Runtime links `libstdc++`, and without it the link fails with
   `cannot find -lstdc++`, which reads as a Rust problem and is not one.
3. **`clang-devel`** — the vendored LibRaw generates its bindings with bindgen, which needs
   `libclang`. Without it the build stops with `Unable to find libclang`, and the message
   suggests `LIBCLANG_PATH` — which is the wrong fix, because there is no libclang to point
   at until the package is installed.

```bash
distrobox create --name chaff-build --image fedora:41
distrobox enter chaff-build -- sudo dnf install -y webkit2gtk4.1-devel gcc-c++ clang-devel
```

Each of these three was found by a **build that failed**, in that order, and each one reads
as a different kind of problem than it is.
