# Deploying to GitHub Pages

sshmux is a fully static site (`trunk build` output in `dist/`), so GitHub
Pages hosts it directly. Because Pages serves over https, the connect screen
can only reach `wss://` bridges (see `docs/BRIDGE.md`).

## One-time repo setup

1. Push the repo to GitHub (e.g. `ar4l/sshmux`).
2. Repo **Settings → Pages → Build and deployment → Source**: select
   **GitHub Actions**.

That's it — the included workflow does the rest on every push to `main`.

## CI workflow

`.github/workflows/deploy.yml` builds and deploys:

- installs stable Rust + the `wasm32-unknown-unknown` target and Trunk;
- overrides `CC_wasm32_unknown_unknown`/`AR_wasm32_unknown_unknown` to plain
  `clang`/`llvm-ar` (the Homebrew paths in `.cargo/config.toml` are
  macOS-only; ring's C sources need clang for the wasm target);
- runs `trunk build --release --public-url /sshmux/` — the `--public-url`
  must match the repo name because project Pages sites are served from
  `https://<user>.github.io/<repo>/`;
- uploads `dist/` with `upload-pages-artifact` and deploys with
  `deploy-pages`.

Manual runs: **Actions → deploy → Run workflow** (`workflow_dispatch`).

## Custom domain / base path (ssh.aral.cc)

Project Pages under a custom domain are served from the domain **root**, so
the base path changes:

1. **Namecheap DNS** (aral.cc): add a CNAME record

   | Type  | Host | Value            |
   |-------|------|------------------|
   | CNAME | ssh  | `ar4l.github.io.` |

2. **Repo Settings → Pages → Custom domain**: enter `ssh.aral.cc`, save, and
   wait for the DNS check; then tick **Enforce HTTPS** once the certificate
   is issued. (This writes a `CNAME` file into the Pages deployment; with the
   Actions flow, also add a `CNAME` file containing `ssh.aral.cc` to the
   deployed artifact — simplest is `<link data-trunk rel="copy-file"
   href="CNAME" />` in `index.html` with a root `CNAME` file.)

3. **Change the build's public URL** in `.github/workflows/deploy.yml` from
   `/sshmux/` to `/`:

   ```yaml
   - run: trunk build --release --public-url /
   ```

   Without this the app would request assets from `/sshmux/...` which does
   not exist under the custom domain (and vice versa: keep `/sshmux/` if you
   drop the custom domain).

The app is then live at `https://ssh.aral.cc`.
